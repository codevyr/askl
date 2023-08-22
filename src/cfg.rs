use std::iter::FromIterator;
use std::iter::Iterator;

use crate::scope::Scope;
use crate::symbols::{Symbol, SymbolId, SymbolMap};
use indicatif::ProgressBar;
use itertools::Itertools;
use petgraph::{algo::all_simple_paths, graphmap::DiGraphMap, Direction::Incoming};

type DiGraph<'a> = DiGraphMap<SymbolId, ()>;

#[derive(Debug, Clone)]
pub struct ControlFlowGraph<'a> {
    // graph: DiGraph<'a>,
    symbols: &'a SymbolMap,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub Vec<SymbolId>);

#[derive(Debug, Clone)]
pub struct EdgeList(pub Vec<(SymbolId, SymbolId)>);

impl<'a> ControlFlowGraph<'a> {
    pub fn from_symbols(symbols: &'a SymbolMap) -> Self {
        let mut g = DiGraphMap::new();
        for (parent_l, s) in symbols.iter() {
            for child_l in s.children.iter() {
                g.add_edge(parent_l, child_l, ());
            }
        }
        Self {
            // graph: DiGraphMap::new(),
            symbols: symbols,
        }
    }

    pub fn iter_symbols(&'a self) -> impl Iterator<Item = (&SymbolId, &Symbol)> + 'a {
        self.symbols.iter()
    }

    pub fn get_symbol(&'a self, loc: SymbolId) -> Option<&'a Symbol> {
        self.symbols.map.get(&loc)
    }

    // pub fn get_children(&'a self, parent: SymbolId) -> Vec<SymbolId> {
    //     self.graph
    //         .neighbors_directed(parent, Incoming)
    //         .collect_vec()
    // }

    pub fn find_paths<TargetColl>(
        &'a self,
        from: SymbolId,
        to: SymbolId,
        max_intermediate_nodes: Option<usize>,
    ) -> impl Iterator<Item = TargetColl> + 'a
    where
        TargetColl: FromIterator<SymbolId> + 'a,
    {
        unimplemented!();
        vec![].into_iter()
        //all_simple_paths(&self.graph, from.clone(), to.clone(), 0, max_intermediate_nodes)
    }

    pub fn matched_symbols(&self, sources: Vec<SymbolId>, scope: &dyn Scope, progress: bool) -> Option<(Vec<SymbolId>, EdgeList)>{
        let mut result = EdgeList(vec![]);
        let mut result_sources : Vec<SymbolId> = vec![];

        let pb = if progress {
            Some(ProgressBar::new(self.symbols.map.len() as u64))
        } else {
            None
        };

        if scope.statements().len() == 0 {
            return Some((result_sources, result));
        }

        // Iterate through all the symbols in the CFG
        for parent_id in sources.into_iter() {
            if let Some(pb) = pb.as_ref() {
                pb.inc(1);
            }

            let parent = self.get_symbol(parent_id.clone()).unwrap();

            // Iterate through all the statements in the scope or subscope of
            // the query
            for statement in scope.statements().iter() {

                // If the statement matches the symbol, add it to the result
                if statement.verb().mark(parent) {
                    let children = self.symbols.get_children(parent_id.clone());
                    if let Some((source_ids, mut edges)) = self.matched_symbols(children, statement.scope(), false) {
                        for source_id in source_ids.into_iter() {
                            edges.0.push((parent_id.clone(), source_id.clone()));
                        }
    
                        // This nodes matches the pattern, so remember it
                        result_sources.push(parent_id.clone());
                        result.0.extend(edges.0.into_iter()); 
                    }
                }
            }
        }

        if result_sources.len() == 0 {
            return None;
        }

        // Sort and deduplicate the sources
        result_sources.sort();
        result_sources.dedup();
        result.0.sort();
        result.0.dedup();
        Some((result_sources, result))
    }
}
