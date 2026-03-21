use std::rc::Rc;

use crate::{parser::Rule, statement::Statement};

/// The role of a dependency in the execution state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyRole {
    Parent,
    Child,
    User,
}

/// The type of relationship to traverse when deriving selections.
/// - Refs: Reference-based traversal (calls/uses) via symbol_refs table
/// - Has: Containment-based traversal (composition) via offset_range containment
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RelationshipType {
    #[default]
    Refs,
    Has,
}

#[derive(Debug)]
pub struct StatementDependency {
    pub dependency: Rc<Statement>,
    pub dependency_role: DependencyRole,
}

impl StatementDependency {
    pub fn new(statement: Rc<Statement>, dependency_role: DependencyRole) -> Self {
        Self {
            dependency: statement,
            dependency_role,
        }
    }
}

/// A statement that depends on another statement's execution state.
/// StatementDependent is used to notify the dependent statement when the
/// statement it depends on has completed its execution and to update its
/// execution state accordingly.
#[derive(Debug, Clone)]
pub struct StatementDependent {
    pub statement: Rc<Statement>,
    pub dependency_role: DependencyRole,
    pub label: Option<String>,
}

impl StatementDependent {
    pub fn new(statement: Rc<Statement>, dependency_role: DependencyRole) -> Self {
        Self {
            statement,
            dependency_role,
            label: None,
        }
    }

    pub fn new_user(statement: Rc<Statement>, label: &str) -> Self {
        Self {
            statement,
            dependency_role: DependencyRole::User,
            label: Some(label.to_string()),
        }
    }
}

#[derive(Debug)]
pub struct ExecutionState {
    pub completed: bool,
    /// Statements that this state still depends on. We use this to determine
    /// when this state has its dependencies satisfied.
    pub dependencies: Vec<StatementDependency>,
    /// Statements that depend on this state. We notify them when this state is
    /// completed.
    pub dependents: Vec<StatementDependent>,
    /// Weak unit statements do not constrain the selection of their dependencies.
    pub weak: bool,
    /// Warnings that occurred during the execution of this state.
    pub warnings: Vec<pest::error::Error<Rule>>,
}

impl ExecutionState {
    pub fn new() -> Self {
        Self {
            completed: false,
            dependencies: vec![],
            dependents: vec![],
            weak: false,
            warnings: vec![],
        }
    }
}
