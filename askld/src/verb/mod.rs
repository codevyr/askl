use crate::cfg::ControlFlowGraph;
use crate::diagnostic::Diagnostic;
use crate::execution_context::{selector_state_with, ExecutionContext, SelectorRegistry};
use crate::execution_state::{DependencyKind, DependencyRole};
use crate::parser::{Rule, Value};
use crate::parser_context::ParserContext;
use crate::relationship::Relationship;
use crate::span::Span;
use crate::statement::Statement;
use anyhow::Result;
use async_trait::async_trait;
use index::db_diesel::{AskSide, FamilyDemands, FamilyRead, ReferenceRead, Related};
use index::db_diesel::{
    CompositeFilter, DirectOnlyMixin, EphContext, EphScopedFut, EphTransaction, FoldedOnlyMixin,
    Index, OuterParentFilterMixin, RootLayer, RootShardRef, ScopeContext, Selection,
    SymbolInstanceIdMixin, TraversalMode,
};
use index::edge::{Edge, RefTest};

/// Root-shard populate callback used by [`LayerSpec`].  Called by the
/// statement-execution layer inside [`Index::with_eph_layer`] — once PER
/// VISIBLE ROOT (each root gets its own root shard holding only that
/// project's rows) and only when that root's layer is freshly inserted
/// (cache miss); on hit the
/// executor skips it.  `Fn`, not `FnOnce`: the same closure runs for every
/// root, so verbs capture their inputs in `Arc`/cheap clones and clone them
/// into each returned future (the HRTB means the future cannot borrow the
/// closure's environment).
///
/// The closure borrows mutably from the `EphTransaction` across `.await`
/// points; the boxed `Future` carries the inner borrow lifetime.
///
/// The returned `bool` is the layer's `truncated` flag: `true` means this
/// ROOT's contents were capped by a result limit (limits apply per project)
/// and the caller should surface a truncation warning.  Layers that don't
/// have a notion of truncation (e.g. `loc`, manual `layer { … }` blocks)
/// always return `false`.  The flag is persisted to `layers.truncated`
/// so cache hits propagate it without re-running the populate batch.
pub type RootShardPopulate =
    Box<dyn for<'b> Fn(&'b mut EphTransaction<'_>, &'b RootLayer) -> EphScopedFut<'b, bool>>;

/// Selection-shard populate callback used by [`LayerSpec`].  Same contract as
/// [`RootShardPopulate`], plus the identity of that root's already-committed
/// root shard so future mask/tombstone rows can reference what they mask.
/// The returned `bool` is the selection shard's own `truncated` flag; verbs
/// whose result limit fences the persistent scan return `false` here.
///
/// Mask-milestone caveat: under a multi-selector composite the executor
/// merges all parts into ONE partitioned entry per root, so every part's
/// selection-shard closure receives the COMPOSITE root shard's
/// `RootShardRef` — not the ref of the root shard the part's own verb would
/// have produced standalone.  Closures that start reading `RootShardRef` must
/// account for this.
pub type SelectionShardPopulate = Box<
    dyn for<'b> Fn(
        &'b mut EphTransaction<'_>,
        &'b RootLayer,
        RootShardRef,
    ) -> EphScopedFut<'b, bool>,
>;

/// Layer-shard populate callback for N-way content sharding.  Same contract as
/// [`SelectionShardPopulate`], plus the id of the ONE ephemeral content layer
/// `L_j` this shard covers — the executor invokes it once per visible eph
/// content layer (see [`Index::with_partitioned_layers`]) so the verb's
/// contribution `V(L_j)` caches independently on `(verb inputs, L_j)`.  A verb
/// exposes this by binding its layer-agnostic scan to `visible_layers=[L_j],
/// eph_branch=true`; `layer { … }` blocks (chain-dependent ops, not a
/// content scan) leave [`LayerSpec::layer_shard_populate`] `None`.
pub type LayerShardPopulate = Box<
    dyn for<'b> Fn(
        &'b mut EphTransaction<'_>,
        &'b RootLayer,
        i64,
        RootShardRef,
    ) -> EphScopedFut<'b, bool>,
>;

/// Layer-sharded content-scan callback passed by verbs to
/// [`LayerSpec::sharded_scan`].  The verb provides ONE layer-agnostic scan;
/// the executor invokes it per visible root (chains run in parallel) with a
/// `(visible_layers, eph_branch)` shard:
///   * the root shard `(vec![root.id], false)`, and
///   * one N-way layer shard `(vec![L_j], true)` per visible eph *content*
///     layer — so each layer's contribution `V(L_j)` caches independently on
///     `(verb inputs, L_j)` and is reused by any chain containing `L_j`.
///
/// The selection shard is a NO-OP for content scans: the layer shards already
/// cover every eph content layer, so a scanning selection shard would only
/// DOUBLE-COUNT.  It survives (empty) purely as the deterministic
/// chain-continuation marker (`tip`).  Results compose by union, valid
/// while masking-free.  Ephemeral layers hold no content today, so this is
/// byte-identical to the old 2-way path until content-in-layers lands.
///
/// Boxed AS an explicit `dyn for<'b> Fn` (like [`RootShardPopulate`]) so the HRTB
/// check is driven by the trait-object type rather than by closure inference —
/// a plain closure is not higher-ranked over the `txn`/`root` input lifetime
/// and cannot satisfy the bound.
pub type ShardedScan = Box<
    dyn for<'b> Fn(
        &'b mut EphTransaction<'_>,
        &'b RootLayer,
        Vec<i64>,
        bool,
    ) -> EphScopedFut<'b, bool>,
>;

/// Specification for an ephemeral cache entry that a [`Selector`] wants the
/// statement-execution layer to materialize before its query runs.
///
/// Every cached verb produces a root shard keyed only on its inputs and,
/// under a non-empty eph chain, a selection shard holding the eph-derived
/// delta (see [`Index::with_partitioned_layers`]).  Layer-creating selectors
/// (`search`, `loc`, `layer { … }`) implement [`Selector::layer_spec`]
/// returning `Some(LayerSpec)`.  The executor materializes the layers per
/// visible root, records them as `LayerActivation`s, folds them into its
/// tree's single `EphContext` materialisation, and then builds the `Selection`
/// from the layers' instances.  The selector itself does not own the
/// transaction, the layer ids, or any mutable interior state.
///
/// Carries no `parent_id`: chain topology is the executor's decision (it
/// reads the live eph context), which removes the parent-disagreement
/// failure mode entirely.
pub struct LayerSpec {
    /// The command's input hash H(c): keyed ONLY on verb inputs +
    /// composite-filter hash — never on the eph chain.  That independence is
    /// what lets the root shard survive any upstream ephemeral change.  (The
    /// executor additionally salts this per root — `root_shard_hash` — so each
    /// root's shard is keyed on that single root's identity, independent of
    /// co-visible projects.)
    pub input_hash: [u8; 32],
    /// Populates the root shard — the verb's scan bound to the root's own
    /// layer, unmasked (mask composition happens at read time, never here).
    /// Not a "persistent-only" contract: the verb is layer-agnostic; this shard
    /// is simply scoped to the root layer by the executor.
    pub root_shard_populate: RootShardPopulate,
    /// The eph-derived delta.  Provided unconditionally (uniform shape);
    /// the executor runs it iff an upstream chain exists.
    pub selection_shard_populate: SelectionShardPopulate,
    /// N-way content sharding: the verb's layer-shard contribution `V(L_j)`,
    /// invoked once per visible eph *content* layer.  `Some` for content
    /// scans (search, loc) that decompose as `V(⋃L)=⋃V(L)`; `None` for
    /// `layer { … }` blocks, whose selection shard is genuinely
    /// chain-dependent, not a layer-shard content scan.  Additive to the root
    /// and selection shards: today no eph layer carries content, so the
    /// executor materialises zero layer shards and production is
    /// byte-identical.  See [`Index::with_partitioned_layers`].
    pub layer_shard_populate: Option<LayerShardPopulate>,
    /// Hash fragment covering whatever the selection-shard populate reads
    /// BEYOND the chain and the root shard (folded into the selection-shard
    /// key by `index::db_diesel::selection_shard_hash`).  Empty when the delta
    /// is fully determined by (parent, root shard) — search, loc; the resolved
    /// eph-referencing ops for `layer { … }` blocks.
    pub selection_extra: Vec<u8>,
}

