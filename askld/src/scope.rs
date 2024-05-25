use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::execution_context::ExecutionContext;
use crate::parser::{ParserContext, Rule};
use crate::statement::{build_empty_statement, build_statement, Statement};
use async_trait::async_trait;
use index::symbols::{SymbolRefs, SymbolId, DeclarationId, DeclarationRefs};
use core::fmt::Debug;
use pest::error::Error;
use std::collections::HashSet;

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

/// Scope executes statements. One of the algorithmic difficulties is that each
/// statement must be executed exactly once. Therefore, the statement must be
/// able to accept multiple symbols at once.
#[async_trait(?Send)]
pub trait Scope: Debug {
    fn statements(&self) -> StatementIter;

    async fn run(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<DeclarationRefs>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
        let mut res_nodes = NodeList::new();
        let mut res_edges = EdgeList::new();
        let mut res_symbols = DeclarationRefs::new();

        let mut ignored_ids = HashSet::new();

        for statement in self.statements() {
            // Iterate through all the statements in the scope or subscope of
            // the query
            if let Some((resolved_symbols, scope_nodes, scope_edges)) =
                statement.execute(ctx, cfg, symbols.clone(), &ignored_ids).await
            {
                ignored_ids.extend(resolved_symbols.iter().map(|(id, _)| *id));
                // res_nodes.0.push(symbol.clone());
                res_nodes.0.extend(scope_nodes.0.into_iter());
                res_edges.0.extend(scope_edges.0.into_iter());
                res_symbols.extend(resolved_symbols.into_iter());
            }
        }

        Some((res_symbols, res_nodes, res_edges))
    }

    async fn run_symbols(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: HashSet<DeclarationId>,
    ) -> Option<(HashSet<DeclarationId>, NodeList, EdgeList)> {
        let mut res_nodes = NodeList::new();
        let mut res_edges = EdgeList::new();
        let mut res_symbols = HashSet::new();

        let mut ignored_ids = HashSet::new();
        for statement in self.statements() {
            // Iterate through all the statements in the scope or subscope of
            // the query

            let statement_refs = symbols
                .iter()
                .map(|id| (*id, HashSet::new()))
                .collect();

            if let Some((resolved_symbols, scope_nodes, scope_edges)) =
                statement.execute(ctx, cfg, Some(statement_refs), &ignored_ids).await
            {
                res_symbols.extend(
                    symbols
                        .iter()
                        .filter(|r| resolved_symbols.contains_key(r))
                        .map(|r| r.clone()),
                );
                ignored_ids.extend(resolved_symbols.iter().map(|(id, _)| *id));
                res_nodes.0.extend(scope_nodes.0.into_iter());
                res_edges.0.extend(scope_edges.0.into_iter());
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

#[async_trait(?Send)]
impl Scope for GlobalScope {
    fn statements(&self) -> StatementIter {
        Box::new(self.0.iter())
    }

    async fn run(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _symbols: Option<DeclarationRefs>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
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

#[async_trait(?Send)]
impl Scope for EmptyScope {
    fn statements(&self) -> StatementIter {
        Box::new(std::iter::empty::<_>())
    }

    async fn run(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _symbols: Option<DeclarationRefs>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
        Some((DeclarationRefs::new(), NodeList::new(), EdgeList::new()))
    }
}
