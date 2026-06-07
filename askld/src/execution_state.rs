use std::rc::Rc;

use crate::{parser::Rule, statement::Statement};

/// The role of a dependency in the execution state.
///
/// `Copy` was dropped to make room for `PreSeed`'s optional label.
/// Most call sites pattern-match by reference (`&dep.dependency_role`)
/// or clone explicitly, so the impact is limited.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DependencyRole {
    Parent,
    Child,
    User,
    /// "This dependency must apply its selection before my
    /// `compute_selected` starts."  Used in two cases:
    ///
    /// 1. **Sibling ordering** (`label = None`): added between
    ///    top-level statements where at least one creates an
    ///    ephemeral layer, so the second statement's `eph` capture
    ///    reflects the first's materialised layer.
    /// 2. **Label resolution** (`label = Some(name)`): added when a
    ///    layer-creating verb has a `@label` argument, so
    ///    `compute_roots` can resolve the label to symbol IDs at
    ///    push time and pass them through `LabelResolutions`.
    ///
    /// Both cases require the same action at the scheduling layer:
    /// drain pending before pushing.  No selection-data notification
    /// flows along these edges — they're pure ordering (with optional
    /// resolution as a side-effect).
    PreSeed { label: Option<Rc<str>> },
}

/// Whether a dependency must be satisfied before any output can be produced,
/// or whether any one satisfied dep in the set is enough to enable initial output.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DependencyKind {
    /// Statement cannot produce any output until this dep has a selection.
    Necessary,
    /// Any one satisfied sufficient dep enables initial output; others constrain further.
    Sufficient,
}

/// The type of relationship to traverse when deriving selections.
/// Bitflag newtype: composable via `|`, testable via `contains()`.
/// - REFS: Reference-based traversal (calls/uses) via symbol_refs table
/// - HAS: Containment-based traversal (composition) via offset_range containment
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RelationshipType(u8);

impl RelationshipType {
    pub(crate) const EMPTY: Self = Self(0);
    pub const REFS: Self = Self(0b01);
    pub const HAS: Self = Self(0b10);

    pub fn contains(self, other: Self) -> bool {
        self.0 & other.0 == other.0
    }
}

impl Default for RelationshipType {
    fn default() -> Self {
        Self::REFS
    }
}

impl std::ops::BitOr for RelationshipType {
    type Output = Self;
    fn bitor(self, rhs: Self) -> Self {
        Self(self.0 | rhs.0)
    }
}

#[derive(Debug)]
pub struct StatementDependency {
    pub dependency: Rc<Statement>,
    pub dependency_role: DependencyRole,
    pub kind: DependencyKind,
}

impl StatementDependency {
    pub fn new(statement: Rc<Statement>, dependency_role: DependencyRole) -> Self {
        Self {
            dependency: statement,
            dependency_role,
            kind: DependencyKind::Sufficient,
        }
    }

    pub fn new_with_kind(
        statement: Rc<Statement>,
        dependency_role: DependencyRole,
        kind: DependencyKind,
    ) -> Self {
        Self {
            dependency: statement,
            dependency_role,
            kind,
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
            dependencies: vec![],
            dependents: vec![],
            weak: false,
            warnings: vec![],
        }
    }
}