impl LayerSpec {
    /// A verb whose result is a content scan the executor SHARDS by layer:
    /// the root shard is the root-only shard (chain-independent); one layer
    /// shard per visible eph *content* layer (`vec![L_j]`, chain-independent
    /// key). The verb stays layer-agnostic — it provides one
    /// `scan(txn, root, visible_layers, eph_branch)`; the shard→visibility
    /// mapping lives HERE, not in the verb. The selection shard is a NO-OP:
    /// the layer shards carry every eph content layer, so a scanning selection
    /// shard would double-count; it survives empty only as the `tip`
    /// marker.  `selection_extra` is empty (a scan reads no extra key material
    /// beyond parent + root shard).
    pub fn sharded_scan(input_hash: [u8; 32], scan: ShardedScan) -> Self {
        let scan = std::sync::Arc::new(scan);
        let scan_root_shard = scan.clone();
        let scan_layer_shard = scan;
        let root_shard_populate: RootShardPopulate =
            Box::new(move |txn, root| scan_root_shard(txn, root, vec![root.id], false));
        // NO-OP selection shard (see the type-level and `sharded_scan` docs):
        // the layer shards below are the sole content holders, so scanning
        // here would double-count.  It survives empty as the chain marker.
        let selection_shard_populate: SelectionShardPopulate =
            Box::new(|_txn, _root, _root_shard_ref| Box::pin(async { Ok(false) }));
        // N-way: each layer shard scans exactly ONE eph content layer
        // `[layer_id]`.  Binding the same layer-agnostic scan to a singleton
        // visible set is what makes `V(L_j)` a stand-alone, layer-cacheable
        // unit.
        let layer_shard_populate: LayerShardPopulate =
            Box::new(move |txn, root, layer_id, _root_shard_ref| {
                scan_layer_shard(txn, root, vec![layer_id], true)
            });
        Self {
            input_hash,
            root_shard_populate,
            selection_shard_populate,
            layer_shard_populate: Some(layer_shard_populate),
            selection_extra: Vec::new(),
        }
    }
}
use index::symbols::SymbolInstanceId;
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::fmt::Display;
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;

mod args;
mod compound;
mod generic;
mod labels;
mod preamble;

pub(crate) use self::args::Args;
pub(crate) use self::compound::{build_compound_filter, Term};
pub(crate) use self::generic::{
    name_filter, typed_selector_text, typed_selectors_for, word_a_bare_name_cannot_replace,
    EphemeralOps, LabelResolutions,
};
pub use self::generic::{
    DefaultTypeFilter, GenericFilter, GenericSelector, NameSelector, UnitVerb,
};

use self::generic::{build_generic_verb, ForcedVerb, InheritVerb};
use self::labels::{LabelVerb, UserVerb};

/// Construct the verb a `Rule::verb` pair denotes, WITHOUT applying its
/// parse-context side effects (`update_context`) or adding it to the
/// command.  Shared by [`build_verb`] (which then applies both) and by
/// compound-expression atoms, where the suppression rule says only the
/// predicate contribution of a verb is read.
pub(super) fn construct_verb(
    ctx: &Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>, Error<Rule>> {
    let verb_span = Span::from_pest(pair.as_span(), ctx.source());
    debug!("Build verb {:#?}", pair);
    let verb = if let Some(verb) = pair.into_inner().next() {
        verb
    } else {
        return Err(Error::new_from_span(
            CustomError {
                message: format!("Expected a specific rule"),
            },
            verb_span.as_pest_span(),
        ));
    };

    let verb = if let Rule::generic_verb = verb.as_rule() {
        build_generic_verb(ctx.clone(), verb)?
    } else if let Rule::inherit_verb = verb.as_rule() {
        if ctx.get_eph_ops().is_some() {
            return Err(Error::new_from_span(
                CustomError {
                    message: "Only ephemeral verbs are allowed inside layer blocks".into(),
                },
                verb_span.as_pest_span(),
            ));
        }
        InheritVerb::build(verb_span.clone(), verb).map_err(|message| {
            Error::new_from_span(CustomError { message }, verb_span.as_pest_span())
        })?
    } else {
        if ctx.get_eph_ops().is_some() {
            return Err(Error::new_from_span(
                CustomError {
                    message: "Only ephemeral verbs are allowed inside layer blocks".into(),
                },
                verb_span.as_pest_span(),
            ));
        }
        match verb.as_rule() {
            Rule::label_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let args = Args::of(LabelVerb::NAME).with(Value::plain(label_ident.as_str()));
                LabelVerb::new(verb_span.clone(), &args)
            }
            Rule::inherit_label_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let args = Args::of(LabelVerb::NAME)
                    .with(Value::plain(label_ident.as_str()))
                    .with_named("inherit", Value::Bool(true));
                LabelVerb::new(verb_span.clone(), &args)
            }
            Rule::use_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let args = Args::of(UserVerb::NAME).with(Value::plain(label_ident.as_str()));
                UserVerb::new(verb_span.clone(), &args)
            }
            Rule::plain_filter => {
                let value = Value::build(verb.into_inner().next().unwrap())?;
                let args = Args::of(NameSelector::NAME).with_named("name", value);
                NameSelector::new(verb_span.clone(), &args)
            }
            Rule::forced_verb => {
                let value = Value::build(verb.into_inner().next().unwrap())?;
                let args = Args::of(ForcedVerb::NAME).with_named("name", value);
                ForcedVerb::new(verb_span.clone(), &args)
            }
            _ => {
                return Err(Error::new_from_span(
                    pest::error::ErrorVariant::ParsingError {
                        positives: vec![Rule::generic_verb, Rule::plain_filter, Rule::forced_verb],
                        negatives: vec![verb.as_rule()],
                    },
                    verb_span.as_pest_span(),
                ))
            }
        }
        .map_err(|e| {
            Error::new_from_span(
                CustomError {
                    message: format!("Failed to create filter: {}", e),
                },
                verb_span.as_pest_span(),
            )
        })?
    };

    Ok(verb)
}

/// Give a constructed verb to the statement being built: the context
/// consumes it (a relationship word, a preamble, a layer) or it joins the
/// statement's command.
pub(super) fn install_verb(
    ctx: &Rc<ParserContext>,
    verb: Arc<dyn Verb>,
    verb_span: &Span,
) -> Result<(), Error<Rule>> {
    let verb = ctx.consume(verb).map_err(|e| {
        Error::new_from_span(
            CustomError {
                message: format!("Failed to consume verb: {}", e),
            },
            verb_span.as_pest_span(),
        )
    })?;

    if let Some(verb) = verb {
        // A preamble configures the query: its verbs are redirected to the
        // global context and ANDed into every statement.  A term that selects
        // has nothing to select there, so it was silently swallowed —
        // `preamble project("p") "foo"` ran as though the name had never been
        // written.
        //
        // The test is the conjunction of both halves, and each excludes a
        // construct a preamble is FOR.  Anchoring alone would reject
        // `filter("compound_name", "test")`, a name filter that legitimately
        // scopes every statement in the query.  Being a selector alone would
        // reject `scope(...)`, `unnest` and `nested`, modifiers that are selectors but
        // never originate a row.  A verb that is both — `"foo"`, `func("foo")`,
        // `search(...)`, `loc(...)`, a layer literal, `select` — is a
        // statement's reason to exist, and a preamble is not a statement.
        // (`use(...)` belongs to that list by intent but is not an anchor; the
        // binding check below is what actually catches it.)
        if ctx.redirects_to_global() && verb.as_selector().is_some() && verb.anchor_kind().is_some()
        {
            return Err(Error::new_from_span(
                CustomError {
                    message: format!(
                        "`{}` selects, and a preamble cannot select: its verbs apply to \
                         every statement, so a selector here has nothing of its own to \
                         select.  Move it into a statement after the preamble \
                         (`preamble …; {} …`), or keep only constraints here — \
                         `project(...)`, `ignore(...)`, `filter(...)`, a bare type.",
                        verb_span.as_pest_span().as_str().trim(),
                        verb_span.as_pest_span().as_str().trim(),
                    ),
                },
                verb_span.as_pest_span(),
            ));
        }

        // The same law on the binding aspect.  `use` is a selector but not an
        // anchor, so the test above misses it; `label` is neither.  What rules
        // both out is the class: a label names ONE statement's result set, and
        // "every statement" is not one result set.  Left unchecked these were
        // swallowed exactly as the selectors above once were — `preamble #x`
        // ran as though it had never been written, undefined label and all.
        if ctx.redirects_to_global() && verb.class() == VerbClass::Binding {
            return Err(Error::new_from_span(
                CustomError {
                    message: format!(
                        "`{}` is label plumbing, and a preamble is not a statement: a \
                         label names one statement's result set, so there is nothing \
                         here to name or to read.  Move it into a statement after the \
                         preamble (`preamble …; {} …`), or keep only constraints here — \
                         `project(...)`, `ignore(...)`, `filter(...)`, a bare type.",
                        verb_span.as_pest_span().as_str().trim(),
                        verb_span.as_pest_span().as_str().trim(),
                    ),
                },
                verb_span.as_pest_span(),
            ));
        }

        ctx.extend_verb(verb)
    };

    Ok(())
}

