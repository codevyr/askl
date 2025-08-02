use std::rc::{Rc, Weak};

use crate::{scope::StatementIter, statement::Statement};

pub trait Hierarchy {
    /// Returns the parent of the current object.
    fn parent(&self) -> Option<Weak<Statement>>;

    /// Sets the parent of the current object.
    /// Needs interior mutability in the implementor
    fn set_parent(&self, parent: Weak<Statement>);

    /// Returns all children of the current object.
    fn children(&self) -> StatementIter;
}

pub fn populate_parents(node: &Rc<Statement>) {
    for child in node.children() {
        child.set_parent(Rc::downgrade(&node));
        populate_parents(&child);
    }
}
