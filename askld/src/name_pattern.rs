//! Typed selector name patterns.
//!
//! Every verb argument that names symbols is a [`NamePattern`], built once at
//! verb construction from a typed parser [`Value`]:
//!
//! - plain strings (`"foo"`, `"(*Kubelet).Run"`) keep exact-match semantics —
//!   the index-side normalization strips characters like `*` and `(` on both
//!   sides, so pasted symbol names match as-is;
//! - glob strings (`g"foo*"`) opt into wildcard matching, where `*` matches
//!   any run of characters.
//!
//! Glob strings are reparsed with a dedicated pest grammar
//! (`name_pattern.pest`) into tokens; validation happens here, at
//! construction, so query-time filter building is infallible.
//!
//! Glob patterns are **anchored**: the whole leaf name (or, for compound
//! patterns, the whole symbol name) must match. "Contains" is expressed with
//! explicit wildcards (`g"*alloc*"`), never implied.

use anyhow::{bail, Result};
use index::db_diesel::{CompositeFilter, FullNameGlobMixin, GlobNameMixin, GlobPiece};
use pest::Parser;
use pest_derive::Parser;

use crate::parser::{StringKind, Value};

#[derive(Parser)]
#[grammar = "name_pattern.pest"]
struct NamePatternParser;

/// One token of a reparsed glob string.
#[derive(Debug, Clone, PartialEq)]
pub enum NameToken {
    /// A literal run of characters (no wildcards, no separators).
    Literal(String),
    /// The '*' glob wildcard: any run of characters, including empty.
    Wildcard,
    /// A compound-name separator: '.', '/' or ':'.
    Separator(char),
}

/// A symbol-name argument with its matching semantics resolved at parse time.
#[derive(Debug, Clone)]
pub enum NamePattern {
    /// Exact matching (plain strings) — compiled via the pre-existing
    /// leaf/compound/absolute paths, unchanged.
    Exact(String),
    /// Glob matching (`g"..."` strings).
    Glob(GlobPattern),
}

/// A validated glob pattern.
///
/// Invariant: the compiled SQL pattern always contains literal content — a
/// pattern whose literals would all be erased (by leaf-name normalization or
/// because there are none) is rejected at parse, so no glob can degrade to a
/// match-everything `%` scan.
#[derive(Debug, Clone)]
pub struct GlobPattern {
    raw: String,
    /// Leaf-route pieces: literals with `Separator('.')` folded to a literal
    /// dot (file/dir names), hard separators omitted (unreachable — patterns
    /// containing them always take the full-name route).
    pieces: Vec<GlobPiece>,
    /// Contains '/' or ':': always routes to full-name matching.
    has_hard_separator: bool,
    /// Contains '.': routes to full-name matching when '.' separates path
    /// labels for the symbol type at hand.
    has_dot: bool,
}

impl NamePattern {
    pub fn from_value(value: &Value) -> Result<NamePattern> {
        match value {
            Value::Str {
                kind: StringKind::Plain,
                text,
            } => Ok(NamePattern::Exact(text.clone())),
            Value::Str {
                kind: StringKind::Glob,
                text,
            } => Ok(NamePattern::Glob(GlobPattern::parse(text)?)),
            // A name is text.  An integer or boolean literal in name position
            // is a type error, not a name that happens to look numeric —
            // `func(42)` is a mistake, `func("42")` is the way to say it.
            other => anyhow::bail!(
                "a name pattern expects a string, found {}",
                other.describe()
            ),
        }
    }
}

impl std::fmt::Display for NamePattern {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            NamePattern::Exact(name) => write!(f, "\"{}\"", name),
            NamePattern::Glob(glob) => write!(f, "g\"{}\"", glob.raw),
        }
    }
}

impl GlobPattern {
    /// The pattern text as written (without the g"" wrapper).
    pub fn raw(&self) -> &str {
        &self.raw
    }

    fn parse(raw: &str) -> Result<GlobPattern> {
        let tokens = parse_name_pattern(raw);
        let has_hard_separator = tokens
            .iter()
            .any(|t| matches!(t, NameToken::Separator('/') | NameToken::Separator(':')));
        let has_dot = tokens
            .iter()
            .any(|t| matches!(t, NameToken::Separator('.')));

        // Reject patterns whose compiled SQL would contain no literal
        // content.  The full-name route binds raw literals, so any non-empty
        // literal counts; the leaf route normalizes literals down to
        // [A-Za-z0-9_], so only characters surviving that allowlist count.
        let has_content = if has_hard_separator {
            tokens
                .iter()
                .any(|t| matches!(t, NameToken::Literal(s) if !s.is_empty()))
        } else {
            tokens.iter().any(|t| {
                matches!(t, NameToken::Literal(s)
                    if s.chars().any(|c| c.is_ascii_alphanumeric() || c == '_'))
            })
        };
        if !has_content {
            bail!(
                "glob pattern g\"{}\" must contain at least one literal character \
                 that can match a symbol name (a bare wildcard would select \
                 every symbol)",
                raw
            );
        }

        let pieces = tokens
            .iter()
            .filter_map(|t| match t {
                NameToken::Wildcard => Some(GlobPiece::Star),
                NameToken::Literal(s) => Some(GlobPiece::Lit(s.clone())),
                // Files/dirs treat '.' as part of the name; on the leaf
                // route it folds into a literal.  Hard separators never
                // reach the leaf route.
                NameToken::Separator('.') => Some(GlobPiece::Lit(".".to_string())),
                NameToken::Separator(_) => None,
            })
            .collect();

        Ok(GlobPattern {
            raw: raw.to_string(),
            pieces,
            has_hard_separator,
            has_dot,
        })
    }