pub fn derive_verb(verb: &Arc<dyn Verb>) -> Option<Arc<dyn Verb>> {
    match verb.derive_method() {
        DeriveMethod::Clone => Some(verb.clone()),
        DeriveMethod::Skip => None,
    }
}

pub enum DeriveMethod {
    Clone,
    Skip,
}

/// Per-command result limits set by the `limits()` verb.  Each field is
/// `None` when unset (inherit from parent / fall back to the server
/// default); resolved per field with last-wins in [`crate::command::Command::limits`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct Limits {
    /// Max distinct symbols (nodes) this command's selection ships.
    pub symbols: Option<usize>,
    /// Max rows in any single per-node listing (instances, out-edges,
    /// call-sites per pair).
    pub listing: Option<usize>,
}

/// Which aspect of a statement this verb contributes to.  A statement is a
/// record of aspects (see the filter-expressions design page): the predicate
/// is the only truth-valued one, relationship mode configures how a scope
/// relates to its children, bindings name and reference result sets, and env
/// verbs configure the query's execution environment.  The class is DECLARED
/// per verb and never inferred from role probing — [`crate::command::Command`]
/// partitions its verbs by it, and the boolean-operator work keys its
/// admission rules on it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum VerbClass {
    /// Filters and anchors — the truth-valued aspect.  This includes the
    /// runtime shape of `layer { … }` (an anchor branch denoting the layer's
    /// rows; the block's ephemeral-ops machinery is parse-context state, not
    /// a command verb) and the `scope(...)` stub (a selector operationally,
    /// slated for removal with the filter-expressions work).
    Predicate,
    /// Relationship-mode modifiers: `has`, `refs`, `derive` (consumed at
    /// parse time) and the traversal markers `unnest` / `nested` (read via
    /// [`Verb::traversal_mode`]).
    Relationship,
    /// Label plumbing: `label`/`@x` define, `use`/`#x` consume.  `use` is
    /// operationally a selector (it holds execution state), which is why
    /// selector iteration spans this class too.
    Binding,
    /// Query-environment configuration: `preamble` (consumed at parse time).
    Env,
}

/// The dimension a predicate verb constrains — the slot key of the
/// filter-expressions design.  Within a statement (and across a scope
/// boundary via inheritance), writing a dimension REPLACES the previous
/// holder of that dimension wholesale; verbs that hold no dimension
/// (anchors of every kind, `ignore`, `search`, `loc`, layer literals, units)
/// accumulate.
/// This is the entire conflict-resolution law: no verb knows about any
/// other verb.
///
/// Note the deliberate split from [`Verb::suppresses_default_type_filter`]:
/// that method answers the parse-time question "should the scope's default
/// type filter be injected?", which `search`/`loc`/`layer` answer yes to
/// without occupying the type dimension — a later type verb must not evict
/// them.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Dimension {
    /// `project(...)`.
    Project,
    /// `select`.
    Select,
    /// Type words (`func`, `data("x")`, `any`) and `filter("type", ...)` —
    /// one dimension.  The filters among them hold it and replace each
    /// other; a named selector writes it without holding it (see
    /// [`Verb::writes`]).
    Type,
    /// `filter("exact_name", ...)`.
    FilterExactName,
    /// `filter("compound_name", ...)`.
    FilterCompoundName,
    /// `package(...)` — the positive form of `ignore(package=...)`.
    Package,
}

/// Bundles the notification parameters that always travel together through
/// the notification chain: dependency role, resolved relationship type, and
/// how far into the containment tree the receiver's scope reaches.
///
/// `Copy` dropped along with `DependencyRole::PreSeedLabel(Rc<str>)` —
/// the label string can't be a static type.  Most call sites passed
/// this by value; they now call `.clone()` (cheap: one `Rc` bump for
/// the label case, just an enum copy for the others).
#[derive(Debug, Clone)]
pub struct NotificationContext {
    pub role: DependencyRole,
    pub rel_type: Edge,
    pub mode: TraversalMode,
    /// The receiving statement is an echo (`{ }`): it shows what the
    /// notifier's rows relate to and originates nothing of its own.
    pub receiver_is_echo: bool,
    /// What a probe found for the receiving statement, when one resolved it
    /// (`ExecutionContext::probe_resolved`): every instance it can have.  A
    /// statement with no predicate of its own is derived from a neighbour,
    /// and derives WITHIN this, so it is bounded by what it is and not by
    /// how much one neighbour reaches.
    pub within: Option<Vec<i64>>,
}

/// What kind of anchor a verb contributes (see [`Verb::anchor_kind`]).
/// Anchors are the predicates selective enough *in principle* to drive
/// execution: a name pattern, a content match, a source location, or an
/// explicit layer literal.  Whether an anchored statement actually drives
/// is a cost decision made at execution time, not here.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AnchorKind {
    /// A name predicate: `"foo"`, `g"foo*"`, `!"foo"`, `func("foo")`,
    /// `filter("exact_name"/"compound_name", ...)`.
    Name,
    /// A content predicate: `search(...)`.
    Content,
    /// A source location: `loc(...)`.
    Location,
    /// An explicit `layer { ... }` literal.
    LayerLiteral,
    /// The explicit `all` verb: an always-true anchor.  The user opted
    /// into a broad, budget-bounded enumeration.
    All,
}

pub trait Verb: std::fmt::Debug + Send + Sync {
    fn name(&self) -> &str;

    /// The aspect this verb contributes to.  Required — every verb declares
    /// its class explicitly; nothing derives it from which role casts
    /// succeed.
    fn class(&self) -> VerbClass;

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    /// Create a new instance of this verb for derivation into child scopes.
    fn derive_new_instance(&self) -> Option<Arc<dyn Verb>> {
        None
    }

    /// This verb, as `inherit(...)` on its statement makes it: the same
    /// predicate, derived into the scopes below.  `None` for a verb that
    /// cannot be carried, which then stays as it is.
    fn carried_below(&self) -> Option<Arc<dyn Verb>> {
        None
    }

    /// The dimension this verb constrains, if it is a slot-holding predicate
    /// verb (see [`Dimension`]).  `None` = accumulate.
    fn dimension(&self) -> Option<Dimension> {
        None
    }

    /// Every dimension this verb positively constrains.  Single verbs hold
    /// at most one slot; compound expressions can mention several (each of
    /// which they evict on write, and any of which gets them evicted by a
    /// later write).
    fn dimensions(&self) -> Vec<Dimension> {
        self.dimension().into_iter().collect()
    }

    /// The dimensions this verb REPLACES: writing one evicts whoever holds
    /// it.  For a filter that is the slot it holds itself.  A branch — a
    /// named type selector, an anchored expression — holds no slot, since it
    /// selects rather than constrains, yet it still says a type, and what it
    /// says replaces a type the statement inherited.
    fn writes(&self) -> Vec<Dimension> {
        self.dimensions()
    }

    /// A type word: `func`, `data("x")`, `any`, or an expression built from
    /// them.  Two of them in one statement where the later replaces the
    /// earlier is reported rather than obeyed (see `Command::said_twice`);
    /// the explicit `filter(...)`, `project(...)` and `package(...)` forms
    /// keep replacing in place, which is what they are documented to do.
    fn is_type_word(&self) -> bool {
        false
    }

