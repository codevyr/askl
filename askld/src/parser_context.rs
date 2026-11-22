use crate::{
    command::Command,
    relationship::Relationship,
    scope::{DefaultScope, Scope},
    span::Span,
    statement::Statement,
    verb::{EphemeralOps, UnitVerb, Verb},
};
use anyhow::Result;
use index::edge::Edge;
use std::{
    cell::{Cell, RefCell},
    rc::{Rc, Weak},
    sync::Arc,
};

// Re-export symbol type constants from the index crate (single source of truth)
pub use index::db_diesel::{
    INSTANCE_TYPE_BUILD, INSTANCE_TYPE_CONTAINMENT, INSTANCE_TYPE_DECLARATION,
    INSTANCE_TYPE_DEFINITION, INSTANCE_TYPE_EXPANSION, INSTANCE_TYPE_HEADER,
    INSTANCE_TYPE_SENTINEL, INSTANCE_TYPE_SOURCE, SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY,
    SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE, SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO,
    SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
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
    /// The relationship of statements created in this context to their
    /// parent: the edge, and the words that asked for it.  Set by a
    /// relationship word or an expression of them.
    relationship_type: RefCell<Relationship>,
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
    /// `inherit(relationship)` was written on THIS statement.  Kept apart
    /// from the flag above, which is also true where a relationship is only
    /// passing through: a word written here must know whether its own
    /// statement asked for it to be carried, whichever of the two came first.
    carries_relationship: Cell<bool>,
    /// `inherit(type)` was written on THIS statement.
    carries_type: Cell<bool>,
    /// Shared ephemeral ops vec for the current layer block.
    /// Set by LayerVerb::update_context(), inherited by derive() so child
    /// contexts push ops to the same vec.
    eph_ops: RefCell<Option<EphemeralOps>>,
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
            relationship_type: RefCell::new(Relationship::unwritten(Edge::EVERY_REFERENCE)),
            default_symbol_types: RefCell::new(None),
            has_relationship_modifier: RefCell::new(false),
            inherit_relationship_modifier: RefCell::new(false),
            carries_relationship: Cell::new(false),
            carries_type: Cell::new(false),
            eph_ops: RefCell::new(None),
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
            inherit_relationship_modifier: RefCell::new(
                *from.inherit_relationship_modifier.borrow(),
            ),
            // What a statement says of itself is not said of the next one.
            carries_relationship: Cell::new(false),
            carries_type: Cell::new(false),
            eph_ops: RefCell::new(from.get_eph_ops()),
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

    /// True once `preamble` has redirected this context's verbs to the global
    /// one — including inside a `preamble { … }` scope, since `derive`
    /// carries the redirection down.  A verb landing here configures every
    /// statement in the query rather than joining a statement of its own,
    /// which is why `install_verb` (`crate::verb`) refuses selectors
    /// at this point.
    pub fn redirects_to_global(&self) -> bool {
        self.alternative_context.borrow().is_some()
    }

    pub fn get_prev(&self) -> Option<Weak<ParserContext>> {
        self.prev.clone()
    }

    /// The global context, the only one with no predecessor.  Verbs reach it
    /// through a `preamble` and nothing else.
    ///
    /// A PREAMBLE CARRIES WHAT IT WRITES.  A word governs the statement it
    /// is written in, and `inherit(kind)` carries it into the scopes below;
    /// a preamble is no statement and has no braces, so what it writes is
    /// written for every statement after it, at every depth, until one
    /// writes its own.  It needs no `inherit(...)` to say so.
    fn is_global(&self) -> bool {
        self.prev.is_none()
    }

    /// Finalize this context into the command of the statement being built.
    ///
    /// A context that redirects to global belongs to a preamble directive
    /// (or to a statement inside a `preamble { … }` scope).  Its verbs were
    /// installed on the global context and apply to every statement, so the
    /// directive keeps no command of its own — including the inheritance
    /// [`Self::derive`] seeded it with, which carries the verbs of *earlier*
    /// preambles.  Dropping it is what makes "directives are unit-only by
    /// construction" true rather than merely intended: a directive is not a
    /// statement and must never be held to the anchor rule
    /// (see [`Command::demands_anchoring`]).
    pub fn command(&self, span: Span) -> Command {
        let inherited = self.command.take();
        let mut command = if self.redirects_to_global() {
            Command::new(span.clone())
        } else {
            inherited
        };
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
        if ctx.is_global() {
            // A preamble carries what it writes.
            ctx.command.borrow_mut().carry_everything();
        } else if ctx.carries_type.get() {
            // `inherit(type)` written before the type it carries.
            ctx.command.borrow_mut().carry_type();
        }
    }

    /// See [`Command::said_twice`].
    pub fn said_twice(&self) -> Option<((usize, usize), (usize, usize))> {
        self.command.borrow().said_twice()
    }

    pub fn source(&self) -> Arc<String> {
        self.source.clone()
    }

    /// Set the relationship type explicitly from a verb (@has or @refs).
    /// Marks that a relationship modifier was used, affecting child scope behavior.
    pub fn set_relationship_type_explicit(&self, rel_type: Relationship) {
        *self.relationship_type.borrow_mut() = rel_type;
        *self.has_relationship_modifier.borrow_mut() = true;
    }

    /// Set the relationship type as a default/inherited value.
    /// Does NOT mark as explicit modifier - used for scope transitions and inheritance.
    pub fn set_relationship_type_default(&self, rel_type: Relationship) {
        *self.relationship_type.borrow_mut() = rel_type;
    }

    /// Get the relationship type for statements created in this context.
    pub fn get_relationship_type(&self) -> Relationship {
        self.relationship_type.borrow().clone()
    }

    /// Check if a relationship modifier (@has or @refs) was explicitly used.
    pub fn has_relationship_modifier(&self) -> bool {
        *self.has_relationship_modifier.borrow()
    }

    /// A relationship written on this statement, by a word or an expression
    /// of them.
    ///
    /// A WORD GOVERNS THE STATEMENT IT IS WRITTEN IN.  It describes the edge
    /// into this statement's braces, and it ends whatever an enclosing
    /// statement was carrying down here.  It reaches the scopes below only if
    /// this statement also says `inherit(relationship)`, before the word or
    /// after it -- or if it is written in a preamble ([`Self::is_global`]).
    pub fn write_relationship(&self, rel_type: Relationship) {
        self.set_relationship_type_explicit(rel_type);
        *self.inherit_relationship_modifier.borrow_mut() =
            self.carries_relationship.get() || self.is_global();
    }

    /// `inherit(relationship)`: the relationship in force for this
    /// statement's braces is carried into every scope below, until a
    /// statement writes its own.  With no word in force that is the bare
    /// scope, which the scopes below are anyway; and the word may be still
    /// to come ([`Self::write_relationship`]).
    pub fn carry_relationship(&self) {
        self.carries_relationship.set(true);
        *self.inherit_relationship_modifier.borrow_mut() = true;
    }

    /// `inherit(type)`: the type this statement writes is carried into every
    /// scope below, until a statement writes its own.  The type may be
    /// written already or still to come ([`Self::extend_verb`]).
    pub fn carry_type(&self) {
        self.carries_type.set(true);
        self.command.borrow_mut().carry_type();
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

    /// Set the shared ephemeral ops vec (called by LayerVerb::update_context).
    pub(crate) fn set_eph_ops(&self, ops: EphemeralOps) {
        *self.eph_ops.borrow_mut() = Some(ops);
    }

    /// Get the shared ephemeral ops vec (clones the Option<Arc>).
    pub(crate) fn get_eph_ops(&self) -> Option<EphemeralOps> {
        self.eph_ops.borrow().clone()
    }
}
