use crate::cfg::ControlFlowGraph;
use crate::diagnostic::Diagnostic;
use crate::execution_context::{selector_state_with, ExecutionContext, SelectorRegistry};
use crate::execution_state::{DependencyKind, DependencyRole, RelationshipType};
use crate::parser::{Rule, Value};
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::statement::Statement;
use anyhow::Result;
use async_trait::async_trait;
use index::db_diesel::{
    CompositeFilter, DirectOnlyMixin, EphContext, EphScopedFut, EphTransaction, Index,
    OuterParentFilterMixin, RootLayer, RootShardRef, ScopeContext, Selection,
    SymbolInstanceIdMixin,
};

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
use std::collections::HashMap;
use std::fmt::Display;
use std::ptr;
use std::rc::Rc;
use std::sync::Arc;

mod generic;
mod labels;
mod preamble;

pub(crate) use self::generic::{name_filter, EphemeralOps, LabelResolutions};
pub use self::generic::{
    DefaultTypeFilter, GenericFilter, GenericSelector, NameSelector, UnitVerb,
};

use self::generic::{build_generic_verb, ForcedVerb};
use self::labels::{LabelVerb, UserVerb};

pub fn build_verb(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<(), Error<Rule>> {
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
                let positional = vec![Value::plain(label_ident.as_str())];
                LabelVerb::new(verb_span.clone(), &positional, &HashMap::new())
            }
            Rule::inherit_label_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let positional = vec![Value::plain(label_ident.as_str())];
                let mut named = HashMap::new();
                named.insert("inherit".to_string(), Value::plain("true"));
                LabelVerb::new(verb_span.clone(), &positional, &named)
            }
            Rule::use_shortcut => {
                let label_ident = verb.into_inner().next().unwrap();
                let positional = vec![Value::plain(label_ident.as_str())];
                UserVerb::new(verb_span.clone(), &positional, &HashMap::new())
            }
            Rule::plain_filter => {
                let value = Value::build(verb.into_inner().next().unwrap())?;
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".to_string(), value);
                NameSelector::new(verb_span.clone(), &positional, &named)
            }
            Rule::forced_verb => {
                let value = Value::build(verb.into_inner().next().unwrap())?;
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".to_string(), value);
                ForcedVerb::new(verb_span.clone(), &positional, &named)
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
        // reject `scope(...)` and `unnest`, modifiers that are selectors but
        // never originate a row.  A verb that is both — `"foo"`,
        // `func("foo")`, `search(...)`, `loc(...)`, a layer literal,
        // `select`, `use(...)` — is a statement's reason to exist, and a
        // preamble is not a statement.
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
    /// parse time) and `unnest` (a marker read via [`Verb::is_unnest`]).
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
/// holder of that dimension wholesale; verbs with no dimension (name
/// anchors, `ignore`, `search`, `loc`, layer literals, units) accumulate.
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
    /// Bare and named type selectors (`func`, `data("x")`, `any`) and
    /// `filter("type", ...)` — one dimension, so they replace each other
    /// symmetrically.
    Type,
    /// `filter("exact_name", ...)`.
    FilterExactName,
    /// `filter("compound_name", ...)`.
    FilterCompoundName,
}

/// Bundles the notification parameters that always travel together through
/// the notification chain: dependency role, resolved relationship type, and
/// whether the receiver uses unnest mode.
///
/// `Copy` dropped along with `DependencyRole::PreSeedLabel(Rc<str>)` —
/// the label string can't be a static type.  Most call sites passed
/// this by value; they now call `.clone()` (cheap: one `Rc` bump for
/// the label case, just an enum copy for the others).
#[derive(Debug, Clone)]
pub struct NotificationContext {
    pub role: DependencyRole,
    pub rel_type: RelationshipType,
    pub unnest: bool,
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

    /// The dimension this verb constrains, if it is a slot-holding predicate
    /// verb (see [`Dimension`]).  `None` = accumulate.
    fn dimension(&self) -> Option<Dimension> {
        None
    }

