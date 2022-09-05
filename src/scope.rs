use crate::cfg::ControlFlowGraph;
use crate::parser::Rule;
use crate::statement::{build_statement, Statement};
use core::fmt::Debug;
use pest::error::Error;

pub fn build_scope(pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(build_statement).collect();
    Ok(Box::new(DefaultScope(statements?)))
}

pub trait Scope: Debug {
    fn run(&self, cfg_in: &ControlFlowGraph) -> ControlFlowGraph {
        let mut cfg_out = ControlFlowGraph::new();
        for statement in self.statements().iter() {
            cfg_out.merge(&statement.run(cfg_in));
        }
        cfg_out
    }

    fn combine(&self, outer: &ControlFlowGraph, inner: &ControlFlowGraph) -> ControlFlowGraph;
    fn statements(&self) -> &Vec<Box<dyn Statement>>;
}

#[derive(Debug)]
pub struct DefaultScope(Vec<Box<dyn Statement>>);

impl DefaultScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Self {
        Self(statements)
    }
}

impl Scope for DefaultScope {
    fn combine(&self, outer: &ControlFlowGraph, _inner: &ControlFlowGraph) -> ControlFlowGraph {
        outer.clone()
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }
}

#[derive(Debug)]
pub struct EmptyScope(Vec<Box<dyn Statement>>);

impl EmptyScope {
    pub fn new() -> Self {
        Self (vec![])
    }
}

impl Scope for EmptyScope {
    fn combine(&self, outer: &ControlFlowGraph, _inner: &ControlFlowGraph) -> ControlFlowGraph {
        outer.clone()
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }
}