    /// Whether this verb may appear as an atom inside a compound expression
    /// (`or`/`and`/`not`/parens), and if not, why.  The gate of the
    /// filter-expressions design: an atom must denote ONE concrete predicate
    /// — so verbs whose emission is not a single predicate query (`search`,
    /// `loc`, layer literals, forced selection) and verbs that constrain
    /// nothing (bare `any`, `select`, units) stay out.  `Ok` is a structural,
    /// eph-independent guarantee that the atom compiles to `Some` filter;
    /// [`crate::verb::compound`] relies on it.
    ///
    /// The rejection text lives HERE, next to the flag that produces it,
    /// rather than in a table keyed on display names.  The default rejects
    /// with a reason derived from the verb's class.
    fn compound_admission(&self) -> Result<(), String> {
        Err(match self.class() {
            VerbClass::Relationship => format!(
                "`{}` is a relationship verb and cannot appear in a filter \
                 expression; write it outside the expression",
                self.name()
            ),
            VerbClass::Binding => format!(
                "`{}` is a label verb and cannot appear in a filter expression",
                self.name()
            ),
            VerbClass::Env => format!(
                "`{}` configures the query environment and cannot appear in a \
                 filter expression",
                self.name()
            ),
            VerbClass::Predicate => {
                format!("`{}` cannot appear in a filter expression", self.name())
            }
        })
    }

    /// What this verb asks of the edge between its statement and the
    /// statements in its braces, if it is a relationship word (`has`,
    /// `refs`, `declares`, `holds`, `calls`, `writes`, `takes`, `reads`) or
    /// an expression of them.  The other relationship-class verbs are not
    /// words of that kind: `derive(...)` takes arguments and is not part of
    /// an expression, `unnest` and `nested` say how far a scope reaches.
    fn relationship(&self) -> Option<&Relationship> {
        None
    }

    fn update_context(&self, _ctx: &ParserContext) -> Result<bool> {
        Ok(false)
    }

    /// The traversal mode this verb marks, if it is one of the markers.
    /// `unnest` reaches every descendant, `nested` exactly one structural
    /// level; everything else leaves the scope at its default (unfolded).
    fn traversal_mode(&self) -> Option<TraversalMode> {
        None
    }

    fn is_unit(&self) -> bool {
        false
    }

    fn is_non_constraining_selector(&self) -> bool {
        self.is_unit()
    }

    fn span(&self) -> pest::Span<'_> {
        panic!("Verb does not have a span")
    }

    /// The bare typed name for a plain-name selector (e.g. `vfs_rea`), used to
    /// offer "did you mean?" suggestions when it matches nothing. `None` for
    /// globs and non-name verbs — suggestions don't apply there.
    fn matched_token(&self) -> Option<String> {
        None
    }

    /// This verb's selector contribution, if it has one.  A declared role,
    /// not an error path: `None` simply means "no selector aspect".  May
    /// depend on instance state (a bare inherited type selector is
    /// filter-only).
    fn as_selector<'a>(&'a self) -> Option<&'a dyn Selector> {
        None
    }

    /// This verb's filter contribution, if it has one.
    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        None
    }

    /// This verb's label-definition contribution, if it has one.
    fn as_labeler<'a>(&'a self) -> Option<&'a dyn Labeler> {
        None
    }

    /// The result limits this verb carries, if it is a `limits()` verb.
    fn as_limits(&self) -> Option<Limits> {
        None
    }

    fn suppresses_default_type_filter(&self) -> bool {
        false
    }

    /// The anchor this verb contributes, if any.  A statement carrying at
    /// least one anchored verb is *eligible to drive execution* (it denotes
    /// a set narrow enough to be worth materialising on its own); pure
    /// constraints — type predicates, `project(...)`, `preamble` — never
    /// drive and are derived from their neighbours instead.
    fn anchor_kind(&self) -> Option<AnchorKind> {
        None
    }

    /// Whether this verb constrains by NAME.  Selectors answer this through
    /// [`Verb::is_non_constraining_selector`]; the predicate exists for the
    /// verbs a selector scan cannot see — today `filter("exact_name"/
    /// "compound_name", …)`, which is a *filter* yet names a set as tightly
    /// as `"foo"` does.  It is the same condition
    /// [`Verb::anchor_kind`] reports as [`AnchorKind::Name`], which is what
    /// makes `anchored ⇒ not weak` hold by construction (see
    /// [`crate::command::Command::is_non_constraining`]).
    ///
    /// Type predicates are structural, not naming: bare `func` and
    /// `filter("type", …)` stay `false`, so they remain weakness candidates
    /// and go on deriving their selection from their neighbours.
    fn has_name_constraint(&self) -> bool {
        false
    }
}

/// Filter trait for verbs that constrain symbol selection.
///
/// A filter contributes ONE thing: a `CompositeFilter` tree compiled into a
/// SQL WHERE clause.  It receives the request's `EphContext` so filters whose
/// SQL needs the visibility chain (today: only the direct-only mixin) can bind
/// it; impls that don't need it ignore the parameter.
///
/// There is deliberately no in-memory post-filter hook.  The trait used to
/// carry one for constraints "SQL cannot express"; nothing ever implemented
/// it, and every call site was a no-op walking the filter list.  All filtering
/// happens in SQL, and this trait's shape now says so.
pub trait Filter: std::fmt::Debug + Display + Verb {
    fn get_composite_filter(&self, _eph: &EphContext) -> Option<CompositeFilter> {
        None
    }
}

#[derive(Debug)]
pub struct SelectorState {
    /// Behind an `Rc` so handing the selection to a consumer is a refcount
    /// bump, not a deep copy.  A wide statement's neighbourhood families run
    /// to six figures of rows, and the final graph-assembly phase asks for
    /// the selection once per consumer while each consumer reads ONE field.
    /// Mutation goes through `Rc::make_mut`, so the sole owner (the normal
    /// case during execution) still mutates in place.
    pub selection: Option<Rc<Selection>>,
}

impl SelectorState {
    pub fn new() -> Self {
        Self { selection: None }
    }

    /// `Rc::make_mut` on the stored selection, with the copy made visible.
    ///
    /// Copy-on-write: the sole owner mutates in place, which is the normal
    /// case and the reason the `Rc` pays off.  But a handle held elsewhere —
    /// `derive_from_parent` keeps the parent's across two awaits, and
    /// `Statement::notify` holds every participant's for a constrain loop —
    /// makes this deep-copy the whole selection.  That is still correct, and
    /// never costs more than the unconditional copy this replaced, but it IS
    /// the cost the `Rc` exists to avoid, so it gets traced instead of
    /// silently reappearing under a future concurrency change.
    fn selection_mut(&mut self) -> &mut Selection {
        let rc = self.selection.as_mut().unwrap();
        if Rc::strong_count(rc) > 1 {
            tracing::debug!(
                nodes = rc.nodes.len(),
                holders = Rc::strong_count(rc),
                "selection copy-on-write: a handle was outstanding",
            );
        }
        Rc::make_mut(rc)
    }

    /// Replace this selector's selection and report whether it actually
    /// CHANGED — by node set, the only thing a neighbour constrains against.
    ///
    /// The worklist reschedules a neighbour when a notifier's selection
    /// changed; a derive that reproduces the same nodes (a capped statement
    /// re-deriving the same capped set each round is the case that matters)
    /// must not reschedule, or the worklist churns — re-running a full
    /// `find_symbol` per round for no new information.  Comparing node sets is
    /// enough: families are derived from nodes, so equal nodes ⇒ equal
    /// constraint.  Both sets are ≤ `read_cap` since M1, so the sort is cheap.
    pub fn set_selection_reporting_change(&mut self, new: Option<Selection>) -> bool {
        let key = |s: &Selection| {
            let mut v = s.get_instance_ids();
            v.sort_unstable();
            v
        };
        let before = self.selection.as_deref().map(key);
        let after = new.as_ref().map(key);
        self.selection = new.map(Rc::new);
        before != after
    }

    pub fn constrain_selection(
        &mut self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: Edge,
    ) -> bool {
        self.narrow(dependency, role, rel_type, false, None).changed
    }

    /// [`Self::constrain_selection`], saying "incomplete" when the statement
    /// was whole and comes out a subset.  For the callers that narrow without
    /// the "none of them relate" note of [`Self::constrain_with_warning`].
    pub(crate) fn constrain_saying_cut(
        &mut self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: Edge,
        span: Span,
        for_echo: bool,
    ) -> (bool, Vec<Diagnostic>) {
        let Narrowed { changed, newly_cut } =
            self.narrow(dependency, role, rel_type, for_echo, None);
        let warnings = if newly_cut {
            vec![Diagnostic::incomplete(span)]
        } else {
            vec![]
        };
        (changed, warnings)
    }