    fn update_context(&self, _ctx: &ParserContext) -> Result<bool> {
        Ok(false)
    }

    /// Whether this verb is the `unnest` marker (transitive traversal).
    fn is_unnest(&self) -> bool {
        false
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
    pub selection: Option<Selection>,
}

impl SelectorState {
    pub fn new() -> Self {
        Self { selection: None }
    }

    pub fn constrain_selection(
        &mut self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: RelationshipType,
    ) -> bool {
        // Every arm below RETAINS against `dependency`'s rows, so a dependency
        // that stopped at a `LIMIT` would silently prune neighbours that do
        // relate to the rows it never fetched.  `stage_read` prevents it by
        // clearing the result budget for any composed statement — at a cost
        // measured in `perf/BUDGET_GATE.md` (a bounded wide leaf answers in
        // 0.05s; the same leaf composed times out at 120s).  Retiring that
        // gate means teaching composition to consume a PREDICATE instead of
        // rows; until then this is the invariant that makes the gate load
        // bearing, so assert it rather than leave it implied.
        debug_assert!(
            !dependency.budget_bounded,
            "constrain_selection consumed a budget_bounded (truncated) dependency: \
             composition would treat the rows that fit the LIMIT as the whole answer"
        );

        if self.selection.is_none() {
            return false;
        }

        let len_before = self.selection.as_ref().unwrap().nodes.len();

        if let Some(_) = &mut self.selection {
            match role {
                DependencyRole::Parent => {
                    self.constrain_by_child(dependency, rel_type);
                }
                DependencyRole::Child => {
                    self.constrain_by_parent(dependency, rel_type);
                }
                DependencyRole::User => {
                    self.constrain_by_owner(dependency);
                }
                // PreSeed* edges carry no constraint data — pure
                // ordering (with optional label-resolution as a
                // side-effect at `compute_roots` time, not here).
                DependencyRole::PreSeedSibling | DependencyRole::PreSeedLabel(_) => {}
            }
        }

        let len_after = self.selection.as_ref().unwrap().nodes.len();
        len_before != len_after
    }

    fn constrain_by_parent(&mut self, parent: &Selection, rel_type: RelationshipType) {
        let check_refs = rel_type.contains(RelationshipType::REFS);
        let check_has = rel_type.contains(RelationshipType::HAS);
        let parent_symbol_ids: std::collections::HashSet<_> = if check_refs {
            parent.nodes.iter().map(|n| n.symbol.id).collect()
        } else {
            Default::default()
        };
        let parent_instance_ids: std::collections::HashSet<_> = if check_has {
            parent.nodes.iter().map(|n| n.symbol_instance.id).collect()
        } else {
            Default::default()
        };
        let selection = self.selection.as_mut().unwrap();
        // A relationship is accepted on evidence from EITHER side's edge
        // lists.  The parent's children/has_children are computed when the
        // parent's statement runs — BEFORE a layer-creating child (e.g.
        // `file("x") { search("y") }`) materialises its ephemeral rows — so
        // they cannot name those rows.  This selection's own
        // parents/has_parents were computed with the layer visible and do.
        // The union is safe: both sides express the same predicate, one may
        // just be missing edges to rows that did not exist yet.
        let own_ref_evidence: std::collections::HashSet<i64> = if check_refs {
            selection
                .parents
                .iter()
                .filter(|r| parent_symbol_ids.contains(&r.from_instance.symbol))
                .map(|r| r.to_symbol.id)
                .collect()
        } else {
            Default::default()
        };
        let own_has_evidence: std::collections::HashSet<i64> = if check_has {
            selection
                .has_parents
                .iter()
                .filter(|r| parent_instance_ids.contains(&r.parent_instance.id))
                .map(|r| r.child_instance.id)
                .collect()
        } else {
            Default::default()
        };
        selection.nodes.retain(|s| {
            (check_refs
                && (parent.children.iter().any(|r| {
                    r.symbol.id == s.symbol.id && parent_symbol_ids.contains(&r.parent_symbol.id)
                }) || own_ref_evidence.contains(&s.symbol.id)))
                || (check_has
                    && (parent.has_children.iter().any(|r| {
                        r.child_instance.id == s.symbol_instance.id
                            && parent_instance_ids.contains(&r.parent_instance.id)
                    }) || own_has_evidence.contains(&s.symbol_instance.id)))
        });
    }

