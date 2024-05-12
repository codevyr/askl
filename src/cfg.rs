use std::{collections::HashSet, iter::Iterator};

use crate::symbols::{Occurrence, Symbol, SymbolId, SymbolMap};

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    pub symbols: SymbolMap,
    pub nodes: HashSet<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub HashSet<SymbolId>);

impl NodeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add(&mut self, node: SymbolId) {
        self.0.insert(node);
    }

    pub fn as_vec(&self) -> Vec<SymbolId> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort();
        res
    }
}

#[derive(Debug, Clone)]
pub struct EdgeList(pub HashSet<(SymbolId, SymbolId, Option<Occurrence>)>);

impl EdgeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add_references(&mut self, from: SymbolId, to: SymbolId, occurrences: HashSet<Occurrence>) {
        if occurrences.len() == 0 {
            self.0.insert((from, to, None));
            return;
        }

        for occ in occurrences {
            self.0.insert((from, to, Some(occ)));
        }
    }

    pub fn as_vec(&self) -> Vec<(SymbolId, SymbolId, Option<Occurrence>)> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort();
        res
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