    /// Narrow this selection by a neighbour's.
    ///
    /// EVERYTHING A STATEMENT SHOWS HAS EVIDENCE WITH EVERY NEIGHBOUR THAT IS
    /// NOT CUT.  The retain keeps exactly the rows some family ties to the
    /// neighbour, read from either side.  A row a family holds is a true row
    /// whether or not that family was read to its end, so keeping needs
    /// nothing of the reads.  Dropping does: see `absence_is_proven`.
    ///
    ///  * A cut DEPENDENCY does not narrow.  It is a subset of what its
    ///    statement means, so a row that relates only to the part never read
    ///    would be dropped from a statement that is otherwise whole.  The one
    ///    exception is an echo (`for_echo`): it shows what its parent's SHOWN
    ///    rows relate to and has nothing of its own to lose, so it follows the
    ///    parent down however that came to be narrowed.
    ///  * A cut SELF is narrowed like any other.  It used not to be -- "a
    ///    capped selection neither narrows nor is narrowed" -- on the grounds
    ///    that the retain would then report "matched nothing" from data never
    ///    fully read.  That is a reason not to SAY so, and nothing is said
    ///    (`constrain_with_warning`); it is no reason to keep the rows.  Kept,
    ///    they were an unrelated sample: `mod("amdgpu") { { "spinlock_t" } }`
    ///    listed 687 users of `spinlock_t` from all over the kernel, none of
    ///    them in amdgpu, under "incomplete".
    ///
    /// Returning unchanged means "nothing changed", NOT "derive instead" --
    /// `constrain_with_warning`'s first element is `selection.is_some()`,
    /// decided before this call.
    fn narrow(
        &mut self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: Edge,
        for_echo: bool,
        asked: Option<&Related>,
    ) -> Narrowed {
        let unchanged = Narrowed {
            changed: false,
            newly_cut: false,
        };
        if dependency.capped && !for_echo {
            return unchanged;
        }
        let Some(own) = self.selection.as_deref() else {
            return unchanged;
        };

        let len_before = own.nodes.len();
        let was_cut = own.capped;
        // The lists prove the absence, or the database was asked and knew
        // (`Self::unproven` asks about exactly the kinds the lists leave open).
        let proven = absence_is_proven(own, dependency, role, rel_type)
            || asked.is_some_and(|asked| asked.whole);
        let Some(mut evidenced) = evidenced(own, dependency, role, rel_type) else {
            return unchanged;
        };
        // What an ask names is a true row whether or not it knew them all.
        if let Some(asked) = asked {
            evidenced.extend(asked.ids.iter().copied());
        }
        self.selection_mut()
            .nodes
            .retain(|s| evidenced.contains(&s.symbol_instance.id));

        let changed = self.selection.as_ref().unwrap().nodes.len() != len_before;
        // Rows went for want of evidence that no read ran far enough to rule
        // out, or because the neighbour is itself cut: some of them may
        // belong.  What is left is a subset, and says so.
        let newly_cut = changed && !was_cut && (!proven || dependency.capped);
        if newly_cut {
            self.selection_mut().mark_cut();
        }
        Narrowed { changed, newly_cut }
    }

    /// What narrowing by `dependency` would drop without proof: the rows no
    /// list ties to it, and the kinds of relation whose lists were not read
    /// to their end.  `None` when there is nothing to ask: nothing would be
    /// dropped, the lists settle it, or asking cannot help (a cut neighbour
    /// narrows nobody; a statement that is already a subset stays one).
    ///
    /// WHEN THE LISTS CANNOT PROVE AN ABSENCE, THE ENGINE ASKS WHICH ROWS
    /// RELATE.  The caller puts the question to the index
    /// ([`ask_which_relate`]) and hands the answer to the narrowing.
    pub(crate) fn unproven(
        &self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: Edge,
    ) -> Option<Unproven> {
        let own = self.selection.as_deref()?;
        if dependency.capped || own.capped {
            return None;
        }
        let (refs_proven, has_proven) = absence_by_kind(own, dependency, role, rel_type)?;
        // What of the edge the lists leave open: nothing, and there is
        // nothing to ask.
        let open = Edge::of(
            rel_type.has() && !has_proven,
            rel_type.refs().filter(|_| !refs_proven),
        )?;
        let evidenced = evidenced(own, dependency, role, rel_type)?;
        let lacking: Vec<i64> = own
            .nodes
            .iter()
            .map(|n| n.symbol_instance.id)
            .filter(|id| !evidenced.contains(id))
            .collect();
        if lacking.is_empty() {
            return None;
        }
        Some(Unproven {
            edge: open,
            lacking,
        })
    }

    /// Constrain selection and produce a warning if the result is empty.
    /// Returns `(constrained, changed, warnings)` where `constrained` means a selection
    /// existed to constrain, `changed` means it was actually narrowed.
    ///
    /// The warning is raised once, where the emptiness starts: a neighbour
    /// with matches emptied a selection that had them.  An empty `dependency`
    /// only passes on an emptiness that was reported upstream, so it stays
    /// silent.  `span` locates this selector; `own` and `neighbour` name the
    /// two statements by their command text (a bare `func` has an implicit
    /// selector spanning its whole scope), and `conjoined` marks the
    /// child→parent edge, where siblings narrow the parent together.
    pub fn constrain_with_warning(
        &mut self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: Edge,
        span: Span,
        own: &Span,
        neighbour: &Span,
        conjoined: bool,
        for_echo: bool,
        asked: Option<&Related>,
    ) -> (bool, bool, Vec<Diagnostic>) {
        if self.selection.is_none() {
            return (false, false, vec![]);
        }
        let Narrowed { changed, newly_cut } =
            self.narrow(dependency, role, rel_type, for_echo, asked);
        let mut warnings = vec![];
        if newly_cut {
            warnings.push(Diagnostic::incomplete(span.clone()));
        }
        // "None of them relate" is a claim about every row the statement
        // means.  A cut statement held only some of them, so it cannot make
        // it; it is already said to be incomplete.
        if changed
            && !dependency.nodes.is_empty()
            && self.selection.as_ref().unwrap().nodes.is_empty()
            && !self.selection.as_ref().unwrap().cut()
        {
            warnings.push(Diagnostic::note(
                span,
                format!(
                    "`{}` matched symbols, but none of them {}relate to `{}`",
                    own.as_str().trim(),
                    if conjoined { "also " } else { "" },
                    neighbour.as_str().trim(),
                ),
            ));
        }
        (true, changed, warnings)
    }
}

/// The rows of `own` that some list ties to `dependency`, by instance id.
/// `None` for the roles that carry no constraint.
fn evidenced(
    own: &Selection,
    dependency: &Selection,
    role: &DependencyRole,
    rel_type: Edge,
) -> Option<std::collections::HashSet<i64>> {
    match role {
        DependencyRole::Parent => Some(evidenced_by_child(own, dependency, rel_type)),
        DependencyRole::Child => Some(evidenced_by_parent(own, dependency, rel_type)),
        // Membership in the owner's rows.
        DependencyRole::User => Some(
            dependency
                .nodes
                .iter()
                .map(|o| o.symbol_instance.id)
                .collect(),
        ),
        // PreSeed* edges carry no constraint data — pure ordering (with
        // optional label-resolution as a side-effect at `compute_roots` time,
        // not here).
        DependencyRole::PreSeedSibling | DependencyRole::PreSeedLabel(_) => None,
    }
}

