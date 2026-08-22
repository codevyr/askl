use crate::execution_state::RelationshipType;
use crate::parser::Value;
use crate::parser_context::ParserContext;
use crate::span::Span;
use anyhow::{anyhow, bail, Result};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Verb, VerbClass};

/// HasModifier - sets the relationship type to Has (containment) for child scopes
#[derive(Debug)]
pub(in crate::verb) struct HasModifier {
    span: Span,
}

impl HasModifier {
    pub(in crate::verb) const NAME: &'static str = "has";

    pub fn new(
        span: Span,
        _positional: &Vec<Value>,
        _named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
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

    pub fn new(
        span: Span,
        _positional: &Vec<Value>,
        _named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
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
/// derive(type="has", inherit="false") — has, NOT propagated to descendants
#[derive(Debug)]
pub(in crate::verb) struct DeriveModifier {
    span: Span,
    relationship_type: RelationshipType,
    inherit: bool,
}

impl DeriveModifier {
    pub(in crate::verb) const NAME: &'static str = "derive";

    pub fn new(
        span: Span,
        _positional: &Vec<Value>,
        named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
        let type_str = named
            .get("type")
            .ok_or_else(|| anyhow!("derive requires a 'type' parameter"))?
            .as_plain()?;

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

        let inherit = match named.get("inherit") {
            Some(v) => v.as_plain()?.eq_ignore_ascii_case("true"),
            None => true,
        };

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
/// Without unnest, `{ }` shows only direct children and top-level references.
/// With `unnest`, all levels are included (original behavior).
#[derive(Debug)]
pub(in crate::verb) struct UnnestModifier {
    span: Span,
}

impl UnnestModifier {
    pub(in crate::verb) const NAME: &'static str = "unnest";

    pub fn new(
        span: Span,
        _positional: &Vec<Value>,
        _named: &HashMap<String, Value>,
    ) -> Result<Arc<dyn Verb>> {
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

    fn is_unnest(&self) -> bool {
        true
    }
}

impl Display for UnnestModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnnestModifier")
    }
}