    fn constrain_by_child(&mut self, child: &Selection, rel_type: RelationshipType) {
        let check_refs = rel_type.contains(RelationshipType::REFS);
        let check_has = rel_type.contains(RelationshipType::HAS);
        let child_symbol_ids: std::collections::HashSet<_> = if check_refs {
            child.nodes.iter().map(|n| n.symbol.id).collect()
        } else {
            Default::default()
        };
        let child_instance_ids: std::collections::HashSet<_> = if check_has {
            child.nodes.iter().map(|n| n.symbol_instance.id).collect()
        } else {
            Default::default()
        };
        let selection = self.selection.as_mut().unwrap();
        // Same evidence-union as `constrain_by_parent`, mirrored: a
        // relationship is accepted from EITHER side's edge lists.  With
        // two-phase execution both sides are computed under the same
        // tree-complete visibility, so this is uniformity (one side may have
        // deferred its neighbourhood query), not a freshness patch.
        let own_ref_evidence: std::collections::HashSet<i64> = if check_refs {
            selection
                .children
                .iter()
                .filter(|r| child_symbol_ids.contains(&r.symbol.id))
                .map(|r| r.parent_symbol.id)
                .collect()
        } else {
            Default::default()
        };
        let own_has_evidence: std::collections::HashSet<i64> = if check_has {
            selection
                .has_children
                .iter()
                .filter(|r| child_instance_ids.contains(&r.child_instance.id))
                .map(|r| r.parent_instance.id)
                .collect()
        } else {
            Default::default()
        };
        selection.nodes.retain(|s| {
            (check_refs
                && (child.parents.iter().any(|r| {
                    r.from_instance.symbol == s.symbol.id
                        && child_symbol_ids.contains(&r.to_symbol.id)
                }) || own_ref_evidence.contains(&s.symbol.id)))
                || (check_has
                    && (child.has_parents.iter().any(|r| {
                        r.parent_instance.id == s.symbol_instance.id
                            && child_instance_ids.contains(&r.child_instance.id)
                    }) || own_has_evidence.contains(&s.symbol_instance.id)))
        });
    }

    /// Constrain selection and produce a warning if the result is empty.
    /// Returns `(constrained, changed, warnings)` where `constrained` means a selection
    /// existed to constrain, `changed` means it was actually narrowed.
    pub fn constrain_with_warning(
        &mut self,
        dependency: &Selection,
        role: &DependencyRole,
        rel_type: RelationshipType,
        span: Span,
        context: &str,
    ) -> (bool, bool, Vec<Diagnostic>) {
        if self.selection.is_none() {
            return (false, false, vec![]);
        }
        let changed = self.constrain_selection(dependency, role, rel_type);
        let mut warnings = vec![];
        if changed && self.selection.as_ref().unwrap().nodes.is_empty() {
            warnings.push(Diagnostic::note(
                span,
                format!(
                    "Statement did not match any symbols after applying constraints from {}.",
                    context,
                ),
            ));
        }
        (true, changed, warnings)
    }

