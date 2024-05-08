use crate::cfg::ControlFlowGraph;
use crate::symbols::{SymbolId, SymbolRefs};
use crate::verb::{DeriveMethod, Deriver, Filter, Resolution, Selector, UnitVerb, Verb};
use core::fmt::Debug;
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

    pub fn filter(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Option<SymbolRefs>,
    ) -> Option<SymbolRefs> {
        Some(
            self.filters()
                .fold(symbols?, |symbols, verb| verb.filter(cfg, symbols)),
        )
    }

    pub fn select(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Option<SymbolRefs>,
    ) -> Option<SymbolRefs> {
        let selectors: Vec<_> = self.selectors().collect();

        // Nothing to do
        if selectors.len() == 0 {
            return symbols;
        }

        let selector: Box<dyn Fn(&dyn Selector) -> Option<SymbolRefs>> = match symbols {
            Some(symbols) => Box::new(move |v: &dyn Selector| v.select(cfg, symbols.clone())),
            None => Box::new(|v: &dyn Selector| v.select_from_all(cfg)),
        };

        let symbols: SymbolRefs = selectors
            .into_iter()
            .filter_map(selector)
            .flatten()
            .collect();

        if symbols.len() == 0 {
            return None;
        }

        Some(symbols)
    }

    pub fn derive_symbols(&self, cfg: &ControlFlowGraph, symbol: SymbolId) -> Option<SymbolRefs> {
        if let Some(res) = self.derivers().last().unwrap().derive_symbols(cfg, symbol) {
            Some(res)
        } else {
            None
        }
    }

    pub fn derive_children(&self, cfg: &ControlFlowGraph, symbol: SymbolId) -> Option<SymbolRefs> {
        self.derivers().last().unwrap().derive_children(cfg, symbol)
    }

    pub fn derive_parents(&self, cfg: &ControlFlowGraph, symbol: SymbolId) -> Option<SymbolRefs> {
        self.derivers().last().unwrap().derive_parents(cfg, symbol)
    }
}

impl Verb for Command {
    fn resolution(&self) -> Resolution {
        let mut res = Resolution::Weak;
        for v in self.verbs.iter() {
            res = res.max(v.resolution());
        }

        res
    }
}
