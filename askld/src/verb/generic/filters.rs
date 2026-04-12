use crate::parser_context::{
    ParserContext, SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
use crate::span::Span;
use anyhow::{anyhow, bail, Result};
use index::db_diesel::{
    CompoundNameMixin, CompositeFilter, DefaultSymbolTypeMixin, ExactNameMixin,
    PackageDescendantLeaf, ProjectFilterMixin, SymbolTypeMixin,
};
use std::collections::HashMap;
use std::fmt::Display;
use std::sync::Arc;

use super::selectors::TypeSelector;
use super::super::{DeriveMethod, Filter, Verb, VerbTag};

#[derive(Debug)]
pub(in crate::verb) struct IgnoreVerb {
    span: Span,
    name: Option<String>,
    package: Option<String>,
}

impl IgnoreVerb {
    pub(in crate::verb) const NAME: &'static str = "ignore";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let mut verb = Self {
            span,
            name: None,
            package: None,
        };
        let mut empty = true;

        if let Some(name) = positional.iter().next() {
            verb.name = Some(name.clone());
            empty = false;
        }

        if let Some(package) = named.get("package") {
            verb.package = Some(package.clone());
            empty = false;
        }

        if empty {
            bail!("Expected at least one argument");
        }

        Ok(Arc::new(verb))
    }
}

impl Verb for IgnoreVerb {
    fn name(&self) -> &str {
        IgnoreVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }
}

impl Filter for IgnoreVerb {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        let mut parts = vec![];
        if let Some(ref name) = self.name {
            // Same CompoundNameMixin the positive name filter uses — replaces
            // the old in-memory partial_name_match with an equivalent SQL lquery.
            parts.push(CompositeFilter::leaf(CompoundNameMixin::new(name)));
        }
        if let Some(ref package) = self.package {
            if let Some(leaf) = PackageDescendantLeaf::new(package) {
                parts.push(CompositeFilter::leaf(leaf));
            }
        }
        if parts.is_empty() {
            return None;
        }
        Some(CompositeFilter::not(CompositeFilter::and(parts)))
    }
}

impl Display for IgnoreVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(
            f,
            "IgnoreVerb(name={:?}, package={:?})",
            self.name, self.package
        )
    }
}

#[derive(Debug)]
pub(in crate::verb) struct ProjectFilter {
    span: Span,
    project: String,
}

impl ProjectFilter {
    pub(in crate::verb) const NAME: &'static str = "project";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(project) = positional.iter().next() {
            Ok(Arc::new(Self {
                span,
                project: project.clone(),
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for ProjectFilter {
    fn name(&self) -> &str {
        ProjectFilter::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn get_tag(&self) -> Option<VerbTag> {
        Some(VerbTag::ProjectFilter)
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        self.replace_verb(existing_verbs)
    }
}

impl Filter for ProjectFilter {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        Some(CompositeFilter::leaf(ProjectFilterMixin::new(&self.project)))
    }
}

impl Display for ProjectFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProjectFilter(project={})", self.project)
    }
}

/// DirectOnlyFilter - filter verb that adds DirectOnlyMixin to the search.
/// Added automatically when a statement has a scope and unnest is not set.
/// Restricts children/has_children queries to direct (non-transitive) results.
#[derive(Debug)]
pub struct DirectOnlyFilter {
    span: Span,
}

impl DirectOnlyFilter {
    pub fn new(span: Span) -> Arc<dyn Verb> {
        Arc::new(Self { span })
    }
}

impl Verb for DirectOnlyFilter {
    fn name(&self) -> &str {
        "direct_only_filter"
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        // Don't inherit - each statement decides based on its own unnest flag
        DeriveMethod::Skip
    }
}

impl Filter for DirectOnlyFilter {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        Some(CompositeFilter::leaf(index::db_diesel::DirectOnlyMixin))
    }
}

impl Display for DirectOnlyFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DirectOnlyFilter")
    }
}

/// DefaultTypeFilter - filters symbols by multiple types (OR).
/// Used when a child scope inherits default types from parent without explicit type selector.
#[derive(Debug)]
pub struct DefaultTypeFilter {
    span: Span,
    pub symbol_type_ids: Vec<i32>,
}

impl DefaultTypeFilter {
    pub fn new(span: Span, symbol_type_ids: Vec<i32>) -> Arc<dyn Verb> {
        Arc::new(Self {
            span,
            symbol_type_ids,
        })
    }
}

impl Verb for DefaultTypeFilter {
    fn name(&self) -> &str {
        "default_type_filter"
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        // Don't inherit to children - each level decides its own default types
        DeriveMethod::Skip
    }
}

impl Filter for DefaultTypeFilter {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        if self.symbol_type_ids.is_empty() {
            return None;
        }
        Some(CompositeFilter::leaf(DefaultSymbolTypeMixin::new(
            self.symbol_type_ids.clone(),
        )))
    }
}

impl Display for DefaultTypeFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DefaultTypeFilter({:?})", self.symbol_type_ids)
    }
}

// ============================================================================
// GenericFilter — filter
// ============================================================================

