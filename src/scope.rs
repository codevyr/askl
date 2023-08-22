use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::Rule;
use crate::statement::{build_statement, Statement};
use crate::symbols::SymbolId;
use core::fmt::Debug;
use itertools::Itertools;
use log::debug;
use pest::error::Error;

pub fn build_scope(pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(build_statement).collect();
    Ok(Box::new(DefaultScope(statements?)))
}

pub trait Scope: Debug {
    fn find_matches<'a>(
        &self,
        cfg_in: &'a ControlFlowGraph,
        parent: SymbolId,
    ) -> (NodeList, EdgeList);

    /// Run statements in a scope. Return list of top-level nodes and all egdes
    /// that belong to the scope.
    fn run<'a>(&self, cfg_in: &'a ControlFlowGraph) -> (NodeList, EdgeList) {
        let mut nodes_scope = vec![];
        let mut edges_scope = vec![];
        for statement in self.statements().iter() {
            let (nodes, edges) = statement.run(cfg_in);
            nodes_scope.extend(nodes.0);
            edges_scope.extend(edges.0);
        }
        (NodeList(nodes_scope), EdgeList(edges_scope))
    }

    fn combine<'a>(
        &self,
        full: &'a ControlFlowGraph,
        outer: NodeList,
        inner: NodeList,
    ) -> (NodeList, EdgeList) {
        let mut node_matches = vec![];
        let mut edge_matches : Vec<(SymbolId, SymbolId)> = vec![];
        // for from in outer.0.into_iter() {
        //     for to in inner.0.iter() {
        //         let edges = self.matching_edges(full, from.clone(), to.clone());
        //         if edges.0.len() > 0 {
        //             debug!("FROM: {:?}", full.get_symbol(from).unwrap().name.clone());
        //             debug!("TO: {:?}", full.get_symbol(to.clone()).unwrap().name.clone());
        //             debug!(
        //                 "EDGES: {:#?}",
        //                 edges
        //                     .0
        //                     .iter()
        //                     .map(|e| format!(
        //                         "{}->{}",
        //                         full.get_symbol(e.0.clone()).unwrap().name.clone(),
        //                         full.get_symbol(e.1.clone()).unwrap().name.clone()
        //                     ))
        //                     .collect::<Vec<_>>()
        //             );
        //             node_matches.push(from);
        //             edge_matches.extend(edges.0.into_iter());
        //         }
        //     }
        // }
        unimplemented!();
        (NodeList(node_matches), EdgeList(edge_matches))
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>>;
    fn matching_edges<'a>(
        &self,
        full: &'a ControlFlowGraph,
        from: SymbolId,
        to: SymbolId,
    ) -> EdgeList;
}

#[derive(Debug)]
pub struct DefaultScope(Vec<Box<dyn Statement>>);

impl DefaultScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Self {
        Self(statements)
    }
}

impl Scope for DefaultScope {
    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }

    fn find_matches<'a>(
        &self,
        cfg_in: &'a ControlFlowGraph,
        parent: SymbolId,
    ) -> (NodeList, EdgeList) {
        // let descendants = cfg_in.get_children(parent);
        let descendants = vec![parent.clone()];
        unimplemented!();

        let mut nodes_scope = vec![];
        let mut edges_scope = vec![];
        for statement in self.statements().iter() {
            for node in descendants.into_iter() {
                let (nodes, edges) = statement.find_matches(cfg_in, node);
                nodes_scope.extend(nodes.0);
                edges_scope.extend(edges.0);
            }
        }
        (NodeList(nodes_scope), EdgeList(edges_scope))
    }

    fn matching_edges<'a>(
        &self,
        full: &'a ControlFlowGraph,
        from: SymbolId,
        to: SymbolId,
    ) -> EdgeList {
        let mut result : Vec<(SymbolId, SymbolId)> = vec![];
        for path in full.find_paths::<Vec<SymbolId>>(from, to, Some(0)) {
            path.iter()
                .tuple_windows()
                .map(|(from, to)| {
                    result.push(((*from).clone(), (*to).clone()));
                })
                .collect()
        }
        EdgeList(result)
    }
}

#[derive(Debug)]
pub struct EmptyScope(Vec<Box<dyn Statement>>);

impl EmptyScope {
    pub fn new() -> Self {
        Self(vec![])
    }
}

impl Scope for EmptyScope {
    fn find_matches<'a>(
        &self,
        _cfg_in: &'a ControlFlowGraph,
        _parent: SymbolId,
    ) -> (NodeList, EdgeList) {
        (NodeList(vec![]), EdgeList(vec![]))
    }

    fn combine<'a>(
        &self,
        _full: &ControlFlowGraph,
        outer: NodeList,
        _inner: NodeList,
    ) -> (NodeList, EdgeList) {
        (outer, EdgeList(vec![]))
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }

    fn matching_edges<'a>(
        &self,
        _full: &'a ControlFlowGraph,
        _from: SymbolId,
        _to: SymbolId,
    ) -> EdgeList {
        unreachable!("Cannot match edges in empty scope")
    }
}
