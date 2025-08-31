use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::statement::Statement;
use crate::verb::{DeriveMethod, Deriver, Filter, Marker, Selector, UnitVerb, Verb};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{ChildReference, ParentReference, Selection};
use index::symbols::DeclarationRefs;
use std::sync::Arc;

#[derive(Debug, Default)]
pub struct Command {
    verbs: Vec<Arc<dyn Verb>>,
}

impl Command {
    pub fn new() -> Command {
        Self {
            verbs: vec![UnitVerb::new()],
        }
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
        self.verbs.push(other);
    }

    fn filters<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Filter> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_filter().ok()))
    }

    fn selectors<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Selector> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_selector().ok()))
    }

    fn derivers<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Deriver> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_deriver().ok()))
    }

    fn markers<'a>(&'a self) -> Box<dyn Iterator<Item = &'a dyn Marker> + 'a> {
        Box::new(self.verbs.iter().filter_map(|verb| verb.as_marker().ok()))
    }

    pub fn filter(&self, cfg: &ControlFlowGraph, selection: &mut Selection) {
        let _command_filter: tracing::span::EnteredSpan =
            tracing::info_span!("command_filter").entered();
        for verb in self.filters() {
            verb.filter(cfg, selection);
        }
    }

    pub fn constrain_references(&self, cfg: &ControlFlowGraph, selection: &mut Selection) {
        let _constrain_references: tracing::span::EnteredSpan =
            tracing::info_span!("constrain_references").entered();
        self.derivers()
            .for_each(|verb| verb.constrain_references(cfg, selection))
    }

    pub fn constrain_by_parents(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        parent_refs: &Vec<ChildReference>,
    ) {
        self.derivers()
            .for_each(|verb| verb.constrain_by_parents(cfg, selection, parent_refs))
    }

    pub fn constrain_by_children(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        child_refs: &Vec<ParentReference>,
    ) {
        self.derivers()
            .for_each(|verb| verb.constrain_by_children(cfg, selection, child_refs))
    }

    /// Computes the selected symbols based on the selectors defined in the
    /// command. This method returns an `Option<DeclarationRefs>`, which will be
    /// `None` if no symbols are selected. It returns
    /// `Some(DeclarationRefs::new())` if no symbols match the selectors.
    pub async fn compute_selected(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Option<Selection> {
        let selectors: Vec<_> = self.selectors().collect();

        // Nothing to do
        if selectors.len() == 0 {
            return None;
        }

        let mut selection = Selection::new();
        for selector in selectors.iter() {
            let current_selection = selector.select_from_all(ctx, cfg).await.unwrap();
            selection.extend(current_selection);
        }

        if selection.is_empty() {
            return None;
        }

        self.filter(cfg, &mut selection);

        Some(selection)
    }

    pub async fn derive_children(
        &self,
        statement: &Statement,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        children_refs: &Vec<ChildReference>,
    ) -> Option<Selection> {
        self.derivers()
            .last()
            .unwrap()
            .derive_children(statement, ctx, cfg, children_refs)
            .await
    }

    pub async fn derive_parents(
        &self,
        ctx: &mut ExecutionContext,
        statement: &Statement,
        cfg: &ControlFlowGraph,
        parents_refs: &Vec<ParentReference>,
    ) -> Option<Selection> {
        self.derivers()
            .last()
            .unwrap()
            .derive_parents(ctx, statement, cfg, parents_refs)
            .await
    }

    pub fn mark(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: &DeclarationRefs,
    ) -> Result<()> {
        self.markers().try_for_each(|m| m.mark(ctx, cfg, symbols))
    }
}
