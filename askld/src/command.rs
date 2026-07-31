use crate::cfg::ControlFlowGraph;
use crate::diagnostic::Diagnostic;
use crate::execution_context::{selector_state_with, ExecutionContext};
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::Rule;
use crate::span::Span;
use crate::statement::Statement;
use crate::verb::{
    add_verb, find_symbol_by_instance_id, ConstraintAction, DeriveMethod, Filter, LabelResolutions,
    Labeler, LayerPopulate, LayerSpec, NotificationContext, Selector, SelectorId,
    SupplementPopulate, Verb, VerbTag,
};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{
    CompositeFilter, EphContext, EphLayerKind, Index, InnermostOnlyMixin, ScopeContext, Selection,
    SymbolInstanceIdMixin,
};
use index::symbols::SymbolInstanceId;
use sha2::{Digest, Sha256};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Which half of a root's cache entry a [`LayerActivation`] refers to —
/// re-exported from the index crate, which produces the layers in this
/// exact role order.
pub use index::db_diesel::LayerRole;

/// One ephemeral-layer touch by a statement: either the layer was freshly
/// created and populated in this call (`created = true`), or the statement
/// hit an existing cache row (`created = false`).  `truncated` mirrors the
/// persistent `layers.truncated` flag as observed by this call.
///
/// Recorded on [`ExecutionContext::layer_activations`] so tests can assert
/// cache behaviour (populate vs reuse) directly instead of inferring it
/// from eph instance IDs.  A layer-creating statement records activations
/// per visible root, roots ascending; within a root, base first, then
/// supplement.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerActivation {
    /// The root whose chain this layer joined.
    pub root_id: i64,
    pub layer_id: i64,
    pub created: bool,
    pub truncated: bool,
    pub role: LayerRole,
}

/// Result of computing initial selections for a statement's selectors.
pub struct ComputeResult {
    pub selections: Vec<(SelectorId, Option<Selection>)>,
    pub warnings: Vec<Diagnostic>,
    pub layer_activations: Vec<LayerActivation>,
}

