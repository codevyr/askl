use crate::execution_state::RelationshipType;
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::verb::Args;
use anyhow::{anyhow, bail, Result};
use index::db_diesel::TraversalMode;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Limits, Verb, VerbClass};

/// HasModifier - sets the relationship type to Has (containment) for child scopes
#[derive(Debug)]
pub(in crate::verb) struct HasModifier {
    span: Span,
}

impl HasModifier {
    pub(in crate::verb) const NAME: &'static str = "has";

    pub fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &[])?;
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for HasModifier {
    fn name(&self) -> &str {
        HasModifier::NAME
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

    /// The has verb consumes itself by setting the relationship type in the parser context
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        ctx.set_relationship_type_inherited(RelationshipType::HAS);
        Ok(true) // consumed - don't add to command
    }
}

impl Display for HasModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "HasModifier")
    }
}

/// RefsModifier - explicitly sets the relationship type to Refs (reference/call-based)
/// This is the default, but can be used to override an inherited has
#[derive(Debug)]
pub(in crate::verb) struct RefsModifier {
    span: Span,
}

impl RefsModifier {
    pub(in crate::verb) const NAME: &'static str = "refs";

    pub fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &[])?;
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for RefsModifier {
    fn name(&self) -> &str {
        RefsModifier::NAME
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

    /// The refs verb consumes itself by setting the relationship type in the parser context
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        ctx.set_relationship_type_inherited(RelationshipType::REFS);
        Ok(true) // consumed - don't add to command
    }
}

impl Display for RefsModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RefsModifier")
    }
}

/// DeriveModifier - generalized relationship modifier with combination support
/// derive(type="refs")           — same as refs (inherits by default)
/// derive(type="has")            — same as has (inherits by default)
/// derive(type="refs,has")       — union: either relationship (inherits by default)
/// derive(type="has", inherit=false) — has, NOT propagated to descendants
#[derive(Debug)]
pub(in crate::verb) struct DeriveModifier {
    span: Span,
    relationship_type: RelationshipType,
    inherit: bool,
}

impl DeriveModifier {
    pub(in crate::verb) const NAME: &'static str = "derive";

    pub fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.accepts(0, &["type", "inherit"])?;

        let type_str = args
            .named_str("type")?
            .ok_or_else(|| anyhow!("derive requires a 'type' parameter"))?;

        let mut rel_type = RelationshipType::EMPTY;
        for part in type_str.split(',') {
            match part.trim() {
                "refs" => rel_type = rel_type | RelationshipType::REFS,
                "has" => rel_type = rel_type | RelationshipType::HAS,
                other => bail!(
                    "unknown relationship type '{}' in derive (expected 'refs' or 'has')",
                    other
                ),
            }
        }

        if rel_type == RelationshipType::EMPTY {
            bail!("derive type parameter must contain at least one of 'refs', 'has'");
        }

        let inherit = args.named_bool("inherit")?.unwrap_or(true);

        Ok(Arc::new(Self {
            span,
            relationship_type: rel_type,
            inherit,
        }))
    }
}

impl Verb for DeriveModifier {
    fn name(&self) -> &str {
        DeriveModifier::NAME
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

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        ctx.set_relationship_type_explicit(self.relationship_type);
        ctx.set_inherit_relationship_modifier(self.inherit);
        Ok(true) // consumed - don't add to command
    }
}

impl Display for DeriveModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeriveModifier")
    }
}

/// UnnestModifier - opts in to full transitive traversal for scope derivation.
/// Without it, `{ }` reaches descendants at the same fold depth: through the
/// structural levels (module, directory, file) but not into what a macro
/// generated.  With `unnest`, every level is included.
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
/// one level, unlike `unnest`, and through the fold, unlike the
/// default. It is the only reader of `nesting_level`, and the only way to ask
/// a question the other two modes cannot express — "what does this macro
/// expand to, immediately".
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
