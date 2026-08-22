use crate::cfg::ControlFlowGraph;
use crate::diagnostic::Diagnostic;
use crate::execution_context::{selector_state_with, ExecutionContext};
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::Rule;
use crate::span::Span;
use crate::statement::Statement;
use crate::verb::{
    find_symbol_by_instance_id, weak_notifier_blocks, ConstraintAction, DeriveMethod, Filter,
    LabelResolutions, Labeler, LayerShardPopulate, LayerSpec, NotificationContext, OwnPredicate,
    RootShardPopulate, SelectionShardPopulate, Selector, SelectorId, Verb, VerbClass,
};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{
    CompositeFilter, EphContext, Index, InnermostOnlyMixin, ScopeContext, Selection,
    SymbolInstanceIdMixin,
};
use index::symbols::SymbolInstanceId;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Which shard of a root's cache entry a [`LayerActivation`] refers to —
/// re-exported from the index crate, which produces the layers in this
/// exact role order.
pub use index::db_diesel::ShardRole;

/// One ephemeral-layer touch by a statement: either the layer was freshly
/// created and populated in this call (`created = true`), or the statement
/// hit an existing cache row (`created = false`).  `truncated` mirrors the
/// persistent `layers.truncated` flag as observed by this call.
///
/// Recorded on [`ExecutionContext::layer_activations`] so tests can assert
/// cache behaviour (populate vs reuse) directly instead of inferring it
/// from eph instance IDs.  A layer-creating statement records activations
/// per visible root, roots ascending; within a root, the root shard first,
/// then the selection shard.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerActivation {
    /// The root whose chain this layer joined.
    pub root_id: i64,
    pub layer_id: i64,
    pub created: bool,
    pub truncated: bool,
    pub role: ShardRole,
}

/// Result of computing initial selections for a statement's selectors.
/// Phase R is read-only: chain growth and activation recording happen in
/// Phase M (`materialise_layers` → the executor's per-tree materialisation).
pub struct ComputeResult {
    pub selections: Vec<(SelectorId, Option<Selection>)>,
    pub warnings: Vec<Diagnostic>,
}

impl ComputeResult {
    pub fn apply(self, ctx: &mut ExecutionContext, statement: &Statement) {
        for (id, selection) in self.selections {
            ctx.registry.add_by_id(id, selection);
        }
        statement.get_state_mut().warnings.extend(self.warnings);
    }
}

/// Phase-M output of one statement: the layers it materialised (already
/// created in the DB, NOT yet pushed onto the executor's chains — the
/// executor folds `layers` into its tree's SINGLE materialisation, so the
/// whole tree's layers enter visibility together), plus the Phase-M warnings
/// (layer truncation) to be carried into the statement's Phase R.
pub struct MaterialiseOutcome {
    /// All materialised layer ids (root, layer, and selection shards, every
    /// root) — Phase R's layer-union read enumerates rows from exactly these.
    pub layer_ids: Vec<i64>,
    /// `(root_id, layer_id)` pairs for `EphContext::push_materialisation`.
    pub layers: Vec<(i64, i64)>,
    /// Per-shard cache activations, in recording order.
    pub activations: Vec<LayerActivation>,
    /// Phase-M warnings (verb-limit truncation), surfaced via Phase R's
    /// `ComputeResult`.
    pub warnings: Vec<Diagnostic>,
}

impl MaterialiseOutcome {
    fn empty() -> Self {
        Self {
            layer_ids: Vec::new(),
            layers: Vec::new(),
            activations: Vec::new(),
            warnings: Vec::new(),
        }
    }
}

pub struct NotificationResult {
    pub changed: bool,
    pub warnings: Vec<Diagnostic>,
}

impl NotificationResult {
    pub fn new(changed: bool, warnings: Vec<Diagnostic>) -> Self {
        Self { changed, warnings }
    }
}

/// Fold a statement's filter conjunction with its fusable selectors' OR
/// branches into the single predicate their shared emission (and its
/// cardinality probe) runs: `AND(filters) ∧ OR(own predicates)`.
pub(crate) fn fuse_predicate(
    mut filter_parts: Vec<CompositeFilter>,
    selectors: &[&dyn Selector],
    eph: &EphContext,
) -> CompositeFilter {
    let mut or_parts: Vec<CompositeFilter> = Vec::new();
    let mut unconstrained = false;
    for sel in selectors {
        match sel.own_predicate(eph) {
            OwnPredicate::Filter(Some(f)) => or_parts.push(f),
            // A branch with no extra constraint widens the OR to the
            // whole filter conjunction.
            OwnPredicate::Filter(None) => unconstrained = true,
            OwnPredicate::Opaque => {
                unreachable!("fusable selector must expose an own predicate")
            }
        }
    }
    if !unconstrained && !or_parts.is_empty() {
        filter_parts.push(CompositeFilter::or(or_parts));
    }
    CompositeFilter::and(filter_parts)
}

/// A statement's command, stored as a record of aspects: every verb is
/// routed by its declared [`VerbClass`] into the matching field.  The merge
/// fold (per-tag override, type replacement) is a predicate-aspect law and
/// runs only there; the other aspects accumulate.  Iteration order within
/// each aspect is source order.
#[derive(Debug, Default)]
pub struct Command {
    /// Filters and anchors — the truth-valued aspect.
    predicate: Vec<Arc<dyn Verb>>,
    /// `unnest` marker (`has`/`refs`/`derive` are consumed at parse time).
    relationship: Vec<Arc<dyn Verb>>,
    /// Label plumbing: `label`/`@x` definitions, `use`/`#x` references.
    bindings: Vec<Arc<dyn Verb>>,
    /// Environment configuration.  Today its only member (`preamble`) is
    /// ALWAYS consumed at parse time, so this vec is empty in every
    /// reachable command; the field keeps the routing in `extend` total and
    /// mirrors the statement record of the filter-expressions design.
    env: Vec<Arc<dyn Verb>>,
    span: Option<Span>,
    verb_span: Option<Span>,
}

