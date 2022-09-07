use std::collections::HashSet;
use std::iter::FromIterator;
use std::iter::Iterator;

use crate::symbols::{Location, SymbolMap, Symbols};
use petgraph::{
    algo::all_simple_paths,
    graphmap::DiGraphMap,
    Direction::{Incoming, Outgoing},
};

#[derive(Debug, Clone)]
pub struct ControlFlowGraph {
    graph: DiGraphMap<Location, ()>,
    symbols: SymbolMap,
    /// Marks for locations
    marks: HashSet<Location>,
}

impl ControlFlowGraph {
    pub fn new() -> Self {
        Self {
            graph: DiGraphMap::new(),
            symbols: SymbolMap::new(),
            marks: HashSet::new(),
        }
    }

    pub fn from_symbols(symbols: SymbolMap) -> Self {
        let mut g = DiGraphMap::new();
        for (child_l, s) in symbols.into_iter() {
            for parent_l in s.parents.iter() {
                g.add_edge(parent_l.clone(), child_l.clone(), ());
            }
        }
        Self {
            graph: g,
            symbols: symbols,
            marks: HashSet::new(),
        }
    }

    pub fn merge(&mut self, other: &ControlFlowGraph) {
        self.graph.extend(other.graph.all_edges());
        self.marks.extend(other.marks.iter());
    }

    pub fn iter_sink<'a>(&'a self) -> impl Iterator<Item = Location> + 'a {
        self.graph
            .nodes()
            .filter(move |n| self.graph.neighbors_directed(*n, Outgoing).count() == 0)
    }

    pub fn iter_source<'a>(&'a self) -> impl Iterator<Item = Location> + 'a {
        self.graph
            .nodes()
            .filter(move |n| self.graph.neighbors_directed(*n, Incoming).count() == 0)
    }

    pub fn add_edge(&mut self, from: Location, to: Location) {
        self.graph.add_edge(from, to, ());
    }

    pub fn find_paths<'a, TargetColl>(
        &'a self,
        from: Location,
        to: Location,
        max_intermediate_nodes: Option<usize>,
    ) -> impl Iterator<Item = TargetColl> + 'a
    where
        TargetColl: FromIterator<Location> + 'a,
    {
        all_simple_paths(&self.graph, from, to, 0, max_intermediate_nodes)
    }
}