/// The rows of a child that its parent's rows refer to or hold.
///
/// THE SOURCE OF A REFERENCE IS THE INSTANCE THAT ENCLOSES IT.  A reference
/// is written in one place, so it is evidence for the instance around that
/// place and for no other instance of the same symbol, and only while that
/// instance is still one of the parent's rows.  The TARGET stays a symbol: a
/// reference names a symbol, not one of its instances, so every instance of
/// what is referred to is kept.
///
/// Kept per symbol, "some `main` calls it" stood for every `main`.  C has one
/// symbol per name, so `file("…/ucx_profiling.c") { func("main")
/// { "print_version" } }` returned `print_version` on the strength of another
/// program's `main`, with no edge to show for it.
fn evidenced_by_parent(
    selection: &Selection,
    parent: &Selection,
    rel_type: Edge,
) -> std::collections::HashSet<i64> {
    let check_has = rel_type.has();
    let parent_instance_ids: std::collections::HashSet<_> =
        parent.nodes.iter().map(|n| n.symbol_instance.id).collect();
    // A relationship is accepted on evidence from EITHER side's edge lists.
    // The parent's children/has_children are computed when the parent's
    // statement runs — BEFORE a layer-creating child (e.g. `file("x")
    // { search("y") }`) materialises its ephemeral rows — so they cannot name
    // those rows.  This selection's own parents/has_parents were computed
    // with the layer visible and do.  The union is safe: both sides express
    // the same predicate, one may just be missing edges to rows that did not
    // exist yet.
    let mut ref_evidence: std::collections::HashSet<i64> = Default::default();
    let mut has_evidence: std::collections::HashSet<i64> = Default::default();
    // A reference is evidence when the edge's test admits it: a family may
    // have been read for a wider test than this edge asks (two children of
    // one parent, each with its own).
    if let Some(test) = rel_type.refs() {
        ref_evidence.extend(
            selection
                .parents
                .iter()
                .filter(|r| admitted(test, &r.symbol_ref))
                .filter(|r| parent_instance_ids.contains(&r.from_instance.id))
                .map(|r| r.to_symbol.id),
        );
        ref_evidence.extend(
            parent
                .children
                .iter()
                .filter(|r| admitted(test, &r.symbol_ref))
                .filter(|r| parent_instance_ids.contains(&r.from_instance.id))
                .map(|r| r.symbol.id),
        );
    }
    if check_has {
        has_evidence.extend(
            selection
                .has_parents
                .iter()
                .filter(|r| parent_instance_ids.contains(&r.parent_instance.id))
                .map(|r| r.child_instance.id),
        );
        has_evidence.extend(
            parent
                .has_children
                .iter()
                .filter(|r| parent_instance_ids.contains(&r.parent_instance.id))
                .map(|r| r.child_instance.id),
        );
    }
    selection
        .nodes
        .iter()
        .filter(|s| {
            ref_evidence.contains(&s.symbol.id) || has_evidence.contains(&s.symbol_instance.id)
        })
        .map(|s| s.symbol_instance.id)
        .collect()
}

/// The rows of a parent that refer to, or hold, one of its child's rows: a
/// parent row counts when ITS instance encloses a reference to one of the
/// child's symbols (see `evidenced_by_parent` for the law), or holds one of
/// its instances.
fn evidenced_by_child(
    selection: &Selection,
    child: &Selection,
    rel_type: Edge,
) -> std::collections::HashSet<i64> {
    let check_has = rel_type.has();
    let child_symbol_ids: std::collections::HashSet<_> =
        child.nodes.iter().map(|n| n.symbol.id).collect();
    let child_instance_ids: std::collections::HashSet<_> =
        child.nodes.iter().map(|n| n.symbol_instance.id).collect();
    // Same evidence-union as `evidenced_by_parent`, mirrored: a relationship
    // is accepted from EITHER side's edge lists.  With two-phase execution
    // both sides are computed under the same tree-complete visibility, so
    // this is uniformity (one side may have deferred its neighbourhood
    // query), not a freshness patch.
    let mut evidence: std::collections::HashSet<i64> = Default::default();
    if let Some(test) = rel_type.refs() {
        evidence.extend(
            selection
                .children
                .iter()
                .filter(|r| admitted(test, &r.symbol_ref))
                .filter(|r| child_symbol_ids.contains(&r.symbol.id))
                .map(|r| r.from_instance.id),
        );
        evidence.extend(
            child
                .parents
                .iter()
                .filter(|r| admitted(test, &r.symbol_ref))
                .filter(|r| child_symbol_ids.contains(&r.to_symbol.id))
                .map(|r| r.from_instance.id),
        );
    }
    if check_has {
        evidence.extend(
            selection
                .has_children
                .iter()
                .filter(|r| child_instance_ids.contains(&r.child_instance.id))
                .map(|r| r.parent_instance.id),
        );
        evidence.extend(
            child
                .has_parents
                .iter()
                .filter(|r| child_instance_ids.contains(&r.child_instance.id))
                .map(|r| r.parent_instance.id),
        );
    }
    selection
        .nodes
        .iter()
        .map(|s| s.symbol_instance.id)
        .filter(|id| evidence.contains(id))
        .collect()
}

/// The reference row `reference` passes `test`: the in-memory reading of the
/// test the reads put in SQL (`RefTest::admits`).
fn admitted(test: RefTest, reference: &index::models_diesel::SymbolRef) -> bool {
    test.admits(reference.roles, reference.relation)
}

/// What the lists between two statements leave open; see
/// [`SelectorState::unproven`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Unproven {
    /// The part of the edge no list settles: its reference test when the
    /// reference lists were not read to their end (or were read for
    /// something narrower), containment when the containment lists were not.
    pub edge: Edge,
    /// The rows no list ties to the neighbour.
    pub lacking: Vec<i64>,
}

/// Put what the lists leave open to the index: which of the rows that lack
/// evidence relate to the neighbour's rows.
pub(crate) async fn ask_which_relate(
    index: &Index,
    unproven: &Unproven,
    dependency: &Selection,
    role: &DependencyRole,
    eph: &EphContext,
) -> Result<Related> {
    let neighbour = dependency.get_instance_ids();
    let (parents, children, want) = match role {
        // We are the parent: which of our rows refer to, or hold, a row of
        // the child.
        DependencyRole::Parent => (&unproven.lacking, &neighbour, AskSide::Parents),
        _ => (&neighbour, &unproven.lacking, AskSide::Children),
    };
    index
        .which_relate(parents, children, want, unproven.edge, eph)
        .await
}

/// What [`SelectorState::narrow`] did.
struct Narrowed {
    /// Rows were dropped.
    changed: bool,
    /// The statement was whole before and is a subset now.
    newly_cut: bool,
}

/// Whether a row of `own` that no family ties to `dependency` is thereby shown
/// not to relate to it.
///
/// A LIMIT THAT CUT A READ DISQUALIFIES THAT READ AND NOTHING ELSE.  For each
/// relation kind the edge accepts, the two ends each hold one family about it
/// (`constrain_by_*` takes evidence from either).  Silence in a family proves
/// absence only if that family was read to its end over a scope that held the
/// other statement's rows.  One such family per kind is enough; with none, a
/// dropped row may have had its evidence in the part that was not read:
///
///  * a family nobody read (`Deferred`, not demanded) is empty and says
///    nothing -- it is not "read, and found none";
///  * a family read against another neighbour's rows says nothing about this
///    one (`X { "a" ; { } }`: X's references are read within `"a"`);
///  * a bare scope accepts a reference OR containment, so references read in
///    full cannot rule a row out while containment was cut;
///  * a reference family read under a test says nothing about a reference
///    the test left out (`ReferenceRead`): a list of calls cannot rule out a
///    write.
pub(crate) fn absence_is_proven(
    own: &Selection,
    dependency: &Selection,
    role: &DependencyRole,
    rel_type: Edge,
) -> bool {
    let Some((refs, has)) = absence_by_kind(own, dependency, role, rel_type) else {
        // Membership in the owner's rows: no family is consulted.
        return true;
    };
    (rel_type.refs().is_none() || refs) && (!rel_type.has() || has)
}

/// [`absence_is_proven`] kind by kind: whether silence about a reference
/// that `rel_type` admits, and about containment, proves there is none.
/// `None` for the roles that consult no family.  The reference half is
/// vacuously true for an edge that accepts no reference; its callers read it
/// only for one that does.
fn absence_by_kind(
    own: &Selection,
    dependency: &Selection,
    role: &DependencyRole,
    rel_type: Edge,
) -> Option<(bool, bool)> {
    let own_ids = own.get_instance_ids();
    let dependency_ids = dependency.get_instance_ids();
    let either = |ours: &FamilyRead, theirs: &FamilyRead| {
        ours.proves_absence_for(&dependency_ids) || theirs.proves_absence_for(&own_ids)
    };
    let either_reference = |ours: &ReferenceRead, theirs: &ReferenceRead| {
        rel_type.refs().is_none_or(|test| {
            ours.proves_absence_for(&dependency_ids, test)
                || theirs.proves_absence_for(&own_ids, test)
        })
    };
    match role {
        // We are the parent: our children families, the child's parents ones.
        DependencyRole::Parent => Some((
            either_reference(&own.families.children, &dependency.families.parents),
            either(&own.families.has_children, &dependency.families.has_parents),
        )),
        // We are the child.
        DependencyRole::Child => Some((
            either_reference(&own.families.parents, &dependency.families.children),
            either(&own.families.has_parents, &dependency.families.has_children),
        )),
        DependencyRole::User | DependencyRole::PreSeedSibling | DependencyRole::PreSeedLabel(_) => {
            None
        }
    }
}