fn derive_aspect(verbs: &[Arc<dyn Verb>]) -> Vec<Arc<dyn Verb>> {
    let mut derived = vec![];
    for verb in verbs.iter() {
        match verb.derive_method() {
            DeriveMethod::Clone => {
                // Use derive_new_instance if available to create an independent
                // copy, avoiding shared registry state between parent and child.
                derived.push(verb.derive_new_instance().unwrap_or_else(|| verb.clone()));
            }
            DeriveMethod::Skip => {}
        }
    }
    derived
}

impl Command {
    pub fn new(span: Span) -> Command {
        Self {
            span: Some(span),
            ..Default::default()
        }
    }

    pub fn derive(&self, span: Span) -> Self {
        Self {
            predicate: derive_aspect(&self.predicate),
            relationship: derive_aspect(&self.relationship),
            bindings: derive_aspect(&self.bindings),
            env: derive_aspect(&self.env),
            span: Some(span),
            verb_span: None,
        }
    }

    /// All verbs across every aspect, predicate first.  For aspect-agnostic
    /// set-properties (anchoring, tags); relative order across aspects is
    /// not source order and nothing may depend on it.
    fn all_verbs<'a>(&'a self) -> impl Iterator<Item = &'a Arc<dyn Verb>> + 'a {
        self.predicate
            .iter()
            .chain(self.relationship.iter())
            .chain(self.bindings.iter())
            .chain(self.env.iter())
    }

    pub fn span(&self) -> &Span {
        self.span.as_ref().unwrap()
    }

    pub fn set_verb_span(&mut self, span: Span) {
        self.verb_span = Some(span);
    }

    /// Returns the verb-only span if available (for statements with non-empty scopes),
    /// otherwise falls back to the full statement span.
    pub fn query_statement_span(&self) -> &Span {
        self.verb_span
            .as_ref()
            .unwrap_or_else(|| self.span.as_ref().unwrap())
    }

    pub fn extend(&mut self, other: Arc<dyn Verb>) {
        // A verb's declared class must agree with the roles it exposes:
        // predicate verbs filter or select; bindings label or select (`use`
        // holds selector state); relationship/env verbs never filter.
        debug_assert!(
            match other.class() {
                VerbClass::Predicate =>
                    other.as_filter().is_some() || other.as_selector().is_some(),
                VerbClass::Binding => other.as_labeler().is_some() || other.as_selector().is_some(),
                VerbClass::Relationship | VerbClass::Env => other.as_filter().is_none(),
            },
            "verb `{}` declares class {:?} inconsistent with its roles",
            other.name(),
            other.class(),
        );
        match other.class() {
            // The slot cascade, in full: writing a dimension replaces the
            // previous holder of that dimension wholesale; dimensionless
            // predicate verbs (anchors, exclusions, layer-backed selectors)
            // accumulate.  Verbs never inspect each other.
            VerbClass::Predicate => {
                if let Some(dim) = other.dimension() {
                    self.predicate.retain(|v| v.dimension() != Some(dim));
                }
                self.predicate.push(other);
            }
            VerbClass::Relationship => self.relationship.push(other),
            VerbClass::Binding => self.bindings.push(other),
            VerbClass::Env => self.env.push(other),
        }
    }

    pub(crate) fn filters<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Filter> + 'a> {
        Box::new(self.predicate.iter().filter_map(|verb| verb.as_filter()))
    }

    /// Selectors span two aspects: the predicate's anchors/branches and the
    /// binding aspect's `use` verbs, which hold execution state like any
    /// selector.  Relative order across the two aspects carries no meaning.
    pub fn selectors<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Selector> + 'a> {
        Box::new(
            self.predicate
                .iter()
                .chain(self.bindings.iter())
                .filter_map(|verb| verb.as_selector()),
        )
    }

    /// True when any verb contributes an anchor (see [`Verb::anchor_kind`]):
    /// the statement denotes a set narrow enough in principle to drive
    /// execution on its own.  Non-anchored statements only ever derive from
    /// their neighbours.
    pub fn is_anchored(&self) -> bool {
        self.all_verbs().any(|v| v.anchor_kind().is_some())
    }

    /// True when this command carries a real constraint: a filter verb, or
    /// a selector that constrains.  Unit verbs (and other non-constraining
    /// selectors) are pure structure — a component made only of those is a
    /// harmless degenerate query, not an anchoring error.  Preamble
    /// statements end up unit-only by construction (their verbs are
    /// redirected to the global context), so directives never demand.
    pub fn demands_anchoring(&self) -> bool {
        self.all_verbs().any(|v| {
            v.as_filter().is_some()
                || (v.as_selector().is_some() && !v.is_non_constraining_selector())
        })
    }

    /// The bare filter conjunction of this command, when non-empty — the
    /// predicate the derive path applies for unit/filter-only statements
    /// (`func { }`), used as their refinement-probe predicate.
    pub fn constraint_predicate(&self, eph: &EphContext) -> Option<CompositeFilter> {
        let parts: Vec<CompositeFilter> = self
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        if parts.is_empty() {
            None
        } else {
            Some(CompositeFilter::and(parts))
        }
    }

    /// The predicate a cardinality probe runs for this statement, when one
    /// exists: the statement must be anchored and every selector fusable
    /// (its whole emission is one `find_symbol` over this predicate).
    /// Layer-backed, forced, unit, and label statements return `None` and
    /// keep their own read paths unprobed.
    pub fn probe_predicate(&self, eph: &EphContext) -> Option<CompositeFilter> {
        if !self.is_anchored() {
            return None;
        }
        let selectors: Vec<&dyn Selector> = self.selectors().collect();
        if selectors.is_empty() || !selectors.iter().all(|s| s.fusable()) {
            return None;
        }
        let filter_parts: Vec<CompositeFilter> = self
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        Some(fuse_predicate(filter_parts, &selectors, eph))
    }

    /// Check if any verb suppresses the default type filter.
    pub fn has_suppress_default_type_filter(&self) -> bool {
        self.all_verbs().any(|v| v.suppresses_default_type_filter())
    }

    pub fn has_selectors(&self) -> bool {
        self.selectors().next().is_some()
    }

    /// Whether any selector in this command materializes an ephemeral layer.
    /// Layer-creating commands are implicit barriers: the statement executor
    /// drains pending statements before this one so the layer's `parent_id`
    /// and the visible `eph_ids` reflect prior side effects in order.
    pub fn has_layer_spec(&self) -> bool {
        self.selectors().any(|s| s.has_layer_spec())
    }

    /// Whether the `unnest` marker is present.
    pub fn has_unnest(&self) -> bool {
        self.relationship.iter().any(|v| v.is_unnest())
    }

    pub fn is_unit(&self) -> bool {
        self.selectors().all(|verb| verb.is_unit())
    }

    /// Whether this command constrains nothing of its own — the weakness
    /// candidate test (see `Statement::mark_weak_statements`).  Two
    /// conditions, because a constraint can arrive as either kind of verb:
    /// every selector is non-constraining (units, bare type selectors), AND
    /// no verb carries a name constraint.
    ///
    /// The second clause is what a selector-only scan missed: a filter-only
    /// command (`filter("exact_name","foo") { … }`) has no selector but the
    /// parser's `UnitVerb`, so it used to come out non-constraining — and
    /// therefore weak — while `is_anchored` reported it as anchored on the
    /// very same name.  A weak statement "does not constrain the selection
    /// of its dependencies", yet the probe path binds a resolved
    /// neighbour's role regardless of weakness, so the two disagreed about
    /// one query.  Requiring "no name constraint" makes `anchored ⇒ not
    /// weak` hold by construction: every other [`crate::verb::AnchorKind`]
    /// belongs to a verb that is already a constraining selector.
    ///
    /// Type predicates stay structural — bare `func` and `filter("type", …)`
    /// carry no name constraint and remain weakness candidates, so a caller
    /// chain typed from the top still derives instead of pruning itself.
    pub fn is_non_constraining(&self) -> bool {
        self.selectors()
            .all(|verb| verb.is_non_constraining_selector())
            && !self.all_verbs().any(|verb| verb.has_name_constraint())
    }

    fn labels<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Labeler> + 'a> {
        Box::new(self.bindings.iter().filter_map(|verb| verb.as_labeler()))
    }

    pub fn get_labels(&self) -> Vec<String> {
        self.labels().flat_map(|m| m.get_label()).collect()
    }

    /// Build a composite filter from all selectors (ORed across selectors).
    /// Used by scope builders to construct ScopeContext.
    ///
    /// Honest OR: selectors in a command are ORed, so if ANY selector cannot
    /// express itself as a filter (`build_composite_filter` = None — user
    /// verbs, layer verbs, units), the whole OR must widen to `None`
    /// (unscoped), never narrow to the subset that could.  A narrowed scope is
    /// recoverable for edge prefetches (the notification pass re-constrains
    /// with real ids) but PERMANENT for scope-fusion, which materialises only
    /// what the scope admits.
    pub fn get_selector_composite_filter(&self, eph: &EphContext) -> Option<CompositeFilter> {
        let mut parts: Vec<CompositeFilter> = Vec::new();
        for sel in self.selectors() {
            match sel.build_composite_filter(self, eph) {
                Some(f) => parts.push(f),
                None => return None,
            }
        }
        match parts.len() {
            0 => None,
            1 => parts.into_iter().next(),
            _ => Some(CompositeFilter::or(parts)),
        }
    }

    /// Aggregate every layer-creating selector's `LayerSpec` into a single
    /// per-statement `LayerSpec`.  Returns:
    /// - `None` when no selector contributes a layer (statement inherits
    ///   the prior `eph` chain unchanged).
    /// - `Some(spec)` returned as-is when exactly one selector contributes
    ///   (single-verb statements keep their original hash + kind so the
    ///   cache stays warm across the refactor).
    /// - `Some(composite)` for multi-verb statements: hash chains the
    ///   per-verb hashes in source order, populate runs each verb's
    ///   contribution in turn, `kind = Composite`, `parent_id` taken from
    ///   the first spec (all specs were built from the same `eph`
    ///   snapshot, so they agree).
    /// Labels referenced by any layer-creating selector in this
    /// command.  Used by `build_dependency_graph` to add
    /// `PreSeedLabel` edges so the labelled statements run before
    /// this command's layer materialises.
    ///
    /// Iterates selectors only — `layer_label_refs` lives on the
    /// `Selector` trait next to `layer_spec`, so non-selector verbs
    /// (filters, markers, labelers) don't carry the API.  The default
    /// `Selector::layer_label_refs()` returns an empty `Vec`, so the
    /// iteration cost is negligible for the common no-label case.
    pub fn layer_label_refs(&self) -> Vec<String> {
        self.selectors()
            .flat_map(|s| s.layer_label_refs())
            .collect()
    }

    pub async fn aggregate_layer_spec(
        &self,
        cfg: &ControlFlowGraph,
        eph: &EphContext,
        composite_filter: &CompositeFilter,
        resolved: &LabelResolutions,
        parent_scope: &ScopeContext,
    ) -> Result<Option<LayerSpec>> {
        let mut specs: Vec<LayerSpec> = Vec::new();
        for selector in self.selectors() {
            if let Some(spec) = selector
                .layer_spec(cfg, eph, composite_filter, resolved, parent_scope)
                .await?
            {
                specs.push(spec);
            }
        }
        match specs.len() {
            0 => Ok(None),
            1 => Ok(Some(specs.into_iter().next().unwrap())),
            _ => Ok(Some(Self::partitioned_composite(specs))),
        }
    }

    /// Merge ≥2 specs into one composite: the root-shard halves combine into
    /// one composite root shard (keyed on the part input hashes in source
    /// order — still eph-independent), the selection-shard halves into one
    /// composite selection shard.  Truncation ORs across parts per half.
    fn partitioned_composite(parts: Vec<LayerSpec>) -> LayerSpec {
        let mut h = Sha256::new();
        h.update(b"composite-input-v1");
        for p in &parts {
            h.update(p.input_hash);
        }
        let input_hash: [u8; 32] = h.finalize().into();

        tracing::debug!(
            composite_input_hash = ?&input_hash[..8],
            parts = ?parts
                .iter()
                .map(|p| &p.input_hash[..8])
                .collect::<Vec<_>>(),
            "partitioned composite layer spec synthesised",
        );

        // Composite selection-shard key: fold every part's extra
        // (length-prefixed, source order) so parts that differ only in their
        // selection-shard inputs produce distinct composite selection shards.
        // `selection_extra` is the extra cache-key material a part's
        // chain-dependent delta reads beyond (parent, root shard); a part whose
        // delta is fully determined by (parent, root shard) — search, loc today
        // — contributes nothing to key on.  When *every* part is empty the
        // composite has no extra either, but `finalize()` is unconditionally 32
        // bytes, so hashing here would fabricate a key the guard in
        // `compute_selected` then rejects on an empty chain ("selection-shard
        // inputs but no ephemeral context").  Stay empty so the parts just
        // union.
        let selection_extra: Vec<u8> = if parts.iter().all(|p| p.selection_extra.is_empty()) {
            Vec::new()
        } else {
            let mut he = Sha256::new();
            he.update(b"composite-extra-v1");
            for p in &parts {
                he.update((p.selection_extra.len() as u64).to_le_bytes());
                he.update(&p.selection_extra);
            }
            he.finalize().to_vec()
        };

        let mut root_shard_populates = Vec::with_capacity(parts.len());
        let mut selection_shard_populates = Vec::with_capacity(parts.len());
        // Only content-scan parts (search, loc) expose a layer-shard populate;
        // a `layer { … }` part contributes none.  Collect just the present
        // ones so the composite layer shard is the union of the content scans.
        let mut layer_shard_populates = Vec::with_capacity(parts.len());
        for p in parts {
            root_shard_populates.push(p.root_shard_populate);
            selection_shard_populates.push(p.selection_shard_populate);
            if let Some(pl) = p.layer_shard_populate {
                layer_shard_populates.push(pl);
            }
        }

        // `Fn` closures run once per root; the parts are shared across those
        // runs via `Arc` (the returned future cannot borrow the closure's
        // environment under the HRTB, so each call clones the handle in).
        let root_shard_populates = std::sync::Arc::new(root_shard_populates);
        let selection_shard_populates = std::sync::Arc::new(selection_shard_populates);

        let root_shard_populate: RootShardPopulate = Box::new(move |txn, root| {
            let populates = std::sync::Arc::clone(&root_shard_populates);
            Box::pin(async move {
                let mut truncated = false;
                for populate in populates.iter() {
                    truncated |= populate(&mut *txn, root).await?;
                }
                Ok(truncated)
            })
        });
        let selection_shard_populate: SelectionShardPopulate =
            Box::new(move |txn, root, root_shard_ref| {
                let populates = std::sync::Arc::clone(&selection_shard_populates);
                Box::pin(async move {
                    let mut truncated = false;
                    for populate in populates.iter() {
                        truncated |= populate(&mut *txn, root, root_shard_ref).await?;
                    }
                    Ok(truncated)
                })
            });

        // Composite layer-shard scan: union every content-scan part at each
        // shard layer `L_j`.  `None` iff no part is a content scan (e.g. a
        // lone `layer { … }` block), so the executor materialises no layer
        // shards.
        let layer_shard_populate: Option<LayerShardPopulate> = if layer_shard_populates.is_empty() {
            None
        } else {
            let layer_shard_populates = std::sync::Arc::new(layer_shard_populates);
            let closure: LayerShardPopulate =
                Box::new(move |txn, root, layer_id, root_shard_ref| {
                    let populates = std::sync::Arc::clone(&layer_shard_populates);
                    Box::pin(async move {
                        let mut truncated = false;
                        for populate in populates.iter() {
                            truncated |=
                                populate(&mut *txn, root, layer_id, root_shard_ref).await?;
                        }
                        Ok(truncated)
                    })
                });
            Some(closure)
        };

        LayerSpec {
            input_hash,
            root_shard_populate,
            selection_shard_populate,
            layer_shard_populate,
            selection_extra,
        }
    }

    /// Notify all selectors from a pre-built selection (constraint + derivation).
    ///
    /// This is the parent's side of the bottom-up notification, called ONCE
    /// PER PARTICIPATING CHILD with that child's own selection — sibling
    /// children conjoin.  Both paths compose correctly under repetition:
    /// `constrain_by_child`'s `retain` is a pure monotone filter, so applying
    /// child after child yields their intersection, and derivation happens
    /// only on the call that finds a selector still empty (the first), the
    /// rest constraining what it produced.  Deriving per child and unioning
    /// would re-introduce the disjunction.
    ///
    /// `notifier_weak` is the notifying child's weakness, which gates the
    /// constrain half exactly as it does on the parent→child edge — see
    /// [`weak_notifier_blocks`].  The derivation half below is deliberately
    /// left open to weak children: supplying a selection is not narrowing
    /// one.
    pub async fn notify_from_selection(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        dependency: &Selection,
        notifier_weak: bool,
        role: DependencyRole,
        rel_type: RelationshipType,
        unnest: bool,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<NotificationResult, pest::error::Error<Rule>> {
        let mut changed = false;
        let mut warnings = vec![];
        let selector_filters: Vec<&dyn Filter> = self.filters().collect();
        let mut derivation_ids = None;
        for selector in self.selectors() {
            // A weak child may seed a selector that has nothing (the
            // derivation below), but must not narrow one that has already
            // resolved — the same rule the parent→child edge applies in
            // `Selector::try_constrain_notification`.
            let has_selection = selector_state_with(&mut ctx.registry, selector, |state| {
                state.selection.is_some()
            });
            if weak_notifier_blocks(notifier_weak, has_selection) {
                continue;
            }

            let span = Span::from_pest(selector.span(), self.span().input());
            let (constrained, sel_changed, sel_warnings) =
                selector_state_with(&mut ctx.registry, selector, |state| {
                    state.constrain_with_warning(dependency, &role, rel_type, span, "children")
                });
            changed |= sel_changed;
            warnings.extend(sel_warnings);

            if constrained {
                continue;
            }

            // Derivation path: derive parent's selection from merged children.
            if role != DependencyRole::Parent {
                continue;
            }

            if derivation_ids.is_none() {
                let child_ids = dependency.get_instance_ids();
                let mut find_parts: Vec<CompositeFilter> = vec![];
                if !unnest {
                    find_parts.push(CompositeFilter::leaf(InnermostOnlyMixin::new()));
                }
                let find_filter = CompositeFilter::and(find_parts);
                derivation_ids = Some(
                    index
                        .find_parent_instance_ids(
                            &child_ids,
                            rel_type.contains(RelationshipType::REFS),
                            rel_type.contains(RelationshipType::HAS),
                            &find_filter,
                            &ctx.eph,
                        )
                        .await
                        .map_err(|e| {
                            pest::error::Error::new_from_span(
                                pest::error::ErrorVariant::CustomError {
                                    message: format!("Failed to find parent instance IDs: {}", e),
                                },
                                selector.span(),
                            )
                        })?,
                );
            }
            let decl_ids = derivation_ids.as_ref().unwrap();

            let selection = find_symbol_by_instance_id(
                index,
                &selector_filters,
                decl_ids,
                parent_scope.clone(),
                children_scope.clone(),
                &ctx.eph,
            )
            .await
            .map_err(|e| {
                pest::error::Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!("Failed to derive selection: {}", e),
                    },
                    selector.span(),
                )
            })?;

            selector_state_with(&mut ctx.registry, selector, |state| {
                state.selection = Some(selection);
            });
            changed = true;
        }
        Ok(NotificationResult::new(changed, warnings))
    }

    pub async fn accept_notification(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        notifier: &Statement,
        notif_ctx: NotificationContext,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<NotificationResult, pest::error::Error<Rule>> {
        if !notifier.command().has_selectors() {
            return Ok(NotificationResult::new(false, vec![]));
        }

        let dependency = match notifier.get_selection(&ctx) {
            Some(selection) => selection,
            None => return Ok(NotificationResult::new(false, vec![])),
        };

        let mut changed = false;
        let mut warnings = vec![];
        let selector_filters: Vec<&dyn Filter> = self.filters().collect();
        let notifier_labels = if notif_ctx.role == DependencyRole::User {
            Some(notifier.command().get_labels())
        } else {
            None
        };

        for selector in self.selectors() {
            if let Some(ref notifier_labels) = notifier_labels {
                let self_label = match selector.get_label() {
                    Some(label) => label,
                    None => continue,
                };
                if !notifier_labels.contains(&self_label) {
                    continue;
                }
            }

            match selector.try_constrain_notification(
                &mut ctx.registry,
                &dependency,
                &notif_ctx,
                notifier,
            )? {
                ConstraintAction::Skip => continue,
                ConstraintAction::Constrained(sel_changed, sel_warnings) => {
                    changed |= sel_changed;
                    warnings.extend(sel_warnings);
                    continue;
                }
                ConstraintAction::Derive => {} // fall through to derivation
            }

            // Derive selection: dispatch based on dependency role
            let selection = match notif_ctx.role {
                DependencyRole::Child => {
                    selector
                        .derive_from_parent(
                            ctx,
                            index,
                            &selector_filters,
                            notifier,
                            &notif_ctx,
                            parent_scope.clone(),
                            children_scope.clone(),
                        )
                        .await
                }
                DependencyRole::User => {
                    selector
                        .derive_from_provider(ctx, index, &selector_filters, notifier)
                        .await
                }
                // Parent notifications never reach this dispatch: since
                // siblings conjoin, `Statement::notify` handles the
                // child→parent edge itself, constraining the parent once per
                // participating child through `notify_from_selection` (which
                // derives from the first child that finds it empty).  PreSeed*
                // are short-circuited to a no-op there as well.  If either
                // somehow arrives, produce no selection.
                DependencyRole::Parent
                | DependencyRole::PreSeedSibling
                | DependencyRole::PreSeedLabel(_) => Ok(None),
            }
            .map_err(|e| {
                pest::error::Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!("Failed to derive selection: {}", e),
                    },
                    selector.span(),
                )
            })?;

            selector_state_with(&mut ctx.registry, selector, |state| {
                state.selection = selection;
            });
            changed = true;
        }
        Ok(NotificationResult::new(changed, warnings))
    }

    /// Phase M: materialise this statement's ephemeral layers (if any of its
    /// verbs contribute one).  Multi-verb statements get a `Composite` layer
    /// that combines every verb's contribution; the aggregation is in
    /// `Command::aggregate_layer_spec`.
    ///
    /// Runs against the PRE-TREE visibility snapshot (the executor takes it
    /// once per top-level statement tree): no command sees a tree-mate's
    /// layers at materialise time, so selection shards parent on the previous
    /// tree's tip and `has_chain` guards evaluate pre-tree.  The returned
    /// `layers` is NOT pushed here — the executor folds it into the tree's
    /// single materialisation, after which every statement's Phase R sees the
    /// whole tree's layers.  `parent_scope` carries the enclosing
    /// container's CONDITION for scope-fusion (nothing is computed yet in
    /// Phase M, so it is condition-based by construction).
    pub async fn materialise_layers(
        &self,
        cfg: &ControlFlowGraph,
        eph: &EphContext,
        resolved: &LabelResolutions,
        parent_scope: &ScopeContext,
    ) -> Result<MaterialiseOutcome, pest::error::Error<Rule>> {
        if self.selectors().next().is_none() {
            return Ok(MaterialiseOutcome::empty());
        }

        let to_pest = |e: anyhow::Error| {
            pest::error::Error::new_from_span(
                pest::error::ErrorVariant::CustomError {
                    message: e.to_string(),
                },
                self.span().as_pest_span(),
            )
        };

        // The CompositeFilter is built once per statement so layer-aware
        // selectors can mix it into their cache key and apply its
        // `compose_objects` to scope their populate queries.
        let filter_parts_for_layer: Vec<CompositeFilter> = self
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();
        let layer_composite_filter = CompositeFilter::and(filter_parts_for_layer);

        let spec = match self
            .aggregate_layer_spec(cfg, eph, &layer_composite_filter, resolved, parent_scope)
            .await
            .map_err(to_pest)?
        {
            None => return Ok(MaterialiseOutcome::empty()),
            Some(spec) => spec,
        };

        // Defense in depth, uniform across verbs: non-empty
        // selection-shard inputs under an empty chain means the verb
        // computed eph-derived content that no selection shard will ever
        // materialize — those rows would be silently dropped.  The
        // check runs against the pre-tree snapshot, which is correct
        // by construction: labels may only reference earlier trees,
        // so no eph input can originate from a tree-mate.  The
        // verb-side guards (with better spans) should have caught
        // this; reaching here is an internal invariant violation.
        if !eph.has_chain() && !spec.selection_extra.is_empty() {
            return Err(to_pest(anyhow::anyhow!(
                "internal error: layer spec carries selection-shard inputs \
                 but no ephemeral context exists before this statement"
            )));
        }

        // Chain topology is decided here, not by the verb: per
        // visible root, the root shard parents on the root and the
        // selection shard (created iff that root's pre-tree chain is
        // non-empty) chains on the root's `tip` — the previous
        // TREE's tip, since `eph` is the pre-tree snapshot.  The input
        // hash is salted per root (`root_shard_hash`), so each root's
        // cache entry is independent of co-visible projects.
        let results = cfg
            .index
            .with_partitioned_layers(
                eph,
                &spec.input_hash,
                &spec.selection_extra,
                spec.root_shard_populate,
                spec.selection_shard_populate,
                spec.layer_shard_populate,
            )
            .await
            .map_err(to_pest)?;

        let mut outcome = MaterialiseOutcome::empty();
        let mut hit_ids = Vec::with_capacity(results.len());
        let mut truncated_any = false;
        // Results arrive in recording order (roots ascending; within a
        // root: root shard → layer shards → selection shard), so this
        // command's final layer per root is its selection shard — and when
        // this command is the tree's last layer-bearing substatement, that
        // selection shard is the tree's new tip.
        for layer in results {
            if !layer.outcome.created {
                hit_ids.push(layer.outcome.layer_id);
            }
            outcome.activations.push(LayerActivation {
                root_id: layer.root_id,
                layer_id: layer.outcome.layer_id,
                created: layer.outcome.created,
                truncated: layer.outcome.truncated,
                role: layer.role,
            });
            truncated_any |= layer.outcome.truncated;
            outcome.layers.push((layer.root_id, layer.outcome.layer_id));
            outcome.layer_ids.push(layer.outcome.layer_id);
        }
        // One LRU touch round trip for all roots' hits.
        let _ = cfg.index.touch_eph_layers(&hit_ids).await;

        // Surface truncation as a single warning per statement.  All
        // layer-creating selectors in a statement are aggregated into ONE
        // merged layer (`aggregate_layer_spec`) with ONE `truncated` flag, so
        // truncation can't be attributed to a specific selector — emitting one
        // warning per layer-spec selector would over-report a single event.
        // Use the first layer-spec selector's warning (its span/wording); cache
        // hits and misses both surface it since the persistent
        // `layers.truncated` flag is read on both paths.
        if truncated_any {
            if let Some(w) = self
                .selectors()
                .filter(|s| s.has_layer_spec())
                .find_map(|s| s.make_truncation_warning())
            {
                outcome.warnings.push(w);
            }
        }
        Ok(outcome)
    }

    /// Phase R: build each selector's selection — pure reads.  The layers in
    /// `layer_ids` were materialised by this statement's Phase M, and `eph`
    /// already contains the tree's single materialisation (tree-complete
    /// visibility),
    /// so neighbourhood queries see sibling/child layers symmetrically.
    /// `phase1_warnings` carries Phase M's truncation notices into this
    /// statement's `ComputeResult`.  Since ScopeContext contains non-clonable
    /// Box<dyn>, the caller builds the scopes and hands them in.
    pub async fn compute_selection(
        &self,
        cfg: &ControlFlowGraph,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
        layer_ids: &[i64],
        phase1_warnings: Vec<Diagnostic>,
        probe_ids: Option<Vec<i64>>,
    ) -> Result<ComputeResult, pest::error::Error<Rule>> {
        let selectors: Vec<&dyn Selector> = self.selectors().collect();

        // Nothing to do
        if selectors.is_empty() {
            return Ok(ComputeResult {
                selections: Vec::new(),
                warnings: Vec::new(),
            });
        }

        let mut warnings = phase1_warnings;
        let mut selections = vec![];

        let to_pest = |e: anyhow::Error| {
            pest::error::Error::new_from_span(
                pest::error::ErrorVariant::CustomError {
                    message: e.to_string(),
                },
                self.span().as_pest_span(),
            )
        };

        // Layer-aware selectors (those whose `has_layer_spec()` was true)
        // read from the Phase-M-materialised layer's contents; all other
        // selectors go through `select_from_all_impl`.  Both paths apply
        // the command's composite filter — a statement's filters constrain
        // its layer reads exactly like its persistent reads.
        let filter_parts: Vec<CompositeFilter> = self
            .filters()
            .filter_map(|f| f.get_composite_filter(eph))
            .collect();

        let mut budget_warned = false;
        // Plain predicate-only selectors fold into ONE statement query (the
        // fused block below); layer-backed and side-effectful selectors keep
        // their own paths.
        let mut fused: Vec<&dyn Selector> = Vec::new();
        for selector in selectors.into_iter() {
            if !selector.has_layer_spec() && selector.fusable() {
                fused.push(selector);
                continue;
            }
            let mut current_selection = if selector.has_layer_spec() {
                // Layer-aware selector: return the union of all rows in this
                // statement's materialised layer.  For single-verb statements
                // this is exactly the selector's own contribution (today's
                // behaviour).  For multi-verb statements every layer-aware
                // selector returns the same combined view — the command-level
                // OR across selectors dedupes the union into one selection.
                // Union read across the statement's materialised layers
                // (all roots' root shards ∪ layer shards ∪ selection shards)
                // — the composition point
                // where future mask rows will subtract.  Empty only under a
                // zero-root context (no project exists for ephemeral rows
                // to attach to): nothing was materialised, so the selector
                // contributes nothing.
                if layer_ids.is_empty() {
                    // Zero-root context: the per-root loop ran no populate,
                    // so the verb's own user-facing errors (loc's "no file
                    // matching", …) never had a chance to fire.  Surface an
                    // explicit warning instead of an empty result that is
                    // indistinguishable from a successful non-match.
                    warnings.push(Diagnostic::note(
                        Span::from_pest(selector.span(), self.span().input()),
                        format!(
                            "'{}' produced no layers: no projects are visible \
                             (is the index empty?)",
                            selector.name()
                        ),
                    ));
                    selections.push((selector.id(), None));
                    continue;
                }
                let instance_ids = cfg
                    .index
                    .get_eph_instance_ids_for_layers(layer_ids)
                    .await
                    .map_err(to_pest)?;
                if instance_ids.is_empty() {
                    None
                } else {
                    let ids: Vec<_> = instance_ids
                        .into_iter()
                        .map(SymbolInstanceId::new)
                        .collect();
                    let mut parts = filter_parts.clone();
                    parts.push(CompositeFilter::leaf(SymbolInstanceIdMixin::new(&ids)));
                    let filter = CompositeFilter::and(parts);
                    Some(
                        cfg.index
                            .find_symbol(&filter, parent_scope.clone(), children_scope.clone(), eph)
                            .await
                            .map_err(to_pest)?
                            .into_inner(),
                    )
                }
            } else {
                // Normal selector: query via select_from_all_impl with the
                // command's composite filter.
                let filter = CompositeFilter::and(filter_parts.clone());
                let select_from_all_name = format!("{:?}", selector);
                let _select_from_all =
                    tracing::debug_span!("select_from_all", name = %select_from_all_name).entered();
                selector
                    .select_from_all_impl(
                        cfg,
                        filter,
                        parent_scope.clone(),
                        children_scope.clone(),
                        eph,
                    )
                    .await
                    .map_err(to_pest)?
            };

            if let Some(selection) = &mut current_selection {
                // Budget truncation must SURFACE: a leaf query hit the
                // request's result-budget LIMIT, so this selection may be
                // missing rows.  One warning per statement, attributed to the
                // selector that produced the bounded selection.
                if selection.budget_bounded && !budget_warned {
                    budget_warned = true;
                    let bound = eph.result_budget().leaf_limit().unwrap_or(0);
                    warnings.push(Diagnostic::truncation(
                        Span::from_pest(selector.span(), self.span().input()),
                        format!(
                            "results were bounded to {bound} rows by the server \
                             result budget and may be incomplete — narrow the \
                             query or raise the request `limit`"
                        ),
                    ));
                }
                if selection.is_empty() {
                    warnings.push(Diagnostic::no_match(
                        Span::from_pest(selector.span(), self.span().input()),
                        selector.matched_token(),
                    ));
                }
            }
            selections.push((selector.id(), current_selection));
        }

        // Fused emission: one find_symbol over filters ∧ OR(own predicates);
        // the shared selection is stored under every participating selector
        // (`Selection::extend` is idempotent, so the statement-level OR fold
        // dedups the shared rows).
        //
        // A wave-0 probe that RESOLVED this statement already holds its
        // exact current-instance set — the id leaf is ANDed in, driving the
        // current family through the pkey.  The predicate stays: filter
        // leaves may also constrain the neighbourhood families
        // (compose_parents/compose_children), and those contributions must
        // be byte-identical to the unprobed emission.
        if !fused.is_empty() {
            let mut filter = fuse_predicate(filter_parts.clone(), &fused, eph);
            if let Some(ids) = &probe_ids {
                let ids: Vec<SymbolInstanceId> =
                    ids.iter().map(|id| SymbolInstanceId::new(*id)).collect();
                filter = CompositeFilter::and(vec![
                    filter,
                    CompositeFilter::leaf(SymbolInstanceIdMixin::new(&ids)),
                ]);
            }
            let _fused_span = tracing::debug_span!("select_fused", n = fused.len()).entered();
            let selection = cfg
                .index
                .find_symbol(&filter, parent_scope.clone(), children_scope.clone(), eph)
                .await
                .map_err(to_pest)?
                .into_inner();

            if selection.budget_bounded && !budget_warned {
                let bound = eph.result_budget().leaf_limit().unwrap_or(0);
                warnings.push(Diagnostic::truncation(
                    Span::from_pest(fused[0].span(), self.span().input()),
                    format!(
                        "results were bounded to {bound} rows by the server \
                         result budget and may be incomplete — narrow the \
                         query or raise the request `limit`"
                    ),
                ));
            }

            if selection.is_empty() {
                for sel in &fused {
                    warnings.push(Diagnostic::no_match(
                        Span::from_pest(sel.span(), self.span().input()),
                        sel.matched_token(),
                    ));
                }
            } else if fused.len() > 1 {
                // Per-selector no-match attribution: the shared result cannot
                // say which OR branch matched nothing, but a LIMIT-1
                // existence check per constrained branch can (cached,
                // current-only — the same emptiness the branch's own query
                // would have reported).  Unconstrained branches match
                // whenever the statement result is non-empty.
                for sel in &fused {
                    if let OwnPredicate::Filter(Some(own)) = sel.own_predicate(eph) {
                        let mut probe_parts = filter_parts.clone();
                        probe_parts.push(own);
                        let probe = CompositeFilter::and(probe_parts);
                        if !cfg
                            .index
                            .has_symbol_matching(&probe, eph)
                            .await
                            .map_err(to_pest)?
                        {
                            warnings.push(Diagnostic::no_match(
                                Span::from_pest(sel.span(), self.span().input()),
                                sel.matched_token(),
                            ));
                        }
                    }
                }
            }

            for sel in &fused {
                selections.push((sel.id(), Some(selection.clone())));
            }
        }
        Ok(ComputeResult {
            selections,
            warnings,
        })
    }
}

