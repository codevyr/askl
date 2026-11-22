//! TRANSITIONAL, for one release: `has` and `refs` no longer reach nested
//! scopes.
//!
//! To remove it, delete this module, its one call in `parse.rs`, and
//! `DiagnosticKind::NoLongerCarried`.  Nothing else knows of it.
//!
//! A WORD GOVERNS THE STATEMENT IT IS WRITTEN IN; `inherit(kind)` CARRIES IT.
//! `has` and `refs` used to be carried into every scope below them without
//! being asked to: `file has { "foo" { "bar" } }` asked whether `foo`
//! CONTAINS `bar`.  It now asks whether `foo` contains, refers to, declares
//! or holds it, and answers differently with nothing wrong in the query.  So
//! a query written as if the word still carried is told what changed:
//!
//! > `has` no longer reaches nested scopes; write `inherit(relationship)` to
//! > carry it
//!
//! It is said of a statement when all of these hold, and once however many
//! nested scopes there are:
//!
//!  - the statement's relationship is the single word `has` or `refs`.  The
//!    other six words never shipped carrying, nor did an expression of
//!    words, and `derive(...)` says for itself whether it is carried;
//!  - a statement in its braces has braces of its own and no relationship
//!    for them: the place the word used to reach.  (Where the statement says
//!    `inherit(relationship)` there is no such place: the word is there.)
//!
//! It is decided on the query alone, when the statement is built: whether
//! the answer differs is not looked at.

use std::rc::Rc;

use super::Statement;
use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::relationship::Relationship;
use crate::span::Span;

/// What to say of the statement whose braces hold `nested`, if anything.
///
/// `written` is the relationship the statement's own words asked for, and
/// `at` where they stand.
pub(super) fn notice(
    written: &Relationship,
    at: Span,
    nested: impl Iterator<Item = Rc<Statement>>,
) -> Option<Diagnostic> {
    let [word] = written.words() else {
        return None;
    };
    if !matches!(word.text(), "has" | "refs") {
        return None;
    }
    let mut reached_before = nested.filter(|statement| {
        statement
            .scope()
            .statements()
            .next()
            .is_some_and(|below| below.relationship().words().is_empty())
    });
    reached_before.next()?;
    let message = format!(
        "`{}` no longer reaches nested scopes; write `inherit(relationship)` to carry it",
        word.text()
    );
    Some(Diagnostic {
        span: at,
        kind: DiagnosticKind::NoLongerCarried,
        message,
        token: None,
        suggestions: Vec::new(),
        name_exists: false,
    })
}
