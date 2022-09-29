use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::Rule;
use crate::statement::{build_statement, Statement};
use crate::symbols::Location;
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
        parent: &'a Location,
    ) -> (NodeList<'a>, EdgeList<'a>);

    /// Run statements in a scope. Return list of top-level nodes and all egdes
    /// that belong to the scope.
    fn run<'a>(&self, cfg_in: &'a ControlFlowGraph) -> (NodeList<'a>, EdgeList<'a>) {
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
        outer: NodeList<'a>,
        inner: NodeList<'a>,
    ) -> (NodeList<'a>, EdgeList<'a>) {
        let mut node_matches = vec![];
        let mut edge_matches = vec![];
        for from in outer.0.into_iter() {
            for to in inner.0.iter() {
                let edges = self.matching_edges(full, from, to);
                if edges.0.len() > 0 {
                    node_matches.push(from);
                    edge_matches.extend(edges.0.iter());
                }
            }
        }
        (NodeList(node_matches), EdgeList(edge_matches))
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>>;
    fn matching_edges<'a>(
        &self,
        full: &'a ControlFlowGraph,
        from: &'a Location,
        to: &'a Location,
    ) -> EdgeList<'a>;
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
        parent: &'a Location,
    ) -> (NodeList<'a>, EdgeList<'a>) {
        let descendants = cfg_in.get_children(parent);

        let mut nodes_scope = vec![];
        let mut edges_scope = vec![];
        for statement in self.statements().iter() {
            for node in descendants.iter() {
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
        from: &'a Location,
        to: &'a Location,
    ) -> EdgeList<'a> {
        let mut result = vec![];
        for path in full.find_paths::<Vec<&Location>>(from, to, Some(0)) {
            path.iter()
                .tuple_windows()
                .map(|(from, to)| {
                    result.push((*from, *to));
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
        cfg_in: &'a ControlFlowGraph,
        parent: &'a Location,
    ) -> (NodeList<'a>, EdgeList<'a>) {
        (NodeList(vec![]), EdgeList(vec![]))
    }

    fn combine<'a>(
        &self,
        _full: &ControlFlowGraph,
        outer: NodeList<'a>,
        _inner: NodeList<'a>,
    ) -> (NodeList<'a>, EdgeList<'a>) {
        (outer, EdgeList(vec![]))
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }

    fn matching_edges<'a>(
        &self,
        _full: &'a ControlFlowGraph,
        _from: &'a Location,
        _to: &'a Location,
    ) -> EdgeList<'a> {
        unreachable!("Cannot match edges in empty scope")
    }
}
