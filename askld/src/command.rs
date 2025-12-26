use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::execution_state::DependencyRole;
use crate::statement::Statement;
use crate::verb::{add_verb, DeriveMethod, Filter, Labeler, Selector, Verb};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{Index, Selection, SymbolSearchMixin};
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct Command {
    verbs: Vec<Arc<dyn Verb>>,
}

impl Command {
    pub fn new() -> Command {
        Self { verbs: vec![] }
    }

    pub fn derive(&self) -> Self {
        let mut verbs = vec![];
        for verb in self.verbs.iter() {
            match verb.derive_method() {
                DeriveMethod::Clone => verbs.push(verb.clone()),
                DeriveMethod::Skip => {}
            }
        }

        Self { verbs: verbs }
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

    pub fn has_selectors(&self) -> bool {
        self.verbs.iter().any(|verb| verb.as_selector().is_ok())
    }

    pub fn is_unit(&self) -> bool {
        self.verbs.iter().all(|verb| verb.is_unit())
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

    pub async fn accept_notification(
        &self,
        ctx: &mut ExecutionContext,
        index: &Index,
        notifier: &Statement,
        role: DependencyRole,
    ) -> Result<bool> {
        // Collect filters so we can iterate over them multiple times while notifying selectors.
        let mut changed = false;
        let selector_filters: Vec<&dyn Filter> = self.filters().collect();
        for selector in self.selectors() {
            changed |= selector
                .accept_notification(ctx, index, &selector_filters, notifier, role)
                .await?;
        }
        Ok(changed)
    }

    /// Computes the selected symbols based on the selectors defined in the
    /// command. This method returns an `Option<DeclarationRefs>`, which will be
    /// `None` if no symbols are selected. It returns
    /// `Some(DeclarationRefs::new())` if no symbols match the selectors.
    pub async fn compute_selected(&self, ctx: &mut ExecutionContext, cfg: &ControlFlowGraph) {
        let selectors: Vec<&dyn Selector> = self.selectors().collect();

        // Nothing to do
        if selectors.len() == 0 {
            return;
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
                selection.prune_references();
            }
            ctx.registry.add(selector, current_selection);
        }
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
