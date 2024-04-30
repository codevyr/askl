use std::{iter::Iterator, collections::HashSet};

use crate::symbols::{Symbol, SymbolId, SymbolMap, Occurence};

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    pub symbols: SymbolMap,
    pub nodes: HashSet<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub Vec<SymbolId>);

#[derive(Debug, Clone)]
pub struct EdgeList(pub Vec<(SymbolId, SymbolId, Option<Occurence>)>);

impl EdgeList {
    pub fn new() -> Self {
        EdgeList(vec![])
    }
}

impl ControlFlowGraph {
    pub fn from_symbols(symbols: SymbolMap) -> Self {
        let nodes: HashSet<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();
        Self {
            symbols: symbols,
            nodes: nodes,
        }
    }

    pub fn iter_symbols(&self) -> impl Iterator<Item = (&SymbolId, &Symbol)> {
        self.symbols.iter()
    }

    pub fn get_symbol(&self, loc: &SymbolId) -> Option<&Symbol> {
        self.symbols.map.get(loc)
    }
}
