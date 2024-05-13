use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::execution_context::ExecutionContext;
use crate::parser::{ParserContext, Rule};
use crate::statement::{build_empty_statement, build_statement, Statement};
use crate::symbols::SymbolRefs;
use core::fmt::Debug;
use pest::error::Error;

pub fn build_scope(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Box<dyn Statement>>, _> =
        pair.into_inner().map(|p| build_statement(ctx, p)).collect();

    let statements = statements?;
    let statements = if statements.len() == 0 {
        vec![build_empty_statement(ctx)]
    } else {
        statements
    };

    Ok(ctx.new_scope(statements))
}

#[derive(Debug)]
pub enum ScopeFactory {
    Children,
    Empty,
}

impl ScopeFactory {
    pub fn create(&self, statements: Vec<Box<dyn Statement>>) -> Box<dyn Scope> {
        match self {
            Self::Children => DefaultScope::new(statements),
            _ => panic!("Impossible: {:?}", self),
        }
    }
}

type StatementIter<'a> = Box<dyn Iterator<Item = &'a Box<dyn Statement + 'a>> + 'a>;
pub trait Scope: Debug {
    fn statements(&self) -> StatementIter;

    fn run(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<SymbolRefs>,
    ) -> Option<(SymbolRefs, NodeList, EdgeList)> {
        let mut res_nodes = NodeList::new();
        let mut res_edges = EdgeList::new();
        let mut res_symbols = SymbolRefs::new();

        let mut statement_symbols = symbols.clone();
        for statement in self.statements() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((resolved_symbols, scope_nodes, scope_edges)) =
                statement.execute(ctx, cfg, statement_symbols.clone())
            {
                for (sym, _) in resolved_symbols.iter() {
                    if let Some(statement_symbols) = &mut statement_symbols {
                        statement_symbols.remove(sym);
                    }
                }
                // res_nodes.0.push(symbol.clone());
                res_nodes.0.extend(scope_nodes.0.into_iter());
                res_edges.0.extend(scope_edges.0.into_iter());
                res_symbols.extend(resolved_symbols.into_iter());
            }
        }

        Some((res_symbols, res_nodes, res_edges))
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
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _symbols: Option<SymbolRefs>,
    ) -> Option<(SymbolRefs, NodeList, EdgeList)> {
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
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _symbols: Option<SymbolRefs>,
    ) -> Option<(SymbolRefs, NodeList, EdgeList)> {
        Some((SymbolRefs::new(), NodeList::new(), EdgeList::new()))
    }
}