impl ComputeResult {
    pub fn apply(self, ctx: &mut ExecutionContext, statement: &Statement) {
        for (id, selection) in self.selections {
            ctx.registry.add_by_id(id, selection);
        }
        statement.get_state_mut().warnings.extend(self.warnings);
        // Activations are the single source of truth for chain growth: one
        // statement's layers join the chains as ONE atomic round (lockstep
        // by construction — see EphContext::push_round).
        if !self.layer_activations.is_empty() {
            let round: Vec<(i64, i64)> = self
                .layer_activations
                .iter()
                .map(|a| (a.root_id, a.layer_id))
                .collect();
            ctx.eph.push_round(&round);
        }
        ctx.layer_activations.extend(self.layer_activations);
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

#[derive(Debug, Default)]
pub struct Command {
    verbs: Vec<Arc<dyn Verb>>,
    span: Option<Span>,
    verb_span: Option<Span>,
}

impl Command {
    pub fn new(span: Span) -> Command {
        Self {
            verbs: vec![],
            span: Some(span),
            verb_span: None,
        }
    }

    pub fn derive(&self, span: Span) -> Self {
        let mut verbs = vec![];
        for verb in self.verbs.iter() {
            match verb.derive_method() {
                DeriveMethod::Clone => {
                    // Use derive_new_instance if available to create an independent
                    // copy, avoiding shared registry state between parent and child.
                    let derived = verb.derive_new_instance().unwrap_or_else(|| verb.clone());
                    verbs.push(derived);
                }
                DeriveMethod::Skip => {}
            }
        }

        Self {
            verbs: verbs,
            span: Some(span),
            verb_span: None,
        }
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
        let verbs = std::mem::take(&mut self.verbs);
        self.verbs = add_verb(verbs, other);
    }

    pub(crate) fn filters<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Filter> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_filter().ok()))
    }

    pub fn selectors<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Selector> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_selector().ok()))
    }

    /// Check if any verb suppresses the default type filter.
    pub fn has_suppress_default_type_filter(&self) -> bool {
        self.verbs
            .iter()
            .any(|v| v.suppresses_default_type_filter())
    }

    pub fn has_selectors(&self) -> bool {
        self.verbs.iter().any(|verb| verb.as_selector().is_ok())
    }

    /// Whether any selector in this command materializes an ephemeral layer.
    /// Layer-creating commands are implicit barriers: the statement executor
    /// drains pending statements before this one so the layer's `parent_id`
    /// and the visible `eph_ids` reflect prior side effects in order.
    pub fn has_layer_spec(&self) -> bool {
        self.selectors().any(|s| s.has_layer_spec())
    }

    /// Check if any verb has the given tag.
    pub fn has_verb_tag(&self, tag: &VerbTag) -> bool {
        self.verbs.iter().any(|v| v.get_tag().as_ref() == Some(tag))
    }

    pub fn is_unit(&self) -> bool {
        self.selectors().all(|verb| verb.is_unit())
    }

    /// Whether all selectors in this command are non-constraining.
    pub fn is_non_constraining(&self) -> bool {
        self.selectors()
            .all(|verb| verb.is_non_constraining_selector())
    }

    fn labels<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Labeler> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_labeler().ok()))
    }

    pub fn get_labels(&self) -> Vec<String> {
        self.labels().flat_map(|m| m.get_label()).collect()
    }

    /// Build a composite filter from all selectors (ORed across selectors).
    /// Used by scope builders to construct ScopeContext.
    pub fn get_selector_composite_filter(&self, eph: &EphContext) -> Option<CompositeFilter> {
        let parts: Vec<_> = self
            .selectors()
            .filter_map(|sel| sel.build_composite_filter(self, eph))
            .collect();
        match parts.len() {
            0 => None,
            1 => parts.into_iter().next(),
            _ => Some(CompositeFilter::or(parts)),
        }
    }

    pub fn filter(&self, selection: &mut Selection) {
        let _command_filter: tracing::span::EnteredSpan =
            tracing::debug_span!("command_filter").entered();
        for verb in self.filters() {
            verb.filter(selection);
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
    ) -> Result<Option<LayerSpec>> {
        let mut specs: Vec<LayerSpec> = Vec::new();
        for selector in self.selectors() {
            if let Some(spec) = selector
                .layer_spec(cfg, eph, composite_filter, resolved)
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

    /// Merge ≥2 specs into one composite: the base halves combine into one
    /// composite base (keyed on the part base hashes in source order —
    /// still eph-independent), the supplement halves into one composite
    /// supplement.  Truncation ORs across parts per half.
    fn partitioned_composite(parts: Vec<LayerSpec>) -> LayerSpec {
        let mut h = Sha256::new();
        h.update(b"composite-base-v1");
        for p in &parts {
            h.update(p.base_hash);
        }
        let base_hash: [u8; 32] = h.finalize().into();

        tracing::debug!(
            composite_base_hash = ?&base_hash[..8],
            parts = ?parts
                .iter()
                .map(|p| (p.base_kind, &p.base_hash[..8]))
                .collect::<Vec<_>>(),
            "partitioned composite layer synthesised",
        );

        // Composite supplement key: fold every part's extra (length-prefixed,
        // source order) so parts that differ only in their supplement inputs
        // produce distinct composite supplements.
        let mut he = Sha256::new();
        he.update(b"composite-extra-v1");
        for p in &parts {
            he.update((p.supplement_extra.len() as u64).to_le_bytes());
            he.update(&p.supplement_extra);
        }
        let supplement_extra: Vec<u8> = he.finalize().to_vec();

        let mut base_populates = Vec::with_capacity(parts.len());
        let mut supplement_populates = Vec::with_capacity(parts.len());
        for p in parts {
            base_populates.push(p.base_populate);
            supplement_populates.push(p.supplement_populate);
        }

        // `Fn` closures run once per root; the parts are shared across those
        // runs via `Arc` (the returned future cannot borrow the closure's
        // environment under the HRTB, so each call clones the handle in).
        let base_populates = std::sync::Arc::new(base_populates);
        let supplement_populates = std::sync::Arc::new(supplement_populates);

        let base_populate: LayerPopulate = Box::new(move |txn, root| {
            let populates = std::sync::Arc::clone(&base_populates);
            Box::pin(async move {
                let mut truncated = false;
                for populate in populates.iter() {
                    truncated |= populate(&mut *txn, root).await?;
                }
                Ok(truncated)
            })
        });
        let supplement_populate: SupplementPopulate = Box::new(move |txn, root, base_ref| {
            let populates = std::sync::Arc::clone(&supplement_populates);
            Box::pin(async move {
                let mut truncated = false;
                for populate in populates.iter() {
                    truncated |= populate(&mut *txn, root, base_ref).await?;
                }
                Ok(truncated)
            })
        });

        LayerSpec {
            base_hash,
            base_kind: EphLayerKind::Composite,
            base_populate,
            supplement_populate,
            supplement_extra,
        }
    }

    /// Notify all selectors using a pre-built merged selection (constraint + derivation).
    /// Used when a parent is notified by the union of all its children's selections.
    pub async fn notify_from_selection(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        dependency: &Selection,
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

            let mut selection = find_symbol_by_instance_id(
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

            selector_filters.iter().for_each(|f| {
                f.filter(&mut selection);
            });

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
            let mut selection = match notif_ctx.role {
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
                DependencyRole::Parent => {
                    selector
                        .derive_from_child(
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
                // PreSeed* notifications never reach this dispatch —
                // they're short-circuited to a no-op in
                // `Statement::notify`.  If they somehow do, treat as a
                // no-op (no selection produced).
                DependencyRole::PreSeedSibling | DependencyRole::PreSeedLabel(_) => Ok(None),
            }
            .map_err(|e| {
                pest::error::Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!("Failed to derive selection: {}", e),
                    },
                    selector.span(),
                )
            })?;

            if let Some(ref mut sel) = selection {
                selector_filters.iter().for_each(|f| {
                    f.filter(sel);
                });
            }

            selector_state_with(&mut ctx.registry, selector, |state| {
                state.selection = selection;
            });
            changed = true;
        }
        Ok(NotificationResult::new(changed, warnings))
    }

    /// Computes the selected symbols based on the selectors defined in the
    /// command. This method returns an `Option<SymbolInstanceRefs>`, which will be
    /// `None` if no symbols are selected. It returns
    /// `Some(SymbolInstanceRefs::new())` if no symbols match the selectors.
    /// Scope-building data for parent/children scoping. Since ScopeContext
    /// contains non-clonable Box<dyn>, we store the raw data and rebuild
    /// ScopeContext for each selector.
    pub async fn compute_selected(
        &self,
        cfg: &ControlFlowGraph,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
        resolved: &LabelResolutions,
    ) -> Result<ComputeResult, pest::error::Error<Rule>> {
        let selectors: Vec<&dyn Selector> = self.selectors().collect();

        // Nothing to do
        if selectors.is_empty() {
            return Ok(ComputeResult {
                selections: Vec::new(),
                warnings: Vec::new(),
                layer_activations: Vec::new(),
            });
        }

        let mut warnings = vec![];
        let mut selections = vec![];
        let mut layer_activations = vec![];

        // Validate: each selector that requires a name constraint must have one
        // command-wide (any filter verb on the command counts).
        for selector in selectors.iter() {
            if !selector.requires_name_constraint() {
                continue;
            }
            let has_name = self.verbs.iter().any(|v| v.has_name_constraint());
            if !has_name {
                warnings.push(Diagnostic::note(
                    Span::from_pest(selector.span(), self.span().input()),
                    "select requires at least one name filter (filter(\"compound_name\", ...) or filter(\"exact_name\", ...))",
                ));
            }
        }

        let to_pest = |e: anyhow::Error| {
            pest::error::Error::new_from_span(
                pest::error::ErrorVariant::CustomError {
                    message: e.to_string(),
                },
                self.span().as_pest_span(),
            )
        };

        // Phase 1: materialise this statement's single ephemeral layer (if
        // any of its verbs contribute one).  Multi-verb statements get a
        // `Composite` layer that combines every verb's contribution; the
        // aggregation is in `Command::aggregate_layer_spec`.
        //
        // The CompositeFilter is built here (once per statement) so it can
        // be passed both to `layer_spec` (filter-aware selectors mix it into
        // their cache key and apply its `compose_objects` to scope their
        // queries) and to the Phase 2 `select_from_all_impl` calls below.
        let mut local_eph = eph.clone();
        let filter_parts_for_layer: Vec<CompositeFilter> = self
            .filters()
            .filter_map(|f| f.get_composite_filter(&local_eph))
            .collect();
        let layer_composite_filter = CompositeFilter::and(filter_parts_for_layer);

        let (materialised_layer_ids, any_truncated): (Vec<i64>, bool) = match self
            .aggregate_layer_spec(cfg, &local_eph, &layer_composite_filter, resolved)
            .await
            .map_err(to_pest)?
        {
            None => (Vec::new(), false),
            Some(spec) => {
                // Defense in depth, uniform across verbs: non-empty
                // supplement inputs under an empty chain means the verb
                // computed eph-derived content that no supplement will ever
                // materialize — those rows would be silently dropped.  The
                // verb-side guards (with better spans) should have caught
                // this; reaching here is an internal invariant violation.
                if !local_eph.has_chain() && !spec.supplement_extra.is_empty() {
                    return Err(to_pest(anyhow::anyhow!(
                        "internal error: layer spec carries supplement inputs \
                         but no ephemeral context exists before this statement"
                    )));
                }

                // Chain topology is decided here, not by the verb: per
                // visible root, the base parents on the root and the
                // supplement (created iff that root's chain is non-empty)
                // chains on the root's `chain_last`.  The base hash is
                // salted per root (`root_salted_hash`), so each root's
                // cache entry is independent of co-visible projects.
                let results = cfg
                    .index
                    .with_partitioned_layers(
                        &local_eph,
                        &spec.base_hash,
                        spec.base_kind,
                        &spec.supplement_extra,
                        spec.base_populate,
                        spec.supplement_populate,
                    )
                    .await
                    .map_err(to_pest)?;

                let mut ids = Vec::with_capacity(results.len());
                let mut hit_ids = Vec::with_capacity(results.len());
                let mut round = Vec::with_capacity(results.len());
                let mut truncated_any = false;
                // Results arrive in recording order (roots ascending; base
                // before supplement within a root), so the root's
                // `chain_last` ends up the supplement and downstream
                // chaining keys keep depending on that root's full
                // upstream chain.
                for layer in results {
                    if !layer.outcome.created {
                        hit_ids.push(layer.outcome.layer_id);
                    }
                    layer_activations.push(LayerActivation {
                        root_id: layer.root_id,
                        layer_id: layer.outcome.layer_id,
                        created: layer.outcome.created,
                        truncated: layer.outcome.truncated,
                        role: layer.role,
                    });
                    truncated_any |= layer.outcome.truncated;
                    round.push((layer.root_id, layer.outcome.layer_id));
                    ids.push(layer.outcome.layer_id);
                }
                // One atomic round: this statement's layers join every
                // root's chain together (Phase 2 below reads via local_eph).
                local_eph.push_round(&round);
                // One LRU touch round trip for all roots' hits.
                let _ = cfg.index.touch_eph_layers(&hit_ids).await;
                (ids, truncated_any)
            }
        };

        // Surface truncation warnings.  Each layer-creating selector
        // contributes a warning shaped by its own span; cache hits and
        // misses both surface the warning since the persistent
        // `layers.truncated` flag is read on both paths (for a
        // partitioned entry, on either of its rows).
        if any_truncated {
            for selector in self.selectors() {
                if !selector.has_layer_spec() {
                    continue;
                }
                if let Some(w) = selector.make_truncation_warning() {
                    warnings.push(w);
                }
            }
        }

        // Phase 2: build each selector's selection.  Layer-aware selectors
        // (those whose `has_layer_spec()` was true) read from the
        // freshly-materialised layer's contents.  All other selectors go
        // through `select_from_all_impl` with the command's composite
        // filter, as before.
        let filter_parts: Vec<CompositeFilter> = self
            .filters()
            .filter_map(|f| f.get_composite_filter(&local_eph))
            .collect();

        for selector in selectors.into_iter() {
            let mut current_selection = if selector.has_layer_spec() {
                // Layer-aware selector: return the union of all rows in this
                // statement's materialised layer.  For single-verb statements
                // this is exactly the selector's own contribution (today's
                // behaviour).  For multi-verb statements every layer-aware
                // selector returns the same combined view — the command-level
                // OR across selectors dedupes the union into one selection.
                // Union read across the statement's materialised layers
                // (all roots' bases ∪ supplements) — the composition point
                // where future mask rows will subtract.  Empty only under a
                // zero-root context (no project exists for ephemeral rows
                // to attach to): nothing was materialised, so the selector
                // contributes nothing.
                if materialised_layer_ids.is_empty() {
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
                    .get_eph_instance_ids_for_layers(&materialised_layer_ids)
                    .await
                    .map_err(to_pest)?;
                if instance_ids.is_empty() {
                    None
                } else {
                    let ids: Vec<_> = instance_ids
                        .into_iter()
                        .map(SymbolInstanceId::new)
                        .collect();
                    let filter = CompositeFilter::leaf(SymbolInstanceIdMixin::new(&ids));
                    Some(
                        cfg.index
                            .find_symbol(
                                &filter,
                                parent_scope.clone(),
                                children_scope.clone(),
                                &local_eph,
                            )
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
                        &local_eph,
                    )
                    .await
                    .map_err(to_pest)?
            };

            if let Some(selection) = &mut current_selection {
                self.filter(selection);
                if selection.is_empty() {
                    warnings.push(Diagnostic::no_match(
                        Span::from_pest(selector.span(), self.span().input()),
                        selector.matched_token(),
                    ));
                }
            }
            selections.push((selector.id(), current_selection));
        }
        Ok(ComputeResult {
            selections,
            warnings,
            layer_activations,
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