    /// Compile to a filter.  Leaf globs (no separators) match `leaf_name`
    /// with normalized literals; patterns containing separators match the
    /// whole symbol name.  `leaf_anchored = false` (`match="contains"`)
    /// wraps the leaf pattern in implicit wildcards.
    pub fn filter(&self, dot_is_separator: bool, leaf_anchored: bool) -> CompositeFilter {
        if self.has_hard_separator || (self.has_dot && dot_is_separator) {
            return self.full_name_filter();
        }
        CompositeFilter::leaf(GlobNameMixin::new(
            &self.pieces,
            dot_is_separator,
            leaf_anchored,
        ))
    }

    /// Compile to an anchored glob over the full symbol name.  Used for
    /// compound patterns and absolute-path file/dir patterns.
    pub fn full_name_filter(&self) -> CompositeFilter {
        CompositeFilter::leaf(FullNameGlobMixin::new(&self.raw))
    }
}

/// Reparse the raw content of a glob string into tokens.  The grammar is
/// total over selector strings, so this cannot fail.
fn parse_name_pattern(input: &str) -> Vec<NameToken> {
    let pattern = NamePatternParser::parse(Rule::pattern, input)
        .expect("name pattern grammar is total over selector strings")
        .next()
        .expect("pattern rule always yields one pair");
    pattern
        .into_inner()
        .filter_map(|pair| match pair.as_rule() {
            Rule::wildcard => Some(NameToken::Wildcard),
            Rule::separator => Some(NameToken::Separator(
                pair.as_str().chars().next().expect("separator is one char"),
            )),
            Rule::literal => Some(NameToken::Literal(pair.as_str().to_string())),
            Rule::EOI => None,
            rule => unreachable!("Unknown rule: {:#?}", rule),
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn lit(s: &str) -> NameToken {
        NameToken::Literal(s.to_string())
    }

    #[test]
    fn tokenizes_glob_pattern() {
        assert_eq!(
            parse_name_pattern("*func*bar*"),
            vec![
                NameToken::Wildcard,
                lit("func"),
                NameToken::Wildcard,
                lit("bar"),
                NameToken::Wildcard,
            ]
        );
    }

    #[test]
    fn tokenizes_compound_name() {
        assert_eq!(
            parse_name_pattern("a.b"),
            vec![lit("a"), NameToken::Separator('.'), lit("b")]
        );
    }

    #[test]
    fn tokenizes_hostile_input_as_literal() {
        assert_eq!(parse_name_pattern(r#"%_\'""#), vec![lit(r#"%_\'""#)]);
    }

    #[test]
    fn rejects_patterns_without_literals() {
        assert!(GlobPattern::parse("*").is_err());
        assert!(GlobPattern::parse("**").is_err());
        assert!(GlobPattern::parse("").is_err());
        assert!(GlobPattern::parse("*a*").is_ok());
    }

    #[test]
    fn rejects_literals_erased_by_normalization() {
        // '-', '(' etc. are stripped from leaf names at index time; a
        // pattern with only such literals would compile to a bare '%'.
        assert!(GlobPattern::parse("-*").is_err());
        assert!(GlobPattern::parse("(*)").is_err());
        assert!(GlobPattern::parse("--*--").is_err());
        // '.'-only patterns have no matchable literal either.
        assert!(GlobPattern::parse("*.*").is_err());
        // With a hard separator the raw literal is bound as-is, so
        // non-alphanumeric content is meaningful.
        assert!(GlobPattern::parse("-/*").is_ok());
        assert!(GlobPattern::parse("my-app*").is_ok());
    }

    #[test]
    fn plain_value_is_exact() {
        let pattern = NamePattern::from_value(&Value::plain("(*Kubelet).Run")).unwrap();
        assert!(matches!(pattern, NamePattern::Exact(s) if s == "(*Kubelet).Run"));
    }

    #[test]
    fn routing_flags() {
        let g = GlobPattern::parse("*.go").unwrap();
        assert!(!g.has_hard_separator);
        assert!(g.has_dot);

        let g = GlobPattern::parse("pkg/*handler*").unwrap();
        assert!(g.has_hard_separator);

        let g = GlobPattern::parse("foo*").unwrap();
        assert!(!g.has_hard_separator);
        assert!(!g.has_dot);
    }
}