    fn constrain_by_owner(&mut self, owner: &Selection) {
        let selection = self.selection.as_mut().unwrap();
        selection.nodes.retain(|u| {
            owner
                .nodes
                .iter()
                .any(|o| o.symbol_instance.id == u.symbol_instance.id)
        });
    }
}

pub type SelectorId = usize;

/// The weakness rule, in one place.
///
/// A weak substatement is a display echo, so it must never *narrow* a
/// neighbour that has already resolved — but it may still *supply* a
/// selection to one that has none.  That second half is not a loophole: it
/// is how a chain propagates through weak intermediaries (`{{"a"}}`, or
/// `data(inherit="true") {{{"channels_a"}}}`), where every middle scope is
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
        state.selection.as_mut()
    }

    fn get_selection<'a>(&'a self, state: &'a SelectorState) -> Option<&'a Selection> {
        state.selection.as_ref()
    }

    /// Per-selector constraint logic for `accept_notification`.
    /// Returns whether to skip, constrain, or derive for this notification.
    /// Override to customize constraint behavior (e.g. UserVerb's forced/circular logic).
    fn try_constrain_notification(
        &self,
        registry: &mut SelectorRegistry,
        dependency: &Selection,
        notif_ctx: &NotificationContext,
        notifier: &Statement,
    ) -> Result<ConstraintAction, pest::error::Error<Rule>> {
        // A weak notifier may seed an empty selector below, never narrow a
        // resolved one — see [`weak_notifier_blocks`].
        let has_selection = selector_state_with(registry, self, |state| state.selection.is_some());
        if weak_notifier_blocks(notifier.get_state().weak, has_selection) {
            return Ok(ConstraintAction::Skip);
        }

        let span = Span::from_pest(self.span(), notifier.command().span().input());
        let context = format!("{}", notifier.command().span());
        let (constrained, changed, warnings) = selector_state_with(registry, self, |state| {
            state.constrain_with_warning(
                dependency,
                &notif_ctx.role,
                notif_ctx.rel_type,
                span,
                &context,
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
        eph: &EphContext,
    ) -> Result<Option<Selection>> {
        let selection = cfg
            .index
            .find_symbol(&filter, parent_scope, children_scope, eph)
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
    ) -> Result<Option<Selection>> {
        let parent_sel = match parent.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };
        let parent_ids = parent_sel.get_instance_ids();
        let mut find_parts: Vec<CompositeFilter> = vec![];
        if !notif_ctx.unnest {
            find_parts.push(CompositeFilter::leaf(DirectOnlyMixin::new()));
            find_parts.push(CompositeFilter::leaf(OuterParentFilterMixin::new(
                &parent_ids,
            )));
        }
        let find_filter = CompositeFilter::and(find_parts);
        let eph = &ctx.eph;
        let decl_ids = index
            .find_child_instance_ids(
                &parent_ids,
                notif_ctx.rel_type.contains(RelationshipType::REFS),
                notif_ctx.rel_type.contains(RelationshipType::HAS),
                &find_filter,
                eph,
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find child instance IDs: {}", e))?;

        let selection = find_symbol_by_instance_id(
            index,
            selector_filters,
            &decl_ids,
            parent_scope,
            children_scope,
            eph,
        )
        .await?;

        Ok(Some(selection))
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
        Ok(Some(provider.clone()))
    }
}

pub(crate) async fn find_symbol_by_instance_id(
    index: &Index,
    selector_filters: &[&dyn Filter],
    instances: &Vec<SymbolInstanceId>,
    parent_scope: ScopeContext,
    children_scope: ScopeContext,
    eph: &EphContext,
) -> Result<Selection> {
    let mut parts: Vec<CompositeFilter> = selector_filters
        .iter()
        .filter_map(|f| f.get_composite_filter(eph))
        .collect();
    parts.push(CompositeFilter::leaf(SymbolInstanceIdMixin::new(instances)));
    let filter = CompositeFilter::and(parts);
    Ok(index
        .find_symbol(&filter, parent_scope, children_scope, eph)
        .await?
        .into_inner())
}

pub trait Labeler: std::fmt::Debug {
    fn get_label(&self) -> Option<String>;
}

#[cfg(test)]
mod tests;
