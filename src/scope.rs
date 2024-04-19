use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::Rule;
use crate::statement::{build_statement, Statement};
use crate::symbols::{Occurence, SymbolChild, SymbolId};
use core::fmt::Debug;
use log::debug;
use pest::error::Error;

pub fn build_scope(pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(build_statement).collect();
    Ok(Box::new(DefaultScope(statements?)))
}

pub trait Scope: Debug {
    fn statements(&self) -> &Vec<Box<dyn Statement>>;

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList);
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

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        let mut res_symbols: Vec<SymbolChild> = vec![];
        let mut res_nodes = NodeList(vec![]);
        let mut res_edges = EdgeList(vec![]);

        for statement in self.statements().iter() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((passed_symbols, scope_nodes, scope_edges)) =
                statement.execute(cfg, &active_symbols)
            {
                res_symbols.extend(passed_symbols.into_iter());

                res_nodes.0.extend(scope_nodes.0.into_iter());
                res_nodes
                    .0
                    .extend(res_symbols.iter().map(|s| s.symbol_id.clone()));
                res_edges.0.extend(scope_edges.0.into_iter());
            }
        }

        // Sort and deduplicate the sources
        res_symbols.sort();
        res_symbols.dedup();
        res_nodes.0.sort();
        res_nodes.0.dedup();
        res_edges.0.sort();
        res_edges.0.dedup();
        (res_symbols, res_nodes, res_edges)
    }
}

#[derive(Debug)]
pub struct GlobalScope(Vec<Box<dyn Statement>>);

impl GlobalScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Self {
        Self(statements)
    }
}

impl Scope for GlobalScope {
    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        let mut res_symbols: Vec<SymbolChild> = vec![];
        let mut nodes = NodeList(vec![]);
        let mut edges = EdgeList(vec![]);

        for statement in self.statements().iter() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((new_passed_symbols, node_list, edge_list)) =
                statement.execute(cfg, &active_symbols)
            {
                nodes.0.extend(node_list.0.into_iter());
                nodes
                    .0
                    .extend(new_passed_symbols.iter().map(|s| s.symbol_id.clone()));
                edges.0.extend(edge_list.0.into_iter());
                res_symbols.extend(new_passed_symbols.into_iter());
            }
        }

        // Sort and deduplicate the sources
        res_symbols.sort();
        res_symbols.dedup();
        nodes.0.sort();
        nodes.0.dedup();
        edges.0.sort();
        edges.0.dedup();
        (res_symbols, nodes, edges)
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
    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }

    fn run(
        &self,
        _cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        (
            active_symbols.clone(),
            NodeList(active_symbols.iter().map(|s| s.symbol_id.clone()).collect()),
            EdgeList(vec![]),
        )
    }
}
