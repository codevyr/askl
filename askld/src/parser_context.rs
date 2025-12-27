use crate::{
    command::Command,
    scope::{DefaultScope, Scope},
    span::Span,
    statement::Statement,
    verb::{UnitVerb, Verb},
};
use anyhow::Result;
use std::{
    cell::RefCell,
    rc::{Rc, Weak},
    sync::Arc,
};

#[derive(Debug)]
pub enum ScopeFactory {
    Children,
    Empty,
}

impl ScopeFactory {
    pub fn create(&self, statements: Vec<Rc<Statement>>) -> Rc<dyn Scope> {
        let scope = match self {
            Self::Children => DefaultScope::new(statements),
            _ => panic!("Impossible: {:?}", self),
        };

        scope
    }
}

#[derive(Debug)]
pub struct ParserContext {
    source: Arc<String>,
    prev: Option<Weak<ParserContext>>,
    alternative_context: RefCell<Option<Weak<ParserContext>>>,
    scope_factory: Option<ScopeFactory>,
    command: RefCell<Command>,
}

impl ParserContext {
    pub fn new(source: Arc<String>, scope_factory: ScopeFactory) -> Rc<Self> {
        let command = Command::new(Span::entire(source.clone()));
        Rc::new(Self {
            source,
            prev: None,
            alternative_context: RefCell::new(None),
            command: RefCell::new(command),
            scope_factory: Some(scope_factory),
        })
    }

    pub fn derive(from: Rc<Self>, span: Span) -> Rc<Self> {
        Rc::new(Self {
            source: from.source.clone(),
            prev: Some(Rc::downgrade(&from)),
            alternative_context: RefCell::new(from.alternative_context.borrow().clone()),
            command: RefCell::new(from.command.borrow().derive(span)),
            scope_factory: None,
        })
    }

    pub fn set_scope_factory(&mut self, scope_factory: ScopeFactory) {
        self.scope_factory = Some(scope_factory);
    }

    pub fn new_scope(&self, statements: Vec<Rc<Statement>>) -> Rc<dyn Scope> {
        if let Some(factory) = &self.scope_factory {
            return factory.create(statements);
        }

        let factory = self
            .prev
            .as_ref()
            .expect("Should never try uninitialized factory")
            .upgrade()
            .unwrap();
        factory.new_scope(statements)
    }

    pub fn consume(&self, verb: Arc<dyn Verb>) -> Result<Option<Arc<dyn Verb>>> {
        let ctx = if let Some(alternative) = self.alternative_context.borrow().as_ref() {
            &alternative.upgrade().unwrap()
        } else {
            self
        };

        if !verb.update_context(ctx)? {
            Ok(Some(verb))
        } else {
            Ok(None)
        }
    }

    pub fn set_alternative_context(&self, alternative: Weak<ParserContext>) {
        *self.alternative_context.borrow_mut() = Some(alternative);
    }

    pub fn get_prev(&self) -> Option<Weak<ParserContext>> {
        self.prev.clone()
    }

    pub fn command(&self, span: Span) -> Command {
        let mut command = self.command.take();
        if command.selectors().count() == 0 {
            command.extend(UnitVerb::new(span));
        }
        command
    }

    pub fn extend_verb(&self, verb: Arc<dyn Verb>) {
        let ctx = if let Some(alternative) = self.alternative_context.borrow().as_ref() {
            &alternative.upgrade().unwrap()
        } else {
            self
        };

        ctx.command.borrow_mut().extend(verb);
    }

    pub fn source(&self) -> Arc<String> {
        self.source.clone()
    }
}
