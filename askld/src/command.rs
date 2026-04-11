use crate::cfg::ControlFlowGraph;
use crate::execution_context::{selector_state_with, ExecutionContext};
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::Rule;
use crate::span::Span;
use crate::statement::Statement;
use crate::verb::{add_verb, ConstraintAction, DeriveMethod, Filter, Labeler, NotificationContext, Selector, Verb, VerbTag, find_symbol_by_instance_id};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{CompositeFilter, InnermostOnlyMixin, Index, ScopeContext, Selection};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

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
}

impl Command {
    pub fn new(span: Span) -> Command {
        Self {
            verbs: vec![],
            span: Some(span),
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
        }
    }

    pub fn span(&self) -> &Span {
        self.span.as_ref().unwrap()
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
    pub fn get_selector_composite_filter(&self) -> Option<CompositeFilter> {
        let parts: Vec<_> = self.selectors()
            .filter_map(|sel| sel.build_composite_filter(self))
            .collect();
        match parts.len() {
            0 => None,
            1 => parts.into_iter().next(),
            _ => Some(CompositeFilter::or(parts)),
        }
    }

    pub fn filter(&self, selection: &mut Selection) {
        let _command_filter: tracing::span::EnteredSpan =
            tracing::info_span!("command_filter").entered();
        for verb in self.filters() {
            verb.filter(selection);
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
                    find_parts.push(CompositeFilter::leaf(InnermostOnlyMixin));
                }
                let find_filter = CompositeFilter::and(find_parts);
                derivation_ids = Some(index.find_parent_instance_ids(
                    &child_ids,
                    rel_type.contains(RelationshipType::REFS),
                    rel_type.contains(RelationshipType::HAS),
                    &find_filter,
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

            let mut selection = find_symbol_by_instance_id(index, &selector_filters, decl_ids, parent_scope.clone(), children_scope.clone())
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
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
    ) -> Result<Vec<pest::error::Error<Rule>>, pest::error::Error<Rule>> {
        let selectors: Vec<&dyn Selector> = self.selectors().collect();

        // Nothing to do
        if selectors.len() == 0 {
            return Ok(Vec::new());
        }

        let mut warnings = vec![];

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

        let filter_parts: Vec<CompositeFilter> =
            self.filters().filter_map(|f| f.get_composite_filter()).collect();

        for selector in selectors.into_iter() {
            let filter = CompositeFilter::and(filter_parts.clone());

            let select_from_all_name = format!("{:?}", selector);
            let _select_from_all =
                tracing::info_span!("select_from_all", name = %select_from_all_name).entered();
            let mut current_selection = selector
                .select_from_all_impl(ctx, cfg, filter, parent_scope.clone(), children_scope.clone())
                .await
                .map_err(|e| {
                    pest::error::Error::new_from_span(
                        pest::error::ErrorVariant::CustomError {
                            message: e.to_string(),
                        },
                        self.span().as_pest_span(),
                    )
                })?;
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
            ctx.registry.add(selector, current_selection);
        }
        Ok(warnings)
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
