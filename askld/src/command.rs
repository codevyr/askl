use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::Rule;
use crate::span::Span;
use crate::statement::Statement;
use crate::verb::{add_verb, DeriveMethod, Filter, Labeler, Selector, Verb};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{Index, Selection, SymbolSearchMixin};
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

    fn filters<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Filter> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_filter().ok()))
    }

    pub fn selectors<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Selector> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_selector().ok()))
    }

    /// Return cloned Arcs of verbs that implement Filter.
    pub fn filter_verbs(&self) -> Vec<Arc<dyn Verb>> {
        self.verbs
            .iter()
            .filter(|v| v.as_filter().is_ok())
            .cloned()
            .collect()
    }

    /// Check if any verb suppresses the default type filter.
    pub fn has_suppress_default_type_filter(&self) -> bool {
        self.verbs.iter().any(|v| v.suppresses_default_type_filter())
    }

    pub fn has_selectors(&self) -> bool {
        self.verbs.iter().any(|verb| verb.as_selector().is_ok())
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
    ) -> Result<NotificationResult, pest::error::Error<Rule>> {
        let mut changed = false;
        let mut warnings = vec![];
        let selector_filters: Vec<&dyn Filter> = self.filters().collect();
        for selector in self.selectors() {
            let res = selector
                .accept_notification_from_selection(
                    ctx,
                    index,
                    &selector_filters,
                    dependency,
                    role,
                    rel_type,
                )
                .await?;
            changed |= res.changed;
            warnings.extend(res.warnings);
        }
        Ok(NotificationResult::new(changed, warnings))
    }

    pub async fn accept_notification(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        notifier: &Statement,
        role: DependencyRole,
        receiver_rel_type: RelationshipType,
    ) -> Result<NotificationResult, pest::error::Error<Rule>> {
        // Collect filters so we can iterate over them multiple times while notifying selectors.
        let mut changed = false;
        let mut warnings = vec![];
        let selector_filters: Vec<&dyn Filter> = self.filters().collect();
        for selector in self.selectors() {
            let res = selector
                .accept_notification(ctx, index, &selector_filters, notifier, role, receiver_rel_type)
                .await?;
            changed |= res.changed;
            warnings.extend(res.warnings);
        }
        Ok(NotificationResult::new(changed, warnings))
    }

    /// Computes the selected symbols based on the selectors defined in the
    /// command. This method returns an `Option<SymbolInstanceRefs>`, which will be
    /// `None` if no symbols are selected. It returns
    /// `Some(SymbolInstanceRefs::new())` if no symbols match the selectors.
    pub async fn compute_selected(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Vec<pest::error::Error<Rule>> {
        let selectors: Vec<&dyn Selector> = self.selectors().collect();

        // Nothing to do
        if selectors.len() == 0 {
            return Vec::new();
        }

        let mut warnings = vec![];

        // Validate: each selector that requires a name constraint must have one
        // in its own captured filters (per-selector, not command-wide).
        for selector in selectors.iter() {
            if !selector.requires_name_constraint() {
                continue;
            }
            let has_name = selector.has_name_constraint();
            if !has_name {
                warnings.push(pest::error::Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: "@select requires at least one name filter (@filter(\"compound_name\", ...) or @filter(\"exact_name\", ...))".to_string(),
                    },
                    selector.span(),
                ));
            }
        }

        for selector in selectors.into_iter() {
            let search_mixins: Vec<Box<dyn SymbolSearchMixin>> =
                self.filters().flat_map(|f| f.get_filter_mixins()).collect();

            let mut current_selection = selector
                .select_from_all(ctx, cfg, search_mixins)
                .await
                .unwrap();
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
        warnings
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
                .or_insert(vec![statement.clone()])
                .push(statement.clone());
        }

        marks_len
    }

    pub fn get_statements(&self, label: &str) -> Option<&Vec<Rc<Statement>>> {
        self.0.get(label)
    }
}