pub type SelectorId = usize;

/// The weakness rule, in one place.
///
/// A weak substatement is a display echo, so it must never *narrow* a
/// neighbour that has already resolved — but it may still *supply* a
/// selection to one that has none.  That second half is not a loophole: it
/// is how a chain propagates through weak intermediaries (`{{"a"}}`, or
/// `data(inherit=true) {{{"channels_a"}}}`), where every middle scope is
/// weak and the only data comes from the ends.
///
/// Both notification edges ask this question and must answer it the same
/// way: the parent→child edge in [`Selector::try_constrain_notification`],
/// and the child→parent edge in `Command::notify_from_selection`.  They
/// disagreed until the child→parent edge got this call, which let an empty
/// weak child empty its parent while an empty weak parent left its child
/// alone.
pub fn weak_notifier_blocks(notifier_weak: bool, receiver_has_selection: bool) -> bool {
    notifier_weak && receiver_has_selection
}

/// Result of per-selector constraint logic in `try_constrain_notification`.
pub enum ConstraintAction {
    /// Selector should be skipped (e.g., weak statement already has selection).
    Skip,
    /// Selection was constrained. Contains (changed, warnings).
    Constrained(bool, Vec<Diagnostic>),
    /// No existing selection to constrain; proceed to derive.
    Derive,
}

/// A selector's own predicate contribution BEYOND the command's filters —
/// the OR-branch it contributes to the statement's predicate.
pub enum OwnPredicate {
    /// Rows = command filters ∧ this predicate (`None` = no extra
    /// constraint: the selector's criteria already live in the filter
    /// conjunction, e.g. a `TypeSelector` contributing via `as_filter`).
    Filter(Option<CompositeFilter>),
    /// Not expressible as a SQL predicate (labels, units, layer reads).
    Opaque,
}

#[async_trait(?Send)]
pub trait Selector: std::fmt::Debug + Verb {
    fn id(&self) -> SelectorId {
        ptr::from_ref(self) as *const () as SelectorId
    }

    fn get_label(&self) -> Option<String> {
        None
    }

    /// This selector's own predicate beyond the command's filters.  Keep
    /// consistent with [`Selector::build_composite_filter`]: expressible
    /// selectors must derive both from the same predicate.
    fn own_predicate(&self, _eph: &EphContext) -> OwnPredicate {
        OwnPredicate::Opaque
    }

    /// True when this selector's emission is exactly `find_symbol(command
    /// filters ∧ own_predicate)` with no side effects.  Such selectors fold
    /// into ONE statement-level query (OR across their own predicates)
    /// instead of one round trip each.  Contract: `fusable()` implies
    /// `own_predicate()` returns [`OwnPredicate::Filter`].
    fn fusable(&self) -> bool {
        false
    }

    /// Build a composite filter representing this selector's filter criteria.
    /// Used by scope builders to construct ScopeContext for parent/children scoping.
    /// Default: `None` (no scope filter — scope is unscoped). Override in selectors
    /// that should contribute to scope narrowing.
    fn build_composite_filter(
        &self,
        _command: &crate::command::Command,
        _eph: &EphContext,
    ) -> Option<CompositeFilter> {
        None
    }

    /// The kind of dependency this selector imposes on the given role.
    /// Default: Sufficient — the selector can produce initial output without this input.
    /// Override to Necessary when the selector cannot produce any output without it.
    fn dependency_kind(&self, _role: DependencyRole) -> DependencyKind {
        DependencyKind::Sufficient
    }

    fn update_state(&self, _state: &mut SelectorState) {}

    fn get_selection_mut<'a>(&'a self, state: &'a mut SelectorState) -> Option<&'a mut Selection> {
        state.selection.as_mut().map(Rc::make_mut)
    }

    fn get_selection<'a>(&'a self, state: &'a SelectorState) -> Option<&'a Selection> {
        state.selection.as_deref()
    }

    /// Per-selector constraint logic for `accept_notification`.
    /// Returns whether to skip, constrain, or derive for this notification.
    /// `own` is the receiving command's text, which names it in a warning.
    /// `asked` is what the index said about the rows the lists leave open
    /// (`SelectorState::unproven`), when the caller had to ask.
    /// Override to customize constraint behavior (e.g. UserVerb's forced/circular logic).
    fn try_constrain_notification(
        &self,
        registry: &mut SelectorRegistry,
        dependency: &Selection,
        notif_ctx: &NotificationContext,
        notifier: &Statement,
        own: &Span,
        asked: Option<&Related>,
    ) -> Result<ConstraintAction, pest::error::Error<Rule>> {
        // A weak notifier may seed an empty selector below, never narrow a
        // resolved one — see [`weak_notifier_blocks`].
        let has_selection = selector_state_with(registry, self, |state| state.selection.is_some());
        if weak_notifier_blocks(notifier.get_state().weak, has_selection) {
            return Ok(ConstraintAction::Skip);
        }

        let span = Span::from_pest(self.span(), notifier.command().span().input());
        let neighbour = notifier.command().query_statement_span();
        let (constrained, changed, warnings) = selector_state_with(registry, self, |state| {
            state.constrain_with_warning(
                dependency,
                &notif_ctx.role,
                notif_ctx.rel_type,
                span,
                own,
                neighbour,
                false,
                notif_ctx.receiver_is_echo,
                asked,
            )
        });

        if constrained {
            Ok(ConstraintAction::Constrained(changed, warnings))
        } else {
            Ok(ConstraintAction::Derive)
        }
    }

    /// Layer-creating selectors (e.g. `layer { … }`, `loc(…)`) return
    /// `Some(LayerSpec)` describing the ephemeral layer they want materialized
    /// before this command's query runs. The statement-execution layer is
    /// responsible for creating the layer, extending `eph_ids`, and building
    /// the [`Selection`] from the layer's instances.
    ///
    /// Returning `Some(...)` implicitly marks this selector as a barrier:
    /// pending statements drain before the layer is created so the layer
    /// `parent_id` and `eph_ids` reflect prior side effects in order.
    ///
    /// **Do not call `layer_spec` from inside [`Selector::derive_from_parent`],
    /// [`Selector::derive_from_child`], or [`Selector::derive_from_provider`]**.
    /// Those derive methods run *after* `compute_selected` has already
    /// materialised the layer for this selector; calling `layer_spec` again
    /// would either re-materialise (with a stale parent chain) or hit the
    /// cache redundantly.  If your selector implements `layer_spec`, leave
    /// the derive methods at their trait defaults.
    ///
    /// Default: `None`.
    ///
    /// `composite_filter` is the AND-fold of every filter on the surrounding
    /// command (project, name, type, …).  Selectors whose ephemeral layer
    /// depends on the filter set (currently only `search()`) mix
    /// [`CompositeFilter::hash_into`] into their cache key and apply
    /// [`CompositeFilter::compose_objects`] when scoping their query.
    /// Selectors that do not need filter awareness ignore the parameter.
    async fn layer_spec(
        &self,
        _cfg: &ControlFlowGraph,
        _eph: &EphContext,
        _composite_filter: &CompositeFilter,
        _resolved: &LabelResolutions,
        _parent_scope: &ScopeContext,
    ) -> Result<Option<LayerSpec>> {
        Ok(None)
    }

    /// Build the truncation warning to surface when this selector's ephemeral
    /// layer was capped by a result limit (i.e. `with_eph_layer` returned
    /// `truncated = true`).  The verb owns the wording and reconstructs the
    /// warning with its own span, so cache hits emit the same warning as the
    /// original cache-miss call.
    ///
    /// Default: `None` — selectors that have no notion of truncation never
    /// emit a warning.
    fn make_truncation_warning(&self) -> Option<Diagnostic> {
        None
    }

