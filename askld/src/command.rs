use crate::cfg::ControlFlowGraph;
use crate::execution_context::{selector_state_with, ExecutionContext};
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::Rule;
use crate::span::Span;
use crate::statement::Statement;
use crate::verb::{add_verb, ConstraintAction, DeriveMethod, Filter, Labeler, LabelResolutions, LayerPopulate, LayerSpec, NotificationContext, Selector, SelectorId, Verb, VerbTag, find_symbol_by_instance_id};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{CompositeFilter, EphContext, EphLayerKind, InnermostOnlyMixin, Index, ScopeContext, Selection, SymbolInstanceIdMixin};
use sha2::{Digest, Sha256};
use index::symbols::SymbolInstanceId;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

/// Result of computing initial selections for a statement's selectors.
pub struct ComputeResult {
    pub selections: Vec<(SelectorId, Option<Selection>)>,
    pub warnings: Vec<pest::error::Error<Rule>>,
    pub new_eph_ids: Vec<i64>,
}

impl ComputeResult {
    pub fn apply(self, ctx: &mut ExecutionContext, statement: &Statement) {
        for (id, selection) in self.selections {
            ctx.registry.add_by_id(id, selection);
        }
        statement.get_state_mut().warnings.extend(self.warnings);
        ctx.eph.extend(self.new_eph_ids);
    }
}

pub struct NotificationResult {
    pub changed: bool,
    pub warnings: Vec<pest::error::Error<Rule>>,
}

impl NotificationResult {
    pub fn new(changed: bool, warnings: Vec<pest::error::Error<Rule>>) -> Self {
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
        self.verb_span.as_ref().unwrap_or_else(|| self.span.as_ref().unwrap())
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
        self.verbs.iter().any(|v| v.suppresses_default_type_filter())
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
        let parts: Vec<_> = self.selectors()
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
    /// Labels referenced by any layer-creating verb in this command.
    /// Used by `build_dependency_graph` to add User edges so the
    /// labelled statements run before this command's layer materialises.
    ///
    /// Iterates *all* verbs (not just selectors) so any future verb kind
    /// — including non-selector verbs that grow ephemeral semantics —
    /// can contribute label refs without changes here.  The default
    /// `Verb::layer_label_refs()` returns an empty `Vec`, so the
    /// iteration cost is negligible for the common no-label case.
    pub fn layer_label_refs(&self) -> Vec<String> {
        self.verbs.iter().flat_map(|v| v.layer_label_refs()).collect()
    }

    pub async fn aggregate_layer_spec(
        &self,
        cfg: &ControlFlowGraph,
        eph: &EphContext,
        resolved: &LabelResolutions,
    ) -> Result<Option<LayerSpec>> {
        let mut specs: Vec<LayerSpec> = Vec::new();
        for selector in self.selectors() {
            if let Some(spec) = selector.layer_spec(cfg, eph, resolved).await? {
                specs.push(spec);
            }
        }
        match specs.len() {
            0 => Ok(None),
            1 => Ok(Some(specs.into_iter().next().unwrap())),
            _ => {
                let parent_id = specs[0].parent_id;
                // Invariant: every spec was built from the same `eph`
                // snapshot inside `compute_selected`, so they all derive
                // `parent_id = eph.last()`.  Catch any future selector
                // that breaks this in dev — silent inheritance from
                // `specs[0]` would silently misattribute the composite.
                debug_assert!(
                    specs.iter().all(|s| s.parent_id == parent_id),
                    "all specs in a composite must share parent_id; \
                     got {:?} from kinds {:?}",
                    specs.iter().map(|s| s.parent_id).collect::<Vec<_>>(),
                    specs.iter().map(|s| s.kind).collect::<Vec<_>>(),
                );
                let mut h = Sha256::new();
                h.update(EphLayerKind::Composite.as_str().as_bytes());
                for spec in &specs {
                    h.update(spec.hash);
                }
                let composite_hash: [u8; 32] = h.finalize().into();

                // Diagnostic crumb: composite layers show up in
                // `eph_layers.kind = 'composite'` without any structured
                // record of which verbs contributed.  Log the contributing
                // (kind, hash-prefix) pairs so an operator debugging a
                // composite row can `grep` for it and find this line.
                tracing::debug!(
                    composite_hash = ?&composite_hash[..8],
                    parts = ?specs
                        .iter()
                        .map(|s| (s.kind, &s.hash[..8]))
                        .collect::<Vec<_>>(),
                    "composite layer synthesised",
                );

                let populate: LayerPopulate = Box::new(move |txn| {
                    Box::pin(async move {
                        for spec in specs {
                            (spec.populate)(txn).await?;
                        }
                        Ok(())
                    })
                });
                Ok(Some(LayerSpec {
                    hash: composite_hash,
                    kind: EphLayerKind::Composite,
                    parent_id,
                    populate,
                }))
            }
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
            let span = selector.span();
            let (constrained, sel_changed, sel_warnings) = selector_state_with(&mut ctx.registry, selector, |state| {
                state.constrain_with_warning(dependency, role, rel_type, span, "children")
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
                    find_parts.push(CompositeFilter::leaf(InnermostOnlyMixin::new(&ctx.eph)));
                }
                let find_filter = CompositeFilter::and(find_parts);
                derivation_ids = Some(index.find_parent_instance_ids(
                    &child_ids,
                    rel_type.contains(RelationshipType::REFS),
                    rel_type.contains(RelationshipType::HAS),
                    &find_filter,
                    &ctx.eph,
                ).await.map_err(|e| {
                    pest::error::Error::new_from_span(
                        pest::error::ErrorVariant::CustomError {
                            message: format!("Failed to find parent instance IDs: {}", e),
                        },
                        selector.span(),
                    )
                })?);
            }
            let decl_ids = derivation_ids.as_ref().unwrap();

            let mut selection = find_symbol_by_instance_id(index, &selector_filters, decl_ids, parent_scope.clone(), children_scope.clone(), &ctx.eph)
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

            match selector.try_constrain_notification(&mut ctx.registry, &dependency, notif_ctx, notifier)? {
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
                    selector.derive_from_parent(ctx, index, &selector_filters, notifier, notif_ctx, parent_scope.clone(), children_scope.clone())
                        .await
                }
                DependencyRole::Parent => {
                    selector.derive_from_child(ctx, index, &selector_filters, notifier, notif_ctx, parent_scope.clone(), children_scope.clone())
                        .await
                }
                DependencyRole::User => {
                    selector.derive_from_provider(ctx, index, &selector_filters, notifier)
                        .await
                }
                // Sibling notifications never reach this dispatch — they're
                // short-circuited to a no-op in `Statement::notify`.  If they
                // somehow do, treat as a no-op (no selection produced).
                DependencyRole::Sibling => Ok(None),
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
                new_eph_ids: Vec::new(),
            });
        }

