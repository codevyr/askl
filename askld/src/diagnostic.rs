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
    /// Nearest existing symbol names, filled by the statement-level enrichment
    /// pass for `NoMatch` diagnostics that have a `token`.
    pub suggestions: Vec<String>,
    /// Set by enrichment when the typed name *exists somewhere visible* but was
    /// excluded by a filter/scope (so it is not a typo). Mutually exclusive with
    /// `suggestions`; the renderer phrases the two differently. Structured (not
    /// baked into `message`) so JSON consumers can distinguish the cases.
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
