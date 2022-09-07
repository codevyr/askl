use crate::cfg::ControlFlowGraph;
use crate::parser::Rule;
use crate::statement::{build_statement, Statement};
use crate::symbols::Location;
use core::fmt::Debug;
use itertools::Itertools;
use pest::error::Error;
use std::collections::HashSet;

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

    fn combine(
        &self,
        full: &ControlFlowGraph,
        outer: &ControlFlowGraph,
        inner: &ControlFlowGraph,
    ) -> ControlFlowGraph {
        let mut matched_sinks = HashSet::new();
        let mut result = ControlFlowGraph::new();
        for sink in outer.iter_sink() {
            for source in inner.iter_source() {
                let edges = self.matching_edges(full, sink, source);
                if edges.len() > 0 {
                    matched_sinks.insert(sink);
                    for (from, to) in edges {
                        result.add_edge(from, to);
                    }
                }
            }
        }

        for source in outer.iter_source() {
            for sink in matched_sinks.iter() {
                for path in outer.find_paths::<Vec<Location>>(source, *sink, None) {
                    path.iter()
                        .tuple_windows()
                        .map(|(from, to)| {
                            result.add_edge(*from, *to);
                        })
                        .collect()
                }
            }
        }
        outer.clone()
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>>;
    fn matching_edges(
        &self,
        full: &ControlFlowGraph,
        from: Location,
        to: Location,
    ) -> Vec<(Location, Location)>;
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

    fn matching_edges(
        &self,
        full: &ControlFlowGraph,
        from: Location,
        to: Location,
    ) -> Vec<(Location, Location)> {
        let mut result = vec![];
        for path in full.find_paths::<Vec<Location>>(from, to, Some(1)) {
            path.iter()
                .tuple_windows()
                .map(|(from, to)| {
                    result.push((*from, *to));
                })
                .collect()
        }
        result
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
    fn combine(
        &self,
        _full: &ControlFlowGraph,
        outer: &ControlFlowGraph,
        _inner: &ControlFlowGraph,
    ) -> ControlFlowGraph {
        outer.clone()
    }

    fn statements(&self) -> &Vec<Box<dyn Statement>> {
        &self.0
    }

    fn matching_edges(
        &self,
        _full: &ControlFlowGraph,
        _from: Location,
        _to: Location,
    ) -> Vec<(Location, Location)> {
        unreachable!("Cannot match edges in empty scope")
    }
}
