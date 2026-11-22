use crate::parser_context::ParserContext;
use crate::relationship::{Relationship, Word};
use crate::span::Span;
use crate::verb::Args;
use anyhow::{bail, Result};
use index::db_diesel::TraversalMode;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Limits, Verb, VerbClass};

/// RelationshipVerb - what a scope asks of the edge to the statements in its
/// braces: one relationship word, or several joined with `or`.
///
/// The words and what each selects are [`crate::relationship`]'s.  A word is
/// written with no arguments and GOVERNS THE STATEMENT IT IS WRITTEN IN: it
/// describes the edge into that statement's braces and no other.  What
/// carries it into the scopes below is [`InheritVerb`].
///
/// One word comes here from the verb table ([`RelationshipVerb::word`]); an
/// expression of them comes from the expression builder, already joined
/// ([`RelationshipVerb::joined`], see `verb/compound.rs`).  Either way the
/// verb consumes itself into the parser context: it describes the scope, and
/// is no part of the statement's command.
#[derive(Debug)]
pub(in crate::verb) struct RelationshipVerb {
    span: Span,
    relationship: Relationship,
    /// The expression as written (`calls or holds`): the verb's name.
    written: String,
}

impl RelationshipVerb {
    pub fn word(span: Span, args: &Args, word: Word) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &[])?;
        Ok(Self::joined(span, Relationship::of(word)))
    }

    /// A relationship that words were written for.  (One that no word asked
    /// for is the bare scope, and no verb.)
    pub fn joined(span: Span, relationship: Relationship) -> Arc<dyn Verb> {
        let written = relationship.written().unwrap_or_default();
        Arc::new(Self {
            span,
            relationship,
            written,
        })
    }
}

impl Verb for RelationshipVerb {
    fn name(&self) -> &str {
        &self.written
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Relationship
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn relationship(&self) -> Option<&Relationship> {
        Some(&self.relationship)
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        ctx.write_relationship(self.relationship.clone());
        Ok(true) // consumed - don't add to command
    }
}

impl Display for RelationshipVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RelationshipVerb({})", self.written)
    }
}

/// What a statement writes that `inherit` can carry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::verb) enum Carried {
    /// The relationship: the edge into the statement's braces.
    Relationship,
    /// The type filter.
    Type,
}

impl Carried {
    const ALL: [Carried; 2] = [Carried::Relationship, Carried::Type];

    /// The kind as it is written between the parentheses.
    fn word(self) -> &'static str {
        match self {
            Carried::Relationship => "relationship",
            Carried::Type => "type",
        }
    }

    fn named(word: &str) -> Option<Carried> {
        Carried::ALL.into_iter().find(|kind| kind.word() == word)
    }
}

/// InheritVerb - `inherit(relationship)` / `inherit(type)`.
///
/// A WORD GOVERNS THE STATEMENT IT IS WRITTEN IN; `inherit(kind)` CARRIES IT.
///
/// `inherit(relationship)` carries the statement's relationship into every
/// scope below it, and `inherit(type)` its type filter, until a statement
/// writes its own.  It copies words into the braces that exist and adds no
/// hop: `"a" calls inherit(relationship) { "b" { "c" } }` is
/// `"a" calls { "b" calls { "c" } }`.  Carrying both is two verbs.
///
/// Like every term it acts on the statement it is written in, wherever it
/// stands among the others: before the word it carries or after it.  A
/// statement that wrote nothing of the kind has nothing to carry, and that
/// is no error: the scopes below are what they would be anyway.
#[derive(Debug)]
pub(in crate::verb) struct InheritVerb {
    span: Span,
    carried: Carried,
    /// `inherit(type)`: the verb as it is written, for messages.
    written: String,
}

impl InheritVerb {
    /// Build from the grammar's `inherit_verb`.  The rule admits more than
    /// the two sentences that mean something, so that each mistake is told
    /// here what to write.
    pub fn build(
        span: Span,
        pair: pest::iterators::Pair<crate::parser::Rule>,
    ) -> std::result::Result<Arc<dyn Verb>, String> {
        use crate::parser::Rule;
        let either = format!(
            "write `inherit({})` or `inherit({})`",
            Carried::Relationship.word(),
            Carried::Type.word()
        );
        let kinds: Vec<_> = pair
            .into_inner()
            .filter(|inner| inner.as_rule() == Rule::inherit_kind)
            .collect();
        let kind = match kinds.as_slice() {
            [] => return Err(format!("`inherit` needs what to carry: {either}")),
            [kind] => kind,
            _ => {
                return Err(format!(
                    "`inherit` carries one kind: write `inherit({}) inherit({})` to carry both",
                    Carried::Relationship.word(),
                    Carried::Type.word()
                ))
            }
        };
        let written = kind.as_str().trim();
        let word = kind.clone().into_inner().next();
        let is_word = word.as_ref().map(|w| w.as_rule()) == Some(Rule::ident);
        let Some(carried) = Carried::named(written).filter(|_| is_word) else {
            // The kind in quotes is the one mistake with one right answer.
            if let Some(meant) = Carried::named(written.trim_matches('"')) {
                return Err(format!(
                    "the kind is a word, not a string: write `inherit({})`",
                    meant.word()
                ));
            }
            return Err(format!(
                "`inherit` carries a statement's relationship or its type, not `{written}`: \
                 {either}"
            ));
        };
        Ok(Arc::new(Self {
            span,
            carried,
            written: format!("inherit({})", carried.word()),
        }))
    }
}

