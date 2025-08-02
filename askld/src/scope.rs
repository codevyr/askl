use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::execution_context::ExecutionContext;
use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::statement::{build_empty_statement, build_statement, Statement};
use async_trait::async_trait;
use core::fmt::Debug;
use index::symbols::{DeclarationId, DeclarationRefs};
use pest::error::Error;
use std::cell::RefCell;
use std::collections::HashSet;
use std::rc::{Rc, Weak};

pub fn build_scope(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Rc<dyn Scope>, Error<Rule>> {
    let statements: Result<Vec<Rc<Statement>>, _> = pair
        .into_inner()
        .map(|p| build_statement(ctx.clone(), p))
        .collect();

    let statements = statements?;
    let statements = if statements.is_empty() {
        vec![build_empty_statement(ctx.clone())]
    } else {
        statements
    };
    let scope = ctx.new_scope(statements);

    Ok(scope)
}

/// Visit every statement included in the scope recursively
pub fn visit<'a, 'b, F>(scope: Rc<dyn Scope>, func: &'b mut F) -> Result<(), Error<Rule>>
where
    F: FnMut(Rc<Statement>) -> bool,
{
    for statement in scope.statements() {
        if !func(statement.clone()) {
            return Ok(());
        }
        visit(statement.scope(), func)?;
    }
    Ok(())
}

pub type StatementIter = Box<dyn Iterator<Item = Rc<Statement>>>;

/// Scope executes statements. One of the algorithmic difficulties is that each
/// statement must be executed exactly once. Therefore, the statement must be
/// able to accept multiple symbols at once.
#[async_trait(?Send)]
pub trait Scope: Debug + Hierarchy {
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
            if let Some((resolved_symbols, scope_nodes, scope_edges)) = statement
                .execute(ctx, cfg, symbols.clone(), &ignored_ids)
                .await
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

            let statement_refs = symbols.iter().map(|id| (*id, HashSet::new())).collect();

            if let Some((resolved_symbols, scope_nodes, scope_edges)) = statement
                .execute(ctx, cfg, Some(statement_refs), &ignored_ids)
                .await
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
pub struct DefaultScope {
    parent: RefCell<Option<Weak<Statement>>>,
    children: Vec<Rc<Statement>>,
}

impl DefaultScope {
    pub fn new(statements: Vec<Rc<Statement>>) -> Rc<dyn Scope> {
        Rc::new(Self {
            parent: RefCell::new(None),
            children: statements,
        })
    }
}

impl Scope for DefaultScope {
    fn statements(&self) -> StatementIter {
        Box::new(self.children.clone().into_iter())
    }
}

impl Hierarchy for DefaultScope {
    fn parent(&self) -> Option<Weak<Statement>> {
        self.parent.borrow().clone()
    }

    fn set_parent(&self, parent: Weak<Statement>) {
        *self.parent.borrow_mut() = Some(parent);
    }

    fn children(&self) -> StatementIter {
        self.statements()
    }
}

#[derive(Debug)]
pub struct EmptyScope {
    parent: RefCell<Option<Weak<Statement>>>,
}

impl EmptyScope {
    pub fn new() -> Self {
        Self {
            parent: RefCell::new(None),
        }
    }
}

#[async_trait(?Send)]
impl Scope for EmptyScope {
    fn statements(&self) -> StatementIter {
        Box::new(std::iter::empty())
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

impl Hierarchy for EmptyScope {
    fn parent(&self) -> Option<Weak<Statement>> {
        self.parent.borrow().clone()
    }

    fn set_parent(&self, parent: Weak<Statement>) {
        *self.parent.borrow_mut() = Some(parent);
    }

    fn children(&self) -> StatementIter {
        self.statements()
    }
}
