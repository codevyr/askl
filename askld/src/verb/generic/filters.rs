use crate::name_pattern::NamePattern;
use crate::parser::Value;
use crate::parser_context::{
    ParserContext, SYMBOL_TYPE_DATA, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FIELD, SYMBOL_TYPE_FILE,
    SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MACRO, SYMBOL_TYPE_MODULE, SYMBOL_TYPE_TYPE,
};
use crate::span::Span;
use crate::verb::Args;
use anyhow::{anyhow, bail, Result};
use index::db_diesel::{
    CompositeFilter, CompoundNameMixin, DefaultSymbolTypeMixin, ExactNameMixin,
    PackageDescendantLeaf, ProjectFilterMixin, SymbolTypeMixin,
};
use std::fmt::Display;
use std::sync::Arc;

use super::super::{AnchorKind, DeriveMethod, Dimension, Filter, Verb, VerbClass};
use super::selectors::TypeSelector;

#[derive(Debug)]
pub(in crate::verb) struct IgnoreVerb {
    span: Span,
    name: Option<NamePattern>,
    /// The VALIDATED package leaf, not the raw string: whether this verb can
    /// produce a predicate at all must be answerable without an EphContext,
    /// because `compound_admission` promises exactly that (a package that
    /// normalises to no ltree labels yields none).
    package: Option<PackageDescendantLeaf>,
}

impl IgnoreVerb {
    pub(in crate::verb) const NAME: &'static str = "ignore";

    pub(in crate::verb) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        let mut verb = Self {
            span,
            name: None,
            package: None,
        };
        let mut empty = true;

        if let Some(name) = args.value_at(0) {
            verb.name = Some(NamePattern::from_value(name)?);
            empty = false;
        }

        if let Some(package) = args.named_str("package")? {
            // A package that normalises to nothing stays a no-op filter, as
            // before — `compound_admission` is what keeps it out of
            // expressions, where a no-op would break the compiler's totality.
            verb.package = PackageDescendantLeaf::new(package);
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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn compound_admission(&self) -> Result<(), String> {
        if self.name.is_some() || self.package.is_some() {
            Ok(())
        } else {
            Err("this `ignore` excludes nothing (its package path names no \
                 package), so it constrains nothing and cannot appear in a \
                 filter expression"
                .to_string())
        }
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        Some(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }
}

impl Filter for IgnoreVerb {
    fn get_composite_filter(&self, _eph: &index::db_diesel::EphContext) -> Option<CompositeFilter> {
        let mut parts = vec![];
        match &self.name {
            // Same CompoundNameMixin the positive name filter uses — replaces
            // the old in-memory partial_name_match with an equivalent SQL lquery.
            Some(NamePattern::Exact(name)) => {
                parts.push(CompositeFilter::leaf(CompoundNameMixin::new(name)));
            }
            // Same glob semantics the positive name filter uses.
            Some(NamePattern::Glob(glob)) => parts.push(glob.filter(true, true)),
            None => {}
        }
        if let Some(ref leaf) = self.package {
            parts.push(CompositeFilter::leaf(leaf.clone()));
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

    pub(in crate::verb) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        if args.no_positional() {
            bail!("Expected a positional argument");
        }
        Ok(Arc::new(Self {
            span,
            project: args.str_at(0, "project name")?.to_string(),
        }))
    }
}

impl Verb for ProjectFilter {
    fn name(&self) -> &str {
        ProjectFilter::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn compound_admission(&self) -> Result<(), String> {
        Ok(())
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        Some(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::Project)
    }
}

impl Filter for ProjectFilter {
    fn get_composite_filter(&self, _eph: &index::db_diesel::EphContext) -> Option<CompositeFilter> {
        Some(CompositeFilter::leaf(ProjectFilterMixin::new(
            &self.project,
        )))
    }
}

impl Display for ProjectFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProjectFilter(project={})", self.project)
    }
}

/// PackageFilter — keep only symbols strictly under a package path: the
/// positive form of `ignore(package=...)`.  With `not`, the old
/// `ignore("f", package="p")` is expressible as `not (g"f" and
/// package("p"))`; `ignore` remains for compatibility.
#[derive(Debug)]
pub(in crate::verb) struct PackageFilter {
    span: Span,
    /// Kept for diagnostics only — the predicate comes from `leaf`.
    package: String,
    /// The VALIDATED leaf: constructing it here (and failing the parse if it
    /// cannot be built) makes "this verb always compiles to a predicate" a
    /// structural property rather than a comment, which is exactly what the
    /// compound admission gate promises.
    leaf: PackageDescendantLeaf,
}

impl PackageFilter {
    pub(in crate::verb) const NAME: &'static str = "package";

    pub(in crate::verb) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        args.allow(&[])?;
        if args.count() != 1 {
            bail!("package requires exactly one positional argument: the package path");
        }
        let package = args.str_at(0, "package path")?.to_string();
        let Some(leaf) = PackageDescendantLeaf::new(&package) else {
            bail!("'{}' does not name a package path", package);
        };
        Ok(Arc::new(Self {
            span,
            package,
            leaf,
        }))
    }
}

impl Verb for PackageFilter {
    fn name(&self) -> &str {
        PackageFilter::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn compound_admission(&self) -> Result<(), String> {
        Ok(())
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        Some(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::Package)
    }
}

impl Filter for PackageFilter {
    fn get_composite_filter(&self, _eph: &index::db_diesel::EphContext) -> Option<CompositeFilter> {
        Some(CompositeFilter::leaf(self.leaf.clone()))
    }
}

impl Display for PackageFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "PackageFilter(package={})", self.package)
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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        Some(self)
    }

    // Semantically a type-dimension constraint.  By construction it can
    // never conflict: it is injected last and only when no type verb exists.
    fn dimension(&self) -> Option<Dimension> {
        Some(Dimension::Type)
    }

    fn derive_method(&self) -> DeriveMethod {
        // Don't inherit to children - each level decides its own default types
        DeriveMethod::Skip
    }
}