impl Verb for InheritVerb {
    fn name(&self) -> &str {
        &self.written
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Relationship
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    /// It says nothing true or false of a symbol, so it joins no expression:
    /// `func or inherit(type)` has no reading.
    fn compound_admission(&self) -> std::result::Result<(), String> {
        Err(format!(
            "`{}` carries what its statement writes and is no part of an expression: \
             write it beside the expression",
            self.written
        ))
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        match self.carried {
            Carried::Relationship => ctx.carry_relationship(),
            Carried::Type => ctx.carry_type(),
        }
        Ok(true) // consumed - don't add to command
    }
}

impl Display for InheritVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "InheritVerb({})", self.carried.word())
    }
}

/// UnnestModifier - lists everything a scope contains, folded symbols too.
/// Without it, `{ }` lists the descendants whose symbol is not folded: it
/// reaches through every level, generated declarations included, and leaves
/// out only the symbols nobody wrote down.  With `unnest`, nothing is left
/// out.
#[derive(Debug)]
pub(in crate::verb) struct UnnestModifier {
    span: Span,
}

impl UnnestModifier {
    pub(in crate::verb) const NAME: &'static str = "unnest";

    pub fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &[])?;
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for UnnestModifier {
    fn name(&self) -> &str {
        UnnestModifier::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Relationship
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn traversal_mode(&self) -> Option<TraversalMode> {
        Some(TraversalMode::Unnest)
    }
}

impl Display for UnnestModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnnestModifier")
    }
}

/// NestedModifier - narrows a scope to exactly one structural level.
///
/// `foo { printk { nested } }` shows what sits one level inside `printk` —
/// the mode sits inside the braces, on the statement whose reach it bounds:
/// one level, unlike `unnest`, and folded symbols included, unlike the
/// default. It is the only reader of `nesting_level`, and the only way to ask
/// a question the other two modes cannot express — "what sits immediately
/// inside this".
#[derive(Debug)]
pub(in crate::verb) struct NestedModifier {
    span: Span,
}

impl NestedModifier {
    pub(in crate::verb) const NAME: &'static str = "nested";

    pub fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &[])?;
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for NestedModifier {
    fn name(&self) -> &str {
        NestedModifier::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Relationship
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn traversal_mode(&self) -> Option<TraversalMode> {
        Some(TraversalMode::Nested)
    }
}

impl Display for NestedModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "NestedModifier")
    }
}

/// LimitsVerb — per-command result limits (`limits(symbols=N, listing=M)`).
///
/// Stays on the command (class `Env`) and inherits into child scopes by
/// cloning (unless `inherit=false`), so [`crate::command::Command::limits`]
/// reads the effective value at each selecting command.  Each field is
/// optional: an unset field inherits from the parent or falls back to the
/// server default.
#[derive(Debug, Clone)]
pub(in crate::verb) struct LimitsVerb {
    span: Span,
    limits: Limits,
    inherit: bool,
}

impl LimitsVerb {
    pub(in crate::verb) const NAME: &'static str = "limits";

    pub fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &["symbols", "listing", "inherit"])?;
        let symbols = args.named_usize("symbols")?;
        let listing = args.named_usize("listing")?;
        if symbols.is_none() && listing.is_none() {
            bail!("limits requires at least one of 'symbols' or 'listing'");
        }
        let inherit = args.named_bool("inherit")?.unwrap_or(true);
        Ok(Arc::new(Self {
            span,
            limits: Limits { symbols, listing },
            inherit,
        }))
    }
}

impl Verb for LimitsVerb {
    fn name(&self) -> &str {
        LimitsVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Env
    }

    fn derive_method(&self) -> DeriveMethod {
        if self.inherit {
            DeriveMethod::Clone
        } else {
            DeriveMethod::Skip
        }
    }

    fn derive_new_instance(&self) -> Option<Arc<dyn Verb>> {
        if self.inherit {
            Some(Arc::new(self.clone()))
        } else {
            None
        }
    }

    fn as_limits(&self) -> Option<Limits> {
        Some(self.limits)
    }
}

impl Display for LimitsVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "LimitsVerb(symbols={:?}, listing={:?})",
            self.limits.symbols, self.limits.listing
        )
    }
}
