//! Structured, non-fatal diagnostics produced during query execution.
//!
//! Runtime warnings (a selector matched nothing, a result was truncated, …) used
//! to be carried as `pest::error::Error<Rule>` and rendered via their terminal
//! `-->  | ^---` caret art. That art is right for a *parse error* but noise for an
//! agent reading markdown, and a pest error can't carry the structured extras a
//! good "did you mean?" needs. A [`Diagnostic`] keeps the location (via [`Span`],
//! which is self-describing — it owns its source) plus the real typed token and
//! any name suggestions, so every consumer (JSON, markdown) can present it well.
//!
//! Parse/build errors stay `pest::error::Error` — they are fatal and the caret is
//! the right presentation there.

use crate::span::Span;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticKind {
    /// A selector matched no symbols. Carries the typed token when it was a plain
    /// name (`"vfs_rea"`), which the statement layer uses to look up suggestions.
    NoMatch,
    /// Any other advisory note (constrained no-match, validation).
    Note,
    /// A result was truncated (verb limit, server result budget).  Never
    /// collapsed by the overlap-dedup in `gather_warnings`: truncation is
    /// acceptable, hidden truncation is not.
    Truncation,
    /// A default listing passed over folded symbols.  Like `Truncation` it is
    /// never collapsed: it is the only thing saying the answer is not all
    /// there is.  Unlike `Truncation` the answer is complete for what was
    /// asked -- nothing was cut.
    LeftOut,
    /// A relationship word listed nothing where a bare scope would have
    /// listed something: the word passed over rows drawn with another mark,
    /// or was asked of something that cannot be its source.  Never collapsed,
    /// for the reason `LeftOut` is not: it is the only thing saying that the
    /// empty answer is the word's doing.
    PassedOver,
}

impl DiagnosticKind {
    /// Exempt from the overlap-collapse in `gather_warnings`: each of these
    /// says something about ONE statement's answer that no warning of another
    /// statement can stand for.
    pub fn always_surfaces(&self) -> bool {
        matches!(
            self,
            DiagnosticKind::Truncation | DiagnosticKind::LeftOut | DiagnosticKind::PassedOver
        )
    }
}

#[derive(Debug, Clone)]
pub struct Diagnostic {
    /// Where in the query this diagnostic applies (owns its source text).
    pub span: Span,
    pub kind: DiagnosticKind,
    /// Clean, human/agent-readable message — no caret art.
    pub message: String,
    /// The bare typed name (e.g. `vfs_rea`) for a `NoMatch` on a plain-name
    /// selector; drives the suggestion lookup. `None` for globs / other selectors.
    pub token: Option<String>,
    /// Selectors that would match, filled by the statement-level enrichment
    /// pass for `NoMatch` diagnostics that have a `token`: each is query text
    /// to type in place of the selector that missed — the same name under the
    /// type it exists as (`data("x")`, `file("/a/b.c")`), or, for a typo, the
    /// nearest existing names.  Never the selector that was written.
    pub suggestions: Vec<String>,
    /// Set by enrichment when the typed name *exists somewhere visible* but was
    /// excluded by a filter/scope (so it is not a typo).  With `suggestions`,
    /// those are the spellings that find it; without, nothing but
    /// `project(...)` or the scope can be the reason.  Structured (not baked
    /// into `message`) so JSON consumers can distinguish the cases.
    pub name_exists: bool,
}

impl Diagnostic {
    /// A "selector matched nothing" diagnostic. `token` is the bare name to
    /// suggest against, or `None` when suggestions don't apply (glob / non-name).
    pub fn no_match(span: Span, token: Option<String>) -> Self {
        let message = format!("`{}` matched no symbols", span.as_str().trim());
        Diagnostic {
            span,
            kind: DiagnosticKind::NoMatch,
            message,
            token,
            suggestions: Vec::new(),
            name_exists: false,
        }
    }

