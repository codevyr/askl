use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::{ParserContext, Rule};
use crate::statement::{build_statement, Statement};
use crate::symbols::{SymbolId, SymbolChild};
use crate::verb::Resolution;
use core::fmt::Debug;
use pest::error::Error;

pub fn build_scope(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(|p| build_statement(ctx, p)).collect();

    Ok(ctx.new_scope(statements?))
}

#[derive(Debug)]
pub enum ScopeFactory {
    Children,
    Global,
    Empty,
}

impl ScopeFactory {
    pub fn create(&self, statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        match self {
            Self::Children => DefaultScope::new(statements),
            Self::Global => GlobalScope::new(statements),
            _ => panic!("Impossible: {:?}", self),
        }
    }
}

type StatementIter<'a> = Box<dyn Iterator<Item = &'a Box<dyn Statement + 'a>> + 'a>;
pub trait Scope: Debug {
    fn statements(&self) -> StatementIter;

    fn run(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, Vec<SymbolChild>, NodeList, EdgeList)> {
        let mut res_nodes = NodeList(vec![]);
        let mut res_edges = EdgeList(vec![]);
        let mut res_symbols = vec![];
        let mut resolution = Resolution::None;

        for statement in self.statements() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((scope_resolution, resolved_symbols, scope_nodes, scope_edges)) =
                statement.execute(cfg, symbols.clone(), parent_resolution)
            {
                // res_nodes.0.push(symbol.clone());
                res_nodes.0.extend(scope_nodes.0.into_iter());
                res_edges.0.extend(scope_edges.0.into_iter());
                res_symbols.extend(resolved_symbols.into_iter());
                resolution = resolution.max(scope_resolution);
            }
        }

        // Sort and deduplicate the sources
        res_nodes.0.sort();
        res_nodes.0.dedup();
        res_symbols.sort();
        res_symbols.dedup();

        Some((resolution, res_symbols, res_nodes, res_edges))
    }
}

#[derive(Debug)]
pub struct DefaultScope(Vec<Box<dyn Statement>>);

impl DefaultScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        Box::new(Self(statements))
    }
}

impl Scope for DefaultScope {
    fn statements(&self) -> StatementIter {
        Box::new(self.0.iter())
    }
}

#[derive(Debug)]
pub struct GlobalScope(Vec<Box<dyn Statement>>);

impl GlobalScope {
    pub fn new(statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        Box::new(Self(statements))
    }
}

impl Scope for GlobalScope {
    fn statements(&self) -> StatementIter {
        Box::new(self.0.iter())
    }

    fn run(
        &self,
        _cfg: &ControlFlowGraph,
        _symbol: Vec<SymbolChild>,
        _parent_resolution: Resolution,
    ) -> Option<(Resolution, Vec<SymbolChild>, NodeList, EdgeList)> {
        None
    }
}

#[derive(Debug)]
pub struct EmptyScope;

impl EmptyScope {
    pub fn new() -> Self {
        Self {}
    }
}

impl Scope for EmptyScope {
    fn statements(&self) -> StatementIter {
        Box::new(std::iter::empty::<_>())
    }

    fn run(
        &self,
        _cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, Vec<SymbolChild>, NodeList, EdgeList)> {
        Some((parent_resolution, vec![], NodeList(vec![]), EdgeList(vec![])))
    }
}