pub struct LabeledStatements(HashMap<String, Vec<Rc<Statement>>>);

impl LabeledStatements {
    pub fn new() -> Self {
        Self(HashMap::new())
    }

    pub fn remember(&mut self, statement: Rc<Statement>) -> usize {
        let marks = statement.command().get_labels();
        let marks_len = marks.len();
        for mark in marks {
            self.0
                .entry(mark)
                .or_insert_with(Vec::new)
                .push(statement.clone());
        }

        marks_len
    }

    pub fn get_statements(&self, label: &str) -> Option<&Vec<Rc<Statement>>> {
        self.0.get(label)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn noop_root_shard() -> RootShardPopulate {
        Box::new(|_txn, _root| Box::pin(async { Ok(false) }))
    }

    fn noop_selection_shard() -> SelectionShardPopulate {
        Box::new(|_txn, _root, _root_shard_ref| Box::pin(async { Ok(false) }))
    }

    /// A content-scan-less part with an empty `selection_extra` and no
    /// layer-shard populate (`None`) — the shape a lone `layer { … }` with only
    /// root-shard ops produces. (A real `search()` differs: it sets
    /// `layer_shard_populate: Some(..)` via `LayerSpec::sharded_scan`.)
    fn empty_part() -> LayerSpec {
        LayerSpec {
            input_hash: [0u8; 32],
            root_shard_populate: noop_root_shard(),
            selection_shard_populate: noop_selection_shard(),
            layer_shard_populate: None,
            selection_extra: Vec::new(),
        }
    }

    /// A part carrying real selection-shard inputs, like a `layer { … }` that
    /// references ephemeral ids.
    fn part_with_selection_extra(extra: Vec<u8>) -> LayerSpec {
        LayerSpec {
            input_hash: [0u8; 32],
            root_shard_populate: noop_root_shard(),
            selection_shard_populate: noop_selection_shard(),
            layer_shard_populate: None,
            selection_extra: extra,
        }
    }

    #[test]
    fn composite_of_empty_selection_extras_stays_empty() {
        // `search("A") search("B")`: both parts are persistent-only, so the
        // composite must carry NO selection-shard key — otherwise the
        // empty-chain guard in `compute_selected` rejects it as
        // "selection-shard inputs but no ephemeral context". Regression test
        // for that internal error.
        let composite = Command::partitioned_composite(vec![empty_part(), empty_part()]);
        assert!(
            composite.selection_extra.is_empty(),
            "two persistent-only parts must not fabricate a selection-shard key"
        );
    }

    #[test]
    fn composite_with_a_real_selection_extra_is_nonempty_and_order_distinct() {
        // If any part has selection-shard inputs, the composite keeps a
        // (32-byte) selection-shard key that still distinguishes source order.
        let a = Command::partitioned_composite(vec![
            empty_part(),
            part_with_selection_extra(vec![1, 2, 3]),
        ]);
        assert!(!a.selection_extra.is_empty());
        let b = Command::partitioned_composite(vec![
            part_with_selection_extra(vec![1, 2, 3]),
            empty_part(),
        ]);
        assert!(!b.selection_extra.is_empty());
        assert_ne!(
            a.selection_extra, b.selection_extra,
            "selection-shard key must fold parts in source order"
        );
    }
}
