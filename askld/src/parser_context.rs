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

// Re-export symbol type constants from the index crate (single source of truth)
pub use index::db_diesel::{
    SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FILE, SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE, SYMBOL_TYPE_DATA, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_FIELD,
    INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_DECLARATION, INSTANCE_TYPE_EXPANSION, INSTANCE_TYPE_SENTINEL, INSTANCE_TYPE_CONTAINMENT, INSTANCE_TYPE_SOURCE, INSTANCE_TYPE_HEADER, INSTANCE_TYPE_BUILD,
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
    /// The relationship type for statements created in this context.
    /// Set by @has verb, default is Refs.
    relationship_type: RefCell<RelationshipType>,
    /// Default symbol types for child scopes when no explicit type selector is present.
    /// Set by type selectors (@module, @function, etc.) to [parent_type, function_type].
    /// When a child scope has no explicit type selector, it will filter by these types.
    default_symbol_types: RefCell<Option<Vec<i32>>>,
    /// Whether a relationship modifier (@has or @refs) was explicitly used in this context.
    /// Used to distinguish inherited @has from explicit @has in nested scopes.
    has_relationship_modifier: RefCell<bool>,
    /// Whether the relationship modifier should be inherited by all descendants.
    /// When true, derive() copies both has_relationship_modifier and inherit_relationship_modifier
    /// to child contexts, so the relationship type propagates to all descendants.
    inherit_relationship_modifier: RefCell<bool>,
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
            relationship_type: RefCell::new(RelationshipType::REFS),
            default_symbol_types: RefCell::new(None),
            has_relationship_modifier: RefCell::new(false),
            inherit_relationship_modifier: RefCell::new(false),
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
            // Don't inherit - each context tracks its own modifiers
            // UNLESS inherit_relationship_modifier is set, in which case propagate both flags
            has_relationship_modifier: RefCell::new(*from.inherit_relationship_modifier.borrow()),
            inherit_relationship_modifier: RefCell::new(*from.inherit_relationship_modifier.borrow()),
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
            command.extend(UnitVerb::new(span.clone()));
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

    /// Set the relationship type explicitly from a verb (@has or @refs).
    /// Marks that a relationship modifier was used, affecting child scope behavior.
    pub fn set_relationship_type_explicit(&self, rel_type: RelationshipType) {
        *self.relationship_type.borrow_mut() = rel_type;
        *self.has_relationship_modifier.borrow_mut() = true;
    }

    /// Set the relationship type as a default/inherited value.
    /// Does NOT mark as explicit modifier - used for scope transitions and inheritance.
    pub fn set_relationship_type_default(&self, rel_type: RelationshipType) {
        *self.relationship_type.borrow_mut() = rel_type;
    }

    /// Get the relationship type for statements created in this context.
    pub fn get_relationship_type(&self) -> RelationshipType {
        *self.relationship_type.borrow()
    }

    /// Check if a relationship modifier (@has or @refs) was explicitly used.
    pub fn has_relationship_modifier(&self) -> bool {
        *self.has_relationship_modifier.borrow()
    }

    /// Set the relationship type and mark it for propagation to all descendants.
    /// Combines set_relationship_type_explicit + set_inherit_relationship_modifier(true).
    pub fn set_relationship_type_inherited(&self, rel_type: RelationshipType) {
        self.set_relationship_type_explicit(rel_type);
        self.set_inherit_relationship_modifier(true);
    }

    /// Set the inherit_relationship_modifier flag.
    /// When true, derive() will propagate the relationship modifier to all descendants.
    pub fn set_inherit_relationship_modifier(&self, val: bool) {
        *self.inherit_relationship_modifier.borrow_mut() = val;
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

    /// Check if the command has a verb that provides its own type filtering,
    /// making the automatic DefaultTypeFilter unnecessary.
    /// This covers: @func, @file, @mod, @dir, @filter("type", ...)
    pub fn has_type_selector(&self) -> bool {
        self.command.borrow().has_suppress_default_type_filter()
    }

}
