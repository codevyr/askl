use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use index::db::Declaration;
use index::symbols::{Reference, SymbolId, SymbolRefs, DeclarationRefs, DeclarationId};
use crate::verb::{DeriveMethod, Deriver, Filter, Marker, Selector, UnitVerb, Verb};
use anyhow::Result;
use core::fmt::Debug;
use std::collections::HashSet;
use std::sync::Arc;

#[derive(Debug)]
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

        Self { verbs }
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

    pub fn filter(&self, cfg: &ControlFlowGraph, symbols: DeclarationRefs) -> Option<DeclarationRefs> {
        Some(
            self.filters()
                .fold(symbols, |symbols, verb| verb.filter(cfg, symbols)),
        )
    }

    pub fn select(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<DeclarationRefs>,
    ) -> Option<DeclarationRefs> {
        let selectors: Vec<_> = self.selectors().collect();

        // Nothing to do
        if selectors.len() == 0 {
            return symbols;
        }

        let selector: Box<dyn FnMut(&dyn Selector) -> Option<DeclarationRefs>> = match symbols {
            Some(symbols) => Box::new(move |v: &dyn Selector| v.select(ctx, cfg, symbols.clone())),
            None => Box::new(|v: &dyn Selector| v.select_from_all(ctx, cfg)),
        };

        let symbols: DeclarationRefs = selectors
            .into_iter()
            .filter_map(selector)
            .flatten()
            .collect();

        if symbols.len() == 0 {
            return None;
        }

        Some(symbols)
    }

    pub async fn derive_children(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference> {
        self.derivers()
            .last()
            .unwrap()
            .derive_children(ctx, cfg, declarations).await
    }

    pub async fn derive_parents(&self, cfg: &ControlFlowGraph, symbol: DeclarationId) -> Option<DeclarationRefs> {
        self.derivers().last().unwrap().derive_parents(cfg, symbol).await
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