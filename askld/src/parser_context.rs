use crate::{
    command::Command,
    execution_state::RelationshipType,
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

/// Symbol type IDs for default symbol type inheritance
pub const SYMBOL_TYPE_FUNCTION: i32 = 1;
pub const SYMBOL_TYPE_FILE: i32 = 2;
pub const SYMBOL_TYPE_MODULE: i32 = 3;
pub const SYMBOL_TYPE_DIRECTORY: i32 = 4;

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
    /// The relationship type for statements created in this context.
    /// Set by @has verb, default is Refs.
    relationship_type: RefCell<RelationshipType>,
    /// Default symbol types for child scopes when no explicit type selector is present.
    /// Set by type selectors (@module, @function, etc.) to [parent_type, function_type].
    /// When a child scope has no explicit type selector, it will filter by these types.
    default_symbol_types: RefCell<Option<Vec<i32>>>,
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
            relationship_type: RefCell::new(RelationshipType::Refs),
            default_symbol_types: RefCell::new(None),
        })
    }

    pub fn derive(from: Rc<Self>, span: Span) -> Rc<Self> {
        Rc::new(Self {
            source: from.source.clone(),
            prev: Some(Rc::downgrade(&from)),
            alternative_context: RefCell::new(from.alternative_context.borrow().clone()),
            command: RefCell::new(from.command.borrow().derive(span)),
            scope_factory: None,
            // Inherit relationship type from parent context
            relationship_type: RefCell::new(from.get_relationship_type()),
            // Inherit default symbol types from parent context
            default_symbol_types: RefCell::new(from.get_default_symbol_types()),
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

    /// Set the relationship type for statements created in this context.
    pub fn set_relationship_type(&self, rel_type: RelationshipType) {
        *self.relationship_type.borrow_mut() = rel_type;
    }

    /// Get the relationship type for statements created in this context.
    pub fn get_relationship_type(&self) -> RelationshipType {
        *self.relationship_type.borrow()
    }

    /// Set the default symbol types for child scopes.
    /// When a child scope has no explicit type selector, it will filter by these types.
    pub fn set_default_symbol_types(&self, types: Vec<i32>) {
        *self.default_symbol_types.borrow_mut() = Some(types);
    }

    /// Get the default symbol types for this context.
    pub fn get_default_symbol_types(&self) -> Option<Vec<i32>> {
        self.default_symbol_types.borrow().clone()
    }

    /// Check if the command has a type selector verb.
    /// Type selectors are: @function, @file, @module, @directory
    pub fn has_type_selector(&self) -> bool {
        const TYPE_SELECTOR_NAMES: &[&str] = &["function", "file", "module", "directory"];
        self.command
            .borrow()
            .selectors()
            .any(|s| TYPE_SELECTOR_NAMES.contains(&s.name()))
    }
}
