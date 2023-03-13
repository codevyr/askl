use std::iter::FromIterator;
use std::iter::Iterator;

use crate::symbols::{Symbol, SymbolId, SymbolMap};
use itertools::Itertools;
use petgraph::{algo::all_simple_paths, graphmap::DiGraphMap, Direction::Incoming};

#[derive(Debug, Clone)]
pub struct ControlFlowGraph<'a> {
    graph: DiGraphMap<&'a SymbolId, ()>,
    symbols: &'a SymbolMap,
}

#[derive(Debug, Clone)]
pub struct NodeList<'a>(pub Vec<&'a SymbolId>);

#[derive(Debug, Clone)]
pub struct EdgeList<'a>(pub Vec<(&'a SymbolId, &'a SymbolId)>);

impl<'a> ControlFlowGraph<'a> {
    pub fn from_symbols(symbols: &'a SymbolMap) -> Self {
        let mut g = DiGraphMap::new();
        for (parent_l, s) in symbols.iter() {
            for child_l in s.children.iter() {
                g.add_edge(parent_l, child_l, ());
            }
        }
        Self {
            graph: g,
            symbols: symbols,
        }
    }

    pub fn iter_symbols(&'a self) -> impl Iterator<Item = (&SymbolId, &Symbol)> + 'a {
        self.symbols.iter()
    }

    pub fn get_symbol(&'a self, loc: &'a SymbolId) -> Option<&'a Symbol> {
        self.symbols.map.get(loc)
    }

    pub fn get_children(&'a self, parent: &'a SymbolId) -> Vec<&'a SymbolId> {
        self.graph
            .neighbors_directed(parent, Incoming)
            .collect_vec()
    }

    pub fn find_paths<TargetColl>(
        &'a self,
        from: &'a SymbolId,
        to: &'a SymbolId,
        max_intermediate_nodes: Option<usize>,
    ) -> impl Iterator<Item = TargetColl> + 'a
    where
        TargetColl: FromIterator<&'a SymbolId> + 'a,
    {
        all_simple_paths(&self.graph, from, to, 0, max_intermediate_nodes)
    }
}
