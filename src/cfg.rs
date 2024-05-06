use std::{collections::HashSet, iter::Iterator};

use crate::symbols::{Occurrence, Symbol, SymbolId, SymbolMap};

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    pub symbols: SymbolMap,
    pub nodes: HashSet<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub Vec<SymbolId>);

#[derive(Debug, Clone)]
pub struct EdgeList(pub Vec<(SymbolId, SymbolId, Option<Occurrence>)>);

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
        self.symbols.symbols.get(loc)
    }

    pub fn get_symbol_by_name(&self, name: &str) -> Vec<&Symbol> {
        self.symbols
            .symbols
            .iter()
            .filter_map(|(_, v)| if v.name == *name { Some(v) } else { None })
            .collect()
    }
}