    /// A generic advisory note (constrained no-match, validation).
    pub fn note(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            span,
            kind: DiagnosticKind::Note,
            message: message.into(),
            token: None,
            suggestions: Vec::new(),
            name_exists: false,
        }
    }

    /// A truncation notice (verb limit hit, server result budget hit).
    /// Exempt from warning overlap-dedup — it must always surface.
    pub fn truncation(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            span,
            kind: DiagnosticKind::Truncation,
            message: message.into(),
            token: None,
            suggestions: Vec::new(),
            name_exists: false,
        }
    }

    /// A limit cut this statement's answer.  The message quotes the statement,
    /// because a query can hold several and each is warned about separately:
    /// two such warnings used to read identically, down to "(line 1)".
    pub fn incomplete(span: Span) -> Self {
        let message = format!(
            "too many results in `{}`: this part of the graph is incomplete. Narrow it \
             with a name or glob, a type filter, `ignore(...)`, `project(...)`, or fewer hops",
            Self::quote(span.as_str())
        );
        Self::truncation(span, message)
    }

    /// A statement's text on one line, cut to a length that still tells it
    /// from its neighbours.
    /// "A listing that passes over a folded symbol says so."  `names` are the
    /// symbols the statement at `span` would also have listed were they not
    /// folded; `at_least` when the read that found them was itself cut.
    pub fn left_out(span: Span, names: &[String], at_least: bool) -> Self {
        const SHOWN: usize = 3;
        let mut listed = names
            .iter()
            .take(SHOWN)
            .map(|name| format!("`{name}`"))
            .collect::<Vec<_>>()
            .join(", ");
        if names.len() > SHOWN {
            if at_least {
                listed.push_str(" and more");
            } else {
                listed.push_str(&format!(" and {} more", names.len() - SHOWN));
            }
        }
        let one = names.len() == 1 && !at_least;
        let message = format!(
            "`{}` leaves out {}{} folded (macro-generated) symbol{}: {}. Add `unnest` to \
             list {}, or name {}",
            Self::quote(span.as_str()),
            if at_least { "at least " } else { "" },
            names.len(),
            if one { "" } else { "s" },
            listed,
            if one { "it" } else { "them" },
            if one { "it" } else { "one" },
        );
        Diagnostic {
            span,
            kind: DiagnosticKind::LeftOut,
            message,
            token: None,
            suggestions: Vec::new(),
            name_exists: false,
        }
    }

    /// "A filter that passed over what it would otherwise have answered says
    /// so."  `message` is the whole sentence; see
    /// `crate::statement::passed_over`, which is the one place that writes
    /// them.
    pub fn passed_over(span: Span, message: impl Into<String>) -> Self {
        Diagnostic {
            span,
            kind: DiagnosticKind::PassedOver,
            message: message.into(),
            token: None,
            suggestions: Vec::new(),
            name_exists: false,
        }
    }

    pub(crate) fn quote(text: &str) -> String {
        const MAX: usize = 48;
        let one_line = text.split_whitespace().collect::<Vec<_>>().join(" ");
        if one_line.chars().count() <= MAX {
            one_line
        } else {
            let head: String = one_line.chars().take(MAX).collect();
            format!("{} …", head.trim_end())
        }
    }

    /// Byte offset where the diagnostic's span starts — used to order/dedup.
    pub fn start(&self) -> usize {
        self.span.start()
    }

    /// Byte offset where the diagnostic's span ends.
    pub fn end(&self) -> usize {
        self.span.end()
    }
}

impl std::fmt::Display for Diagnostic {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{}", self.message)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn no_match_builds_message_from_span_text() {
        let span = Span::synthetic("\"vfs_rea\"");
        let d = Diagnostic::no_match(span, Some("vfs_rea".to_string()));
        assert_eq!(d.kind, DiagnosticKind::NoMatch);
        assert_eq!(d.message, "`\"vfs_rea\"` matched no symbols");
        assert_eq!(d.token.as_deref(), Some("vfs_rea"));
        assert!(d.suggestions.is_empty());
    }

    #[test]
    fn note_keeps_message() {
        let span = Span::synthetic("whatever");
        let d = Diagnostic::note(span, "custom note");
        assert_eq!(d.kind, DiagnosticKind::Note);
        assert_eq!(d.message, "custom note");
        assert!(d.token.is_none());
    }
}
