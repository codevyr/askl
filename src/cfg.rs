use std::iter::Iterator;

use crate::symbols::{Symbol, SymbolId, SymbolMap};
use petgraph::graphmap::DiGraphMap;

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    pub symbols: SymbolMap,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub Vec<SymbolId>);

#[derive(Debug, Clone)]
pub struct EdgeList(pub Vec<(SymbolId, SymbolId)>);

impl ControlFlowGraph {
    pub fn from_symbols(symbols: SymbolMap) -> Self {
        let mut g = DiGraphMap::new();
        for (parent_l, s) in symbols.iter() {
            for child_l in s.children.iter() {
                g.add_edge(parent_l, child_l, ());
            }
        }
        Self {
            symbols: symbols,
        }
    }

    pub fn iter_symbols(&self) -> impl Iterator<Item = (&SymbolId, &Symbol)> {
        self.symbols.iter()
    }

    pub fn get_symbol(&self, loc: &SymbolId) -> Option<&Symbol> {
        self.symbols.map.get(loc)
    }
}
