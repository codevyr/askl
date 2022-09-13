use std::iter::FromIterator;
use std::iter::Iterator;

use crate::symbols::Symbol;
use crate::symbols::{Location, SymbolMap};
use petgraph::{
    algo::all_simple_paths,
    graphmap::DiGraphMap,
    Direction::{Incoming, Outgoing},
};

#[derive(Debug, Clone)]
pub struct ControlFlowGraph<'a> {
    graph: DiGraphMap<&'a Location, ()>,
    symbols: &'a SymbolMap,
}

#[derive(Debug, Clone)]
pub struct NodeList<'a>(pub Vec<&'a Location>);

#[derive(Debug, Clone)]
pub struct EdgeList<'a>(pub Vec<(&'a Location, &'a Location)>);

impl<'a> ControlFlowGraph<'a> {
    // pub fn new() -> Self {
    //     Self {
    //         graph: DiGraphMap::new(),
    //         symbols: SymbolMap::new(),
    //     }
    // }

    pub fn from_symbols(symbols: &'a SymbolMap) -> Self {
        let mut g = DiGraphMap::new();
        for (child_l, s) in symbols.iter() {
            for parent_l in s.parents.iter() {
                g.add_edge(parent_l, child_l, ());
            }
        }
        Self {
            graph: g,
            symbols: symbols,
        }
    }

    pub fn merge(&'a mut self, other: &'a ControlFlowGraph) {
        self.graph.extend(other.graph.all_edges());
    }

    pub fn iter_symbols(&'a self) -> impl Iterator<Item = (&Location, &Symbol)> + 'a {
        self.symbols.iter()
    }

    pub fn iter_sink(&'a self) -> impl Iterator<Item = &'a Location> {
        self.graph
            .nodes()
            .filter(move |n| self.graph.neighbors_directed(*n, Outgoing).count() == 0)
    }

    pub fn iter_source(&'a self) -> impl Iterator<Item = &'a Location> {
        self.graph
            .nodes()
            .filter(move |n| self.graph.neighbors_directed(*n, Incoming).count() == 0)
    }

    pub fn add_edge(&mut self, from: &'a Location, to: &'a Location) {
        self.graph.add_edge(from, to, ());
    }

    pub fn find_paths<TargetColl>(
        &'a self,
        from: &'a Location,
        to: &'a Location,
        max_intermediate_nodes: Option<usize>,
    ) -> impl Iterator<Item = TargetColl> + 'a
    where
        TargetColl: FromIterator<&'a Location> + 'a,
    {
        all_simple_paths(&self.graph, from, to, 0, max_intermediate_nodes)
    }
}
