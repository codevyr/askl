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
    fn get_children(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Vec<SymbolChild>;

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        active_symbols: &Vec<SymbolChild>,
    ) -> (Vec<SymbolChild>, NodeList, EdgeList) {
        let mut passed_symbols = vec![];
        let mut nodes = NodeList(vec![]);
        let mut edges = EdgeList(vec![]);

        if self.statements().len() == 0 {
            return (active_symbols.clone(), nodes, edges);
        }

        for statement in self.statements().iter() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((passed_children, node_list, edge_list)) =
                statement.execute(cfg, &active_symbols)
            {
                passed_symbols.extend(passed_children.into_iter());
                nodes.0.extend(node_list.0.into_iter());
                nodes
                    .0
                    .extend(passed_symbols.iter().map(|s| s.symbol_id.clone()));
                edges.0.extend(edge_list.0.into_iter());
            }
        }

        // Sort and deduplicate the sources
        passed_symbols.sort();
        passed_symbols.dedup();
        nodes.0.sort();
        nodes.0.dedup();
        edges.0.sort();
        edges.0.dedup();
        (passed_symbols, nodes, edges)
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

    fn get_children(&self, _cfg: &ControlFlowGraph, symbol: &SymbolId) -> Vec<SymbolChild> {
        debug!("get_children from Empty: {:?}", symbol);
        vec![]
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
