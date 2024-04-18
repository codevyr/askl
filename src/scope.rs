use crate::cfg::{ControlFlowGraph, EdgeList};
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
    fn get_children(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Vec<SymbolChild>;

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> Option<(Vec<SymbolChild>, EdgeList)> {
        let mut nodes = vec![];
        let mut edges = EdgeList(vec![]);

        if self.statements().len() == 0 {
            return Some((nodes, edges));
        }

        for statement in self.statements().iter() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((passed_children, edge_list)) = statement.execute(cfg, &active_symbols) {
                nodes.extend(passed_children.into_iter());
                edges.0.extend(edge_list.0.into_iter());
            }
        }

        // Sort and deduplicate the sources
        nodes.sort();
        nodes.dedup();
        edges.0.sort();
        edges.0.dedup();
        Some((nodes, edges))
    }
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

    fn get_children(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Vec<SymbolChild> {
        // debug!("get_children from Default: {:?}", symbol);
        cfg.symbols.get_children(symbol)
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

    fn get_children(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Vec<SymbolChild> {
        debug!("get_children from Empty: {:?}", symbol);
        vec![]
    }
}
