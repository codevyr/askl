use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::statement::{build_empty_statement, build_statement, Statement};
use anyhow::Result;
use async_trait::async_trait;
use core::fmt::Debug;
use std::cell::RefCell;
use std::rc::{Rc, Weak};

pub fn build_scope(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Rc<dyn Scope>, pest::error::Error<Rule>> {
    let scope_span = Span::from_pest(pair.as_span(), ctx.source());
    let statements: Result<Vec<Rc<Statement>>, _> = pair
        .into_inner()
        .map(|p| build_statement(ctx.clone(), p))
        .collect();

    let statements = statements?;
    let statements = if statements.is_empty() {
        vec![build_empty_statement(ctx.clone(), scope_span)]
    } else {
        statements
    };
    let scope = ctx.new_scope(statements);

    Ok(scope)
}

/// Visit every statement included in the scope recursively
pub fn visit<'a, 'b, F, E>(scope: Rc<dyn Scope>, func: &'b mut F) -> Result<(), E>
where
    F: FnMut(Rc<Statement>) -> Result<bool, E>,
{
    for statement in scope.statements() {
        if !func(statement.clone())? {
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