        let mut warnings = vec![];
        let mut selections = vec![];
        let mut new_eph_ids = vec![];

        // Validate: each selector that requires a name constraint must have one
        // command-wide (any filter verb on the command counts).
        for selector in selectors.iter() {
            if !selector.requires_name_constraint() {
                continue;
            }
            let has_name = self.verbs.iter().any(|v| v.has_name_constraint());
            if !has_name {
                warnings.push(pest::error::Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: "select requires at least one name filter (filter(\"compound_name\", ...) or filter(\"exact_name\", ...))".to_string(),
                    },
                    selector.span(),
                ));
            }
        }

        let to_pest = |e: anyhow::Error| pest::error::Error::new_from_span(
            pest::error::ErrorVariant::CustomError { message: e.to_string() },
            self.span().as_pest_span(),
        );

        // Phase 1: materialise this statement's single ephemeral layer (if
        // any of its verbs contribute one).  Multi-verb statements get a
        // `Composite` layer that combines every verb's contribution; the
        // aggregation is in `Command::aggregate_layer_spec`.
        let mut local_eph = eph.clone();
        let materialised_layer_id: Option<i64> = if let Some(spec) =
            self.aggregate_layer_spec(cfg, &local_eph, resolved).await.map_err(to_pest)?
        {
            let (layer_id, created, _) = cfg.index.with_eph_layer(
                spec.parent_id, &spec.hash, spec.kind,
                |txn| if txn.created() {
                    (spec.populate)(txn)
                } else {
                    Box::pin(async { Ok(()) })
                },
            ).await.map_err(to_pest)?;

            if !created {
                let _ = cfg.index.touch_eph_layer(layer_id).await;
            }
            new_eph_ids.push(layer_id);
            local_eph.push(layer_id);
            Some(layer_id)
        } else {
            None
        };

        // Phase 2: build each selector's selection.  Layer-aware selectors
        // (those whose `has_layer_spec()` was true) read from the
        // freshly-materialised layer's contents.  All other selectors go
        // through `select_from_all_impl` with the command's composite
        // filter, as before.
        let filter_parts: Vec<CompositeFilter> = self.filters()
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
                let layer_id = materialised_layer_id
                    .expect("has_layer_spec=true implies aggregate_layer_spec returned Some");
                let instance_ids = cfg.index.get_eph_instance_ids_for_layer(layer_id)
                    .await.map_err(to_pest)?;
                if instance_ids.is_empty() {
                    None
                } else {
                    let ids: Vec<_> = instance_ids.into_iter().map(SymbolInstanceId::new).collect();
                    let filter = CompositeFilter::leaf(SymbolInstanceIdMixin::new(&ids));
                    Some(cfg.index.find_symbol(
                        &filter, parent_scope.clone(), children_scope.clone(), &local_eph
                    ).await.map_err(to_pest)?.into_inner())
                }
            } else {
                // Normal selector: query via select_from_all_impl with the
                // command's composite filter.
                let filter = CompositeFilter::and(filter_parts.clone());
                let select_from_all_name = format!("{:?}", selector);
                let _select_from_all =
                    tracing::debug_span!("select_from_all", name = %select_from_all_name).entered();
                selector
                    .select_from_all_impl(cfg, filter, parent_scope.clone(), children_scope.clone(), &local_eph)
                    .await
                    .map_err(to_pest)?
            };

            if let Some(selection) = &mut current_selection {
                self.filter(selection);
                if selection.is_empty() {
                    warnings.push(pest::error::Error::new_from_span(
                        pest::error::ErrorVariant::CustomError {
                            message: format!(
                                "Selector '{}' did not match any symbols",
                                selector.name()
                            ),
                        },
                        selector.span(),
                    ));
                }
            }
            selections.push((selector.id(), current_selection));
        }
        Ok(ComputeResult { selections, warnings, new_eph_ids })
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