    /// Labels referenced by this selector's layer-creation inputs
    /// (e.g. an `ephemeral_instance(symbol="@foo", …)` inside a
    /// `layer { … }` block exposes `["foo"]` here).  Used by
    /// `build_dependency_graph` to add `PreSeedLabel` edges from the
    /// labelled statement to this selector's enclosing statement so
    /// the labelled statement runs first.
    ///
    /// Only meaningful for selectors whose `layer_spec` ever returns
    /// `Some` and whose layer inputs can reference `@label`s.  Default
    /// empty.
    fn layer_label_refs(&self) -> Vec<String> {
        Vec::new()
    }

    /// True iff `layer_spec` ever returns `Some`. Used by the statement
    /// executor as a sync check to decide whether to drain pending futures
    /// before this command.
    fn has_layer_spec(&self) -> bool {
        false
    }

    async fn select_from_all_impl(
        &self,
        cfg: &ControlFlowGraph,
        filter: CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        demands: FamilyDemands,
        eph: &EphContext,
    ) -> Result<Option<Selection>> {
        let selection = cfg
            .index
            .find_symbol(&filter, parent_scope, children_scope, demands, eph)
            .await?;
        Ok(Some(selection.into_inner()))
    }

    async fn derive_from_parent(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        parent: &Statement,
        notif_ctx: &NotificationContext,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        demands: FamilyDemands,
    ) -> Result<Option<Selection>> {
        // A layer-materialising command's selection comes from ITS OWN LAYER,
        // never from a neighbour.  Its predicate is not expressible as SQL --
        // `search` contributes no composite filter -- so the read below cannot
        // honour it and returns the parent's children verbatim, presented as
        // matches.  `macro("M") { search("x") }` came back as M's children.
        //
        // The command has already had its chance: Phase R runs
        // `Command::compute_selection` for every statement under tree-complete
        // visibility, and a scoped search is answered from its scope's byte
        // ranges (`SearchSelector::fusion_scope`).  So no selection here means
        // the search genuinely matched nothing in that scope, and NOTHING is
        // the right answer -- fabricating one makes it a wrong answer whose
        // wrongness depends on which side the planner resolved first.
        //
        // `Command::has_layer_spec` already gates the own-read path in
        // `command.rs`; it belongs here too.
        if self.has_layer_spec() {
            return Ok(None);
        }
        let parent_sel = match parent.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        let parent_ids = parent_sel.get_instance_ids();
        let eph = &ctx.eph;
        let mut find_parts: Vec<CompositeFilter> = vec![];
        if notif_ctx.mode != TraversalMode::Unnest {
            find_parts.push(CompositeFilter::leaf(DirectOnlyMixin::new(notif_ctx.mode)));
            find_parts.push(CompositeFilter::leaf(OuterParentFilterMixin::new(
                &parent_ids,
            )));
        }
        // M4: carry the receiver's own predicate into the derive read so it
        // fetches only matching children.  `find_child_instance_ids` extracts
        // it through the dedicated `compose_derive_*` channel — NOT the shared
        // `compose_children`/`compose_has_children`, which relate a node to its
        // displayed family and must stay type-agnostic.
        find_parts.extend(
            selector_filters
                .iter()
                .filter_map(|f| f.get_composite_filter(eph)),
        );
        let find_filter = CompositeFilter::and(find_parts);
        let (decl_ids, derive_capped) = index
            .find_child_instance_ids(
                &parent_ids,
                notif_ctx.rel_type,
                &find_filter,
                notif_ctx.within.as_deref(),
                eph,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find child instance IDs: {}", e))?;

        let mut selection = find_symbol_by_instance_id(
            index,
            selector_filters,
            &decl_ids,
            parent_scope,
            children_scope,
            demands,
            eph,
        )
        .await?;

        // An echo of a truncated parent is itself truncated: `decl_ids` was
        // derived from the parent ids that survived its LIMIT, so rows the
        // parent never fetched have no children here either.  The id-scoped
        // query above cannot notice that, so carry the flag across — otherwise
        // the echo renders short with nothing saying why.
        selection.budget_bounded |= parent_sel.budget_bounded;
        // A truncated derive read must not launder into an unflagged selection:
        // the id-fetch above hit the safety limit, so the derived set is
        // incomplete regardless of what the by-id re-read (which a selector
        // predicate can shrink below the cap) reports.  M3 then keeps this
        // selection out of composition; M8 says it.
        //
        // And what is derived from a cut statement is cut: these are the
        // children of the rows the parent happens to hold, not of the rows it
        // means.  A derive that fit its own limit used to come out unflagged,
        // and could then narrow others on that sample.
        if derive_capped || parent_sel.cut() {
            selection.mark_cut();
        }

        Ok(Some(selection))
    }

    /// What the listing [`Selector::derive_from_parent`] produced passed
    /// over: the folded symbols that sit where its children sit and match the
    /// statement's own predicate.  `None` when there are none, and for a
    /// selector whose derivation is not a listing.
    ///
    /// The same read as the derive, with the listing filter replaced by its
    /// complement -- so the two cannot disagree about what "would have been
    /// listed" means.  Only the default mode leaves anything out for being
    /// folded: `unnest` lists everything and `nested` asks about depth.
    async fn left_out_of_parent(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        selector_filters: &[&dyn Filter],
        parent: &Statement,
        notif_ctx: &NotificationContext,
    ) -> Result<Option<LeftOut>> {
        if self.has_layer_spec() || notif_ctx.mode != TraversalMode::Unfolded {
            return Ok(None);
        }
        let parent_ids = match parent.get_selection(&ctx) {
            Some(selection) => selection.get_instance_ids(),
            None => return Ok(None),
        };
        let eph = &ctx.eph;
        let mut find_parts: Vec<CompositeFilter> = vec![
            CompositeFilter::leaf(FoldedOnlyMixin::new()),
            CompositeFilter::leaf(OuterParentFilterMixin::new(&parent_ids)),
        ];
        find_parts.extend(
            selector_filters
                .iter()
                .filter_map(|f| f.get_composite_filter(eph)),
        );
        let found = index
            .find_child_instance_ids(
                &parent_ids,
                notif_ctx.rel_type,
                &CompositeFilter::and(find_parts),
                notif_ctx.within.as_deref(),
                eph,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find left-out children: {}", e))?;
        LeftOut::named(index, found, eph).await
    }

    async fn derive_from_provider(
        &self,
        ctx: &mut ExecutionContext,
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        provider: &Statement,
    ) -> Result<Option<Selection>> {
        let provider = match provider.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        Ok(Some((*provider).clone()))
    }
}

/// The folded symbols a default listing passed over; see
/// [`crate::diagnostic::Diagnostic::left_out`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LeftOut {
    /// Distinct symbol names, sorted.
    pub names: Vec<String>,
    /// The read that found them was cut, so there are more.
    pub at_least: bool,
}

impl LeftOut {
    /// Name the instances a complement read returned.  A symbol with many
    /// instances is one thing left out, so this counts names, not rows.
    pub(crate) async fn named(
        index: &Index,
        (instances, capped): (Vec<SymbolInstanceId>, bool),
        eph: &EphContext,
    ) -> Result<Option<LeftOut>> {
        if instances.is_empty() {
            return Ok(None);
        }
        let filter = CompositeFilter::leaf(SymbolInstanceIdMixin::new(&instances));
        let names = index.symbol_names_matching(&filter, eph).await?;
        if names.is_empty() {
            return Ok(None);
        }
        Ok(Some(LeftOut {
            names,
            at_least: capped,
        }))
    }
}

pub(crate) async fn find_symbol_by_instance_id(
    index: &Index,
    selector_filters: &[&dyn Filter],
    instances: &Vec<SymbolInstanceId>,
    parent_scope: ScopeContext,
    children_scope: ScopeContext,
    demands: FamilyDemands,
    eph: &EphContext,
) -> Result<Selection> {
    let mut parts: Vec<CompositeFilter> = selector_filters
        .iter()
        .filter_map(|f| f.get_composite_filter(eph))
        .collect();
    parts.push(CompositeFilter::leaf(SymbolInstanceIdMixin::new(instances)));
    let filter = CompositeFilter::and(parts);
    Ok(index
        .find_symbol(&filter, parent_scope, children_scope, demands, eph)
        .await?
        .into_inner())
}

pub trait Labeler: std::fmt::Debug {
    fn get_label(&self) -> Option<String>;
}

#[cfg(test)]
mod tests;
