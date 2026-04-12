use crate::cfg::ControlFlowGraph;
use crate::execution_state::RelationshipType;
use crate::parser_context::ParserContext;
use crate::span::Span;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use index::db_diesel::{CompositeFilter, ScopeContext, Selection};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::super::{DeriveMethod, Selector, Verb, VerbTag};

#[derive(Debug)]
pub(in crate::verb) struct IsolatedScope {
    span: Span,
    _isolated: bool,
}

impl IsolatedScope {
    pub(in crate::verb) const NAME: &'static str = "scope";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if !positional.is_empty() {
            bail!("Unexpected positional arguments");
        }

        let isolated = if let Some(isolated_str) = named.get("isolated") {
            if isolated_str == "true" {
                true
            } else if isolated_str == "false" {
                false
            } else {
                bail!("Unexpected value for isolated parameter: {}", isolated_str);
            }
        } else {
            false
        };

        Ok(Arc::new(Self {
            span,
            _isolated: isolated,
        }))
    }
}

impl Verb for IsolatedScope {
    fn name(&self) -> &str {
        IsolatedScope::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Selector for IsolatedScope {
    async fn select_from_all_impl(
        &self,
        _cfg: &ControlFlowGraph,
        _filter: CompositeFilter,
        _parent_scope: ScopeContext,
        _children_scope: ScopeContext,
    ) -> Result<Option<Selection>> {
        Ok(Some(Selection::new()))
    }
}

impl Display for IsolatedScope {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "IsolatedScope")
    }
}

/// HasModifier - sets the relationship type to Has (containment) for child scopes
#[derive(Debug)]
pub(in crate::verb) struct HasModifier {
    span: Span,
}

impl HasModifier {
    pub(in crate::verb) const NAME: &'static str = "has";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
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
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
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
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let type_str = named
            .get("type")
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

        let inherit = named
            .get("inherit")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(true);

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
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
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

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn get_tag(&self) -> Option<VerbTag> {
        Some(VerbTag::Unnest)
    }
}

impl Display for UnnestModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnnestModifier")
    }
}

/// AnyModifier - removes inherited type filtering from parent scopes.
/// When added to a statement, it strips any verbs that suppress the default
/// type filter (i.e. inherited TypeSelectors), allowing all symbol types to match.
#[derive(Debug)]
pub(in crate::verb) struct AnyModifier {
    span: Span,
}

impl AnyModifier {
    pub(in crate::verb) const NAME: &'static str = "any";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        Ok(Arc::new(Self { span }))
    }
}

impl Verb for AnyModifier {
    fn name(&self) -> &str {
        AnyModifier::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    /// Removes inherited TypeSelectors (verbs where suppresses_default_type_filter is true).
    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        existing_verbs
            .into_iter()
            .filter(|v| !v.suppresses_default_type_filter())
            .collect()
    }

    fn update_context(&self, _ctx: &ParserContext) -> Result<bool> {
        // Not consumed — must reach add_verb to strip inherited type selectors.
        Ok(false)
    }
}

impl Display for AnyModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "AnyModifier")
    }
}