impl Filter for DefaultTypeFilter {
    fn get_composite_filter(&self, _eph: &index::db_diesel::EphContext) -> Option<CompositeFilter> {
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
    ExactName { pattern: NamePattern },
    CompoundName { pattern: NamePattern },
}

impl FilterKind {
    fn parse(kind: &str, value: &Value) -> Result<Self> {
        match kind {
            "type" => Ok(FilterKind::Type {
                symbol_type_ids: parse_symbol_types(value.as_plain()?)?,
            }),
            "exact_name" => Ok(FilterKind::ExactName {
                pattern: NamePattern::from_value(value)?,
            }),
            "compound_name" => Ok(FilterKind::CompoundName {
                pattern: NamePattern::from_value(value)?,
            }),
            other => bail!(
                "Unknown filter kind: '{}'. Expected 'type', 'exact_name', or 'compound_name'",
                other
            ),
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

    fn get_composite_filter(&self, _eph: &index::db_diesel::EphContext) -> Option<CompositeFilter> {
        match self {
            FilterKind::Type { symbol_type_ids } => {
                if symbol_type_ids.len() == 1 {
                    Some(CompositeFilter::leaf(SymbolTypeMixin::new(
                        symbol_type_ids[0],
                    )))
                } else {
                    Some(CompositeFilter::leaf(DefaultSymbolTypeMixin::new(
                        symbol_type_ids.clone(),
                    )))
                }
            }
            FilterKind::ExactName { pattern } => match pattern {
                NamePattern::Exact(value) => {
                    Some(CompositeFilter::leaf(ExactNameMixin::new(value)))
                }
                // Anchored full-name glob: the whole name must match.
                NamePattern::Glob(glob) => Some(glob.full_name_filter()),
            },
            FilterKind::CompoundName { pattern } => match pattern {
                NamePattern::Exact(value) => {
                    Some(CompositeFilter::leaf(CompoundNameMixin::new(value)))
                }
                NamePattern::Glob(glob) => Some(glob.filter(true, true)),
            },
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
            FilterKind::ExactName { pattern } => write!(f, "exact_name={}", pattern),
            FilterKind::CompoundName { pattern } => write!(f, "compound_name={}", pattern),
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

    pub(in crate::verb) fn new(span: Span, args: &Args) -> Result<Arc<dyn Verb>> {
        if args.no_positional() {
            bail!("filter requires a kind as first argument");
        }
        let kind_str = args.str_at(0, "filter kind")?;

        let value = args
            .value_at(1)
            .ok_or_else(|| anyhow!("filter requires a value as second argument"))?;

        let kind = FilterKind::parse(kind_str, value)?;

        let inherit = args.named_bool("inherit")?.unwrap_or(false);

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

    fn class(&self) -> VerbClass {
        VerbClass::Predicate
    }

    fn compound_admission(&self) -> Result<(), String> {
        Ok(())
    }

    fn as_filter<'a>(&'a self) -> Option<&'a dyn Filter> {
        Some(self)
    }

    /// `filter("exact_name"/"compound_name", ...)` anchors by name — it
    /// constrains the whole statement to a name, unlike a type filter.
    fn anchor_kind(&self) -> Option<AnchorKind> {
        self.kind.has_name_constraint().then_some(AnchorKind::Name)
    }

    fn has_name_constraint(&self) -> bool {
        self.kind.has_name_constraint()
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

    fn dimension(&self) -> Option<Dimension> {
        Some(match self.kind {
            FilterKind::Type { .. } => Dimension::Type,
            FilterKind::ExactName { .. } => Dimension::FilterExactName,
            FilterKind::CompoundName { .. } => Dimension::FilterCompoundName,
        })
    }

    fn suppresses_default_type_filter(&self) -> bool {
        self.kind.suppresses_default_type_filter()
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        self.kind.update_context(ctx, self.inherit);
        Ok(false)
    }
}

impl Filter for GenericFilter {
    fn get_composite_filter(&self, eph: &index::db_diesel::EphContext) -> Option<CompositeFilter> {
        self.kind.get_composite_filter(eph)
    }
}

impl Display for GenericFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GenericFilter({})", self.kind)
    }
}