pub(in crate::verb) fn parse_symbol_types(s: &str) -> Result<Vec<i32>> {
    s.split(',')
        .map(|part| {
            let part = part.trim();
            match part {
                "func" => Ok(SYMBOL_TYPE_FUNCTION),
                "mod" => Ok(SYMBOL_TYPE_MODULE),
                "file" => Ok(SYMBOL_TYPE_FILE),
                "dir" => Ok(SYMBOL_TYPE_DIRECTORY),
                "type" => Ok(SYMBOL_TYPE_TYPE),
                "data" => Ok(SYMBOL_TYPE_DATA),
                "macro" => Ok(SYMBOL_TYPE_MACRO),
                TypeSelector::NAME_FIELD | TypeSelector::NAME_METHOD => Ok(SYMBOL_TYPE_FIELD),
                other => bail!("Unknown symbol type: '{}'", other),
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(in crate::verb) enum FilterKind {
    Type { symbol_type_ids: Vec<i32> },
    ExactName { value: String },
    CompoundName { value: String },
}

impl FilterKind {
    fn parse(kind: &str, value: &str) -> Result<Self> {
        match kind {
            "type" => Ok(FilterKind::Type {
                symbol_type_ids: parse_symbol_types(value)?,
            }),
            "exact_name" => Ok(FilterKind::ExactName {
                value: value.to_string(),
            }),
            "compound_name" => Ok(FilterKind::CompoundName {
                value: value.to_string(),
            }),
            other => bail!(
                "Unknown filter kind: '{}'. Expected 'type', 'exact_name', or 'compound_name'",
                other
            ),
        }
    }

    fn tag_name(&self) -> &'static str {
        match self {
            FilterKind::Type { .. } => "type",
            FilterKind::ExactName { .. } => "exact_name",
            FilterKind::CompoundName { .. } => "compound_name",
        }
    }

    fn has_name_constraint(&self) -> bool {
        match self {
            FilterKind::Type { .. } => false,
            FilterKind::ExactName { .. } | FilterKind::CompoundName { .. } => true,
        }
    }

    fn suppresses_default_type_filter(&self) -> bool {
        matches!(self, FilterKind::Type { .. })
    }

    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        match self {
            FilterKind::Type { symbol_type_ids } => {
                if symbol_type_ids.len() == 1 {
                    Some(CompositeFilter::leaf(SymbolTypeMixin::new(symbol_type_ids[0])))
                } else {
                    Some(CompositeFilter::leaf(DefaultSymbolTypeMixin::new(
                        symbol_type_ids.clone(),
                    )))
                }
            }
            FilterKind::ExactName { value } => {
                Some(CompositeFilter::leaf(ExactNameMixin::new(value)))
            }
            FilterKind::CompoundName { value } => {
                Some(CompositeFilter::leaf(CompoundNameMixin::new(value)))
            }
        }
    }

    fn update_context(&self, ctx: &ParserContext, inherit: bool) {
        if let FilterKind::Type { symbol_type_ids } = self {
            if inherit {
                let mut default_types = symbol_type_ids.clone();
                if !default_types.contains(&SYMBOL_TYPE_FUNCTION) {
                    default_types.push(SYMBOL_TYPE_FUNCTION);
                }
                ctx.set_default_symbol_types(default_types);
            }
        }
    }
}

impl std::fmt::Display for FilterKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            FilterKind::Type { symbol_type_ids } => write!(f, "type={:?}", symbol_type_ids),
            FilterKind::ExactName { value } => write!(f, "exact_name={}", value),
            FilterKind::CompoundName { value } => write!(f, "compound_name={}", value),
        }
    }
}

#[derive(Debug)]
pub struct GenericFilter {
    span: Span,
    kind: FilterKind,
    inherit: bool,
}

impl GenericFilter {
    pub const NAME: &'static str = "filter";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let kind_str = positional
            .first()
            .ok_or_else(|| anyhow!("filter requires a kind as first argument"))?;

        let value = positional
            .get(1)
            .ok_or_else(|| anyhow!("filter requires a value as second argument"))?;

        let kind = FilterKind::parse(kind_str, value)?;

        let inherit = named
            .get("inherit")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        Ok(Arc::new(Self {
            span,
            kind,
            inherit,
        }))
    }
}

impl Verb for GenericFilter {
    fn name(&self) -> &str {
        GenericFilter::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
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
            Some(Arc::new(GenericFilter {
                span: self.span.clone(),
                kind: self.kind.clone(),
                inherit: self.inherit,
            }))
        } else {
            None
        }
    }

    fn get_tag(&self) -> Option<VerbTag> {
        Some(VerbTag::GenericFilter(self.kind.tag_name()))
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        self.replace_verb(existing_verbs)
    }

    fn suppresses_default_type_filter(&self) -> bool {
        self.kind.suppresses_default_type_filter()
    }

    fn has_name_constraint(&self) -> bool {
        self.kind.has_name_constraint()
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        self.kind.update_context(ctx, self.inherit);
        Ok(false)
    }
}

impl Filter for GenericFilter {
    fn get_composite_filter(&self) -> Option<CompositeFilter> {
        self.kind.get_composite_filter()
    }
}

impl Display for GenericFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GenericFilter({})", self.kind)
    }
}
