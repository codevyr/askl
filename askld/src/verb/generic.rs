use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::execution_state::{DependencyRole, RelationshipType};
use crate::parser::{Identifier, NamedArgument, PositionalArgument, Rule};
use crate::parser_context::{
    ParserContext, SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FILE, SYMBOL_TYPE_FUNCTION,
    SYMBOL_TYPE_MODULE,
};
use crate::span::Span;
use crate::statement::Statement;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompoundNameMixin, ExactNameMixin, IgnoreFilterMixin, Index, ParentReference,
    ProjectFilterMixin, Selection, SymbolSearchMixin,
};
use index::models_diesel::SymbolRef;
use index::symbols::{self, package_match};
use index::symbols::{clean_and_split_string, partial_name_match, SymbolId};
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;
use std::fmt::Display;
use std::rc::Rc;
use std::sync::{Arc, OnceLock};
use std::vec;

use super::labels::{LabelVerb, UserVerb};
use super::preamble::PreambleVerb;
use super::{DeriveMethod, Filter, Selector, Verb, VerbTag};

pub(crate) fn build_generic_verb(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>, Error<Rule>> {
    let verb_span = Span::from_pest(pair.as_span(), ctx.source());
    let mut pair = pair.into_inner();
    let ident = pair.next().unwrap();
    let mut positional = vec![];
    let mut named = HashMap::new();
    pair.map(|pair| match pair.as_rule() {
        Rule::positional_argument => {
            let arg = PositionalArgument::build(pair)?;
            positional.push(arg.value.0);
            Ok(())
        }
        Rule::named_argument => {
            let arg = NamedArgument::build(pair)?;
            named.insert(arg.name.0, arg.value.0);
            Ok(())
        }
        rule => Err(Error::new_from_span(
            pest::error::ErrorVariant::ParsingError {
                positives: vec![Rule::positional_argument, Rule::named_argument],
                negatives: vec![rule],
            },
            pair.as_span(),
        )),
    })
    .collect::<Result<Vec<_>, _>>()?;

    let ident = if let Rule::generic_ident = ident.as_rule() {
        ident.into_inner().next().unwrap()
    } else {
        let span = ident.as_span();
        return Err(Error::new_from_span(
            CustomError {
                message: format!("Expected verb name as @name"),
            },
            span,
        ));
    };

    let span = ident.as_span();
    let res = match Identifier::build(ident)?.0.as_str() {
        GenericSelector::NAME => GenericSelector::new(verb_span, &positional, &named),
        GenericFilter::NAME => GenericFilter::new(verb_span, &positional, &named),
        IgnoreVerb::NAME => IgnoreVerb::new(verb_span, &positional, &named),
        ProjectFilter::NAME => ProjectFilter::new(verb_span, &positional, &named),
        ForcedVerb::NAME => ForcedVerb::new(verb_span, &positional, &named),
        IsolatedScope::NAME => IsolatedScope::new(verb_span, &positional, &named),
        LabelVerb::NAME => LabelVerb::new(verb_span, &positional, &named),
        UserVerb::NAME => UserVerb::new(verb_span, &positional, &named),
        PreambleVerb::NAME => PreambleVerb::new(verb_span, &positional, &named),
        HasModifier::NAME => HasModifier::new(verb_span, &positional, &named),
        RefsModifier::NAME => RefsModifier::new(verb_span, &positional, &named),
        DeriveModifier::NAME => DeriveModifier::new(verb_span, &positional, &named),
        TypeSelector::NAME_FUNCTION => TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_FUNCTION),
        TypeSelector::NAME_FILE => TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_FILE),
        TypeSelector::NAME_MODULE => TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_MODULE),
        TypeSelector::NAME_DIRECTORY => TypeSelector::new(verb_span, &positional, &named, SYMBOL_TYPE_DIRECTORY),
        unknown => Err(anyhow!("unknown verb : {}", unknown)),
    };

    match res {
        Ok(res) => Ok(res),
        Err(err) => Err(Error::new_from_span(
            CustomError {
                message: format!("Failed to create a generic verb: {}", err),
            },
            span,
        )),
    }
}

#[derive(Debug)]
pub struct NameSelector {
    span: Span,
    pub name: String,
}

impl NameSelector {
    pub(super) const NAME: &'static str = "_name_select";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self {
                span,
                name: name.clone(),
            }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for NameSelector {
    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn name(&self) -> &str {
        NameSelector::NAME
    }
}

#[async_trait(?Send)]
impl Selector for NameSelector {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        let mut search_mixins = search_mixins;
        search_mixins.push(Box::new(CompoundNameMixin::new(&self.name)));
        // Type filtering is handled by DefaultTypeFilter added in statement.rs.
        // NameSelector does not add its own type filter to avoid conflicting
        // with inherited default types from parent scopes.
        let selection = cfg.index.find_symbol(&mut search_mixins).await?;
        Ok(Some(selection))
    }
}

#[derive(Debug)]
pub(super) struct ForcedVerb {
    span: Span,
    name: String,
    selection: Arc<OnceLock<Selection>>,
}

impl ForcedVerb {
    pub(super) const NAME: &'static str = "forced";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self {
                span,
                name: name.clone(),
                selection: Arc::new(OnceLock::new()),
            }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for ForcedVerb {
    fn name(&self) -> &str {
        ForcedVerb::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Selector for ForcedVerb {
    fn dependency_ready(&self, dependency_role: DependencyRole) -> bool {
        if dependency_role == DependencyRole::Parent {
            false
        } else {
            true
        }
    }

    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        let mut search_mixins = search_mixins;
        search_mixins.push(Box::new(CompoundNameMixin::new(&self.name)));
        // Type filtering is handled by DefaultTypeFilter added in statement.rs.
        let selection = cfg.index.find_symbol(&mut search_mixins).await?;

        // Cache the forced selection so derivations can fabricate the
        // correct parent ↔ child relationship later on.
        let _ = self.selection.set(selection);

        // Forced matches shouldn't directly contribute nodes; they are only
        // materialised when another statement (e.g. a parent) references
        // them. Returning an empty selection keeps the execution state unset
        // so derivations can populate it when needed.
        Ok(None)
    }

    async fn derive_from_parent(
        &self,
        ctx: &mut ExecutionContext,
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        parent: &Statement,
        _rel_type: RelationshipType,
    ) -> Result<Option<Selection>> {
        let parent_selection = match parent.get_selection(ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };

        let cached_selection = self.selection.get().cloned();

        let mut normal_selection = match cached_selection {
            Some(selection) => selection,
            None => {
                return Ok(Some(Selection::new()));
            }
        };

        let mut fake_parent_references = Vec::<ParentReference>::new();
        for parent_node in parent_selection.nodes.iter() {
            for child_node in normal_selection.nodes.iter() {
                let reference = ParentReference {
                    to_symbol: child_node.symbol.clone(),
                    to_instance: child_node.symbol_instance.clone(),
                    from_instance: parent_node.symbol_instance.clone(),
                    symbol_ref: SymbolRef {
                        id: 0,
                        to_symbol: child_node.symbol.id,
                        from_object: parent_node.object.id,
                        from_offset_range: parent_node.symbol_instance.offset_range.clone(),
                    },
                };
                fake_parent_references.push(reference);
            }
        }

        normal_selection.parents = fake_parent_references;

        Ok(Some(normal_selection))
    }
}

impl Filter for ForcedVerb {
    fn filter_impl(&self, _selection: &mut Selection) {}
}

impl Display for ForcedVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ForcedVerb(name={})", self.name)
    }
}

#[derive(Debug)]
pub struct UnitVerb {
    span: Span,
}

impl UnitVerb {
    pub fn new(span: Span) -> Arc<dyn Verb> {
        Arc::new(Self { span })
    }
}

impl Verb for UnitVerb {
    fn name(&self) -> &str {
        "unit"
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn is_unit(&self) -> bool {
        true
    }
}

#[async_trait(?Send)]
impl Selector for UnitVerb {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        Ok(None)
    }
}

impl Display for UnitVerb {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "UnitVerb")
    }
}

#[derive(Debug)]
pub(super) struct IgnoreVerb {
    span: Span,
    name: Option<String>,
    package: Option<String>,
}

impl IgnoreVerb {
    pub(super) const NAME: &'static str = "ignore";

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
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        vec![Box::new(IgnoreFilterMixin::new(
            self.name.as_deref(),
            self.package.as_deref(),
        ))]
    }

    fn filter_impl(&self, selection: &mut Selection) {
        selection.nodes.retain(|s| {
            let index_symbol: symbols::Symbol = symbols::Symbol {
                id: SymbolId(s.symbol.id.clone()),
                name: s.symbol.name.clone(),
                name_split: clean_and_split_string(&s.symbol.name),
                ..Default::default()
            };

            let id = &index_symbol.id;
            if let Some(ref name) = self.name {
                let matcher = partial_name_match(name);
                let matched_symbol = matcher((id, &index_symbol));
                if matched_symbol.is_none() {
                    return true;
                }
            }

            if let Some(ref package) = self.package {
                let matcher = package_match(package);
                let matched_symbol = matcher((id, &index_symbol));
                if matched_symbol.is_none() {
                    return true;
                }
            }
            false
        });
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
pub(super) struct ProjectFilter {
    span: Span,
    project: String,
}

impl ProjectFilter {
    pub(super) const NAME: &'static str = "project";

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
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        vec![Box::new(ProjectFilterMixin::new(&self.project))]
    }
}

impl Display for ProjectFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ProjectFilter(project={})", self.project)
    }
}

#[derive(Debug)]
pub(super) struct IsolatedScope {
    span: Span,
    _isolated: bool,
}

impl IsolatedScope {
    pub(super) const NAME: &'static str = "scope";

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
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
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
pub(super) struct HasModifier {
    span: Span,
}

impl HasModifier {
    pub(super) const NAME: &'static str = "has";

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

    /// The @has verb consumes itself by setting the relationship type in the parser context
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
/// This is the default, but can be used to override an inherited @has
#[derive(Debug)]
pub(super) struct RefsModifier {
    span: Span,
}

impl RefsModifier {
    pub(super) const NAME: &'static str = "refs";

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

    /// The @refs verb consumes itself by setting the relationship type in the parser context
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
/// @derive(type="refs")           — same as @refs (inherits by default)
/// @derive(type="has")            — same as @has (inherits by default)
/// @derive(type="refs,has")       — union: either relationship (inherits by default)
/// @derive(type="has", inherit="false") — has, NOT propagated to descendants
#[derive(Debug)]
pub(super) struct DeriveModifier {
    span: Span,
    relationship_type: RelationshipType,
    inherit: bool,
}

impl DeriveModifier {
    pub(super) const NAME: &'static str = "derive";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let type_str = named
            .get("type")
            .ok_or_else(|| anyhow!("@derive requires a 'type' parameter"))?;

        let mut rel_type = RelationshipType::EMPTY;
        for part in type_str.split(',') {
            match part.trim() {
                "refs" => rel_type = rel_type | RelationshipType::REFS,
                "has" => rel_type = rel_type | RelationshipType::HAS,
                other => bail!("unknown relationship type '{}' in @derive (expected 'refs' or 'has')", other),
            }
        }

        if rel_type == RelationshipType::EMPTY {
            bail!("@derive type parameter must contain at least one of 'refs', 'has'");
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

/// TypeSelector - selects symbols by type (@function, @file, @module, @directory)
///
/// # Behavior Modes
///
/// - **Filter mode** (`filter_only=true`): Returns `None` from `select_from_all`,
///   forcing derivation from parent. Much more efficient when used inside `@has { }`.
///   At root level (no parent), returns empty results.
///
/// - **Selector mode** (`filter_only=false`): Queries all symbols of this type
///   from the database. Works at any level including root.
///
/// # Default Behavior
///
/// The default is optimized for performance - querying all symbols of a type
/// can be expensive, so filter mode is preferred when possible:
///
/// - `@function` (no args) → filter mode
/// - `@function("foo")` (with name) → selector mode
/// - `@function(filter="true")` → explicitly filter mode
/// - `@function(filter="false")` → explicitly selector mode (select all)
/// - `@function("foo", filter="true")` → filter mode even with name
///
/// # Examples
///
/// ```text
/// @file("main.go") @has { @function }     // filter: derives functions from file
/// @function("main")                        // selector: queries for "main" function
/// @function(filter="false")                // selector: queries ALL functions
/// @function                                // filter: empty at root, derives in @has
/// ```
#[derive(Debug)]
pub(super) struct TypeSelector {
    span: Span,
    symbol_type_id: i32,
    name_pattern: Option<String>,
    /// If true, don't select from all - only act as a filter when deriving from parent.
    /// This is much more efficient for queries like `@file @has { @function }`.
    filter_only: bool,
    /// If true, this filter is inherited (cloned) into derived child scopes.
    /// Used for namespace filters like `@module("test", filter="true", inherit="true")`.
    inherit: bool,
    /// If true, the last query token is anchored to the last path component.
    /// Default for @dir and @file; can be overridden with `match="contains"`.
    leaf_anchored: bool,
}

impl TypeSelector {
    pub(super) const NAME_FUNCTION: &'static str = "func";
    pub(super) const NAME_FILE: &'static str = "file";
    pub(super) const NAME_MODULE: &'static str = "mod";
    pub(super) const NAME_DIRECTORY: &'static str = "dir";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        named: &HashMap<String, String>,
        symbol_type_id: i32,
    ) -> Result<Arc<dyn Verb>> {
        let name_pattern = positional.first().cloned();

        // Check for explicit filter argument (true or false)
        let explicit_filter = named.get("filter").map(|v| v.eq_ignore_ascii_case("true"));

        // Default: filter mode if no name pattern, selector mode if name provided
        // Can be overridden with explicit filter="true" or filter="false"
        let filter_only = match explicit_filter {
            Some(true) => true,   // filter="true" forces filter mode
            Some(false) => false, // filter="false" forces selector mode
            None => name_pattern.is_none(), // default based on name presence
        };

        let inherit = named
            .get("inherit")
            .map(|v| v.eq_ignore_ascii_case("true"))
            .unwrap_or(false);

        let leaf_anchored = match named.get("match").map(|v| v.as_str()) {
            Some("contains") => false,
            _ => matches!(symbol_type_id, SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE),
        };

        Ok(Arc::new(Self {
            span,
            symbol_type_id,
            name_pattern,
            filter_only,
            inherit,
            leaf_anchored,
        }))
    }

    /// Returns the appropriate name mixin for the given name and symbol type.
    /// Directory/file types with path args (starting with '/') use exact match;
    /// all other cases use compound name (ltree) matching.
    fn name_mixin(name: &str, symbol_type_id: i32, leaf_anchored: bool) -> Box<dyn SymbolSearchMixin> {
        match symbol_type_id {
            SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE if name.starts_with('/') => {
                Box::new(ExactNameMixin::new(name))
            }
            _ if leaf_anchored => Box::new(CompoundNameMixin::new_leaf_anchored(name)),
            _ => Box::new(CompoundNameMixin::new(name)),
        }
    }

    /// Build search mixins for this type selector.
    /// Used by both `get_filter_mixins` and `select_from_all_impl` to avoid duplication.
    fn build_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        let mut mixins: Vec<Box<dyn SymbolSearchMixin>> = vec![
            Box::new(SymbolTypeMixin::new(self.symbol_type_id)),
        ];
        if let Some(ref name) = self.name_pattern {
            mixins.push(Self::name_mixin(name, self.symbol_type_id, self.leaf_anchored));
        }
        mixins
    }
}

impl Verb for TypeSelector {
    fn name(&self) -> &str {
        match self.symbol_type_id {
            SYMBOL_TYPE_FUNCTION => TypeSelector::NAME_FUNCTION,
            SYMBOL_TYPE_FILE => TypeSelector::NAME_FILE,
            SYMBOL_TYPE_MODULE => TypeSelector::NAME_MODULE,
            SYMBOL_TYPE_DIRECTORY => TypeSelector::NAME_DIRECTORY,
            _ => "type_selector",
        }
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
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
        // Create a fresh TypeSelector instance so derived child scopes
        // get their own registry entry, avoiding shared state issues.
        Some(Arc::new(TypeSelector {
            span: self.span.clone(),
            symbol_type_id: self.symbol_type_id,
            name_pattern: self.name_pattern.clone(),
            filter_only: self.filter_only,
            inherit: self.inherit,
            leaf_anchored: self.leaf_anchored,
        }))
    }

    fn get_tag(&self) -> Option<VerbTag> {
        if self.filter_only && self.name_pattern.is_some() {
            Some(VerbTag::TypeFilter)
        } else {
            None
        }
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        if self.filter_only && self.name_pattern.is_some() {
            self.replace_verb(existing_verbs)
        } else {
            self.extend_verb(existing_verbs)
        }
    }

    /// Set default symbol types and relationship type for child scopes.
    /// Container types (@dir, @file, @mod) implicitly set refs+has with inherit.
    /// @func explicitly sets REFS to override any inherited refs+has.
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        let default_types = match self.symbol_type_id {
            SYMBOL_TYPE_FUNCTION => vec![SYMBOL_TYPE_FUNCTION],
            SYMBOL_TYPE_MODULE => vec![SYMBOL_TYPE_MODULE, SYMBOL_TYPE_FUNCTION],
            SYMBOL_TYPE_FILE => vec![SYMBOL_TYPE_FUNCTION, SYMBOL_TYPE_MODULE],
            SYMBOL_TYPE_DIRECTORY => vec![SYMBOL_TYPE_DIRECTORY, SYMBOL_TYPE_FILE],
            _ => vec![SYMBOL_TYPE_FUNCTION],
        };
        ctx.set_default_symbol_types(default_types);

        match self.symbol_type_id {
            // Container types: set refs+has with inherit
            SYMBOL_TYPE_DIRECTORY | SYMBOL_TYPE_FILE | SYMBOL_TYPE_MODULE => {
                ctx.set_relationship_type_inherited(RelationshipType::REFS | RelationshipType::HAS);
            }
            // @func: explicitly set REFS to override any inherited refs+has
            SYMBOL_TYPE_FUNCTION => {
                ctx.set_relationship_type_explicit(RelationshipType::REFS);
            }
            _ => {}
        }

        // Don't consume - still add this verb to the command
        Ok(false)
    }

    fn suppresses_default_type_filter(&self) -> bool {
        true
    }
}

impl Filter for TypeSelector {
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        if self.filter_only && self.name_pattern.is_some() {
            // When used as a namespace filter (e.g., @module("test", filter="true")),
            // only constrain by name pattern, not by type. This allows
            // @module("test", filter="true") "a" to find functions named "test.a"
            // rather than restricting to MODULE-type symbols.
            let name = self.name_pattern.as_ref().unwrap();
            vec![Self::name_mixin(name, self.symbol_type_id, self.leaf_anchored)]
        } else {
            self.build_mixins()
        }
    }
}

#[async_trait(?Send)]
impl Selector for TypeSelector {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        // In filter mode, don't query all symbols - wait for derivation from parent.
        // This is much more efficient for queries like `@file @has { @function }`.
        if self.filter_only {
            return Ok(None);
        }

        let mut search_mixins = search_mixins;
        search_mixins.extend(self.build_mixins());
        let selection = cfg.index.find_symbol(&mut search_mixins).await?;
        Ok(Some(selection))
    }
}

impl Display for TypeSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self.symbol_type_id {
            SYMBOL_TYPE_FUNCTION => write!(f, "TypeSelector(function)"),
            SYMBOL_TYPE_FILE => write!(f, "TypeSelector(file)"),
            SYMBOL_TYPE_MODULE => write!(f, "TypeSelector(module)"),
            SYMBOL_TYPE_DIRECTORY => write!(f, "TypeSelector(directory)"),
            _ => write!(f, "TypeSelector({})", self.symbol_type_id),
        }
    }
}

/// SymbolTypeMixin - filters symbols by type ID
#[derive(Debug, Clone)]
pub struct SymbolTypeMixin {
    pub symbol_type_id: i32,
}

impl SymbolTypeMixin {
    pub fn new(symbol_type_id: i32) -> Self {
        Self { symbol_type_id }
    }
}

impl SymbolSearchMixin for SymbolTypeMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::CurrentQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::CurrentQuery<'a>> {
        use diesel::prelude::*;
        use index::schema_diesel::symbols;

        Ok(query.filter(symbols::dsl::symbol_type.eq(self.symbol_type_id)))
    }

    fn filter_has_parents<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::mixins::HasParentsQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::mixins::HasParentsQuery<'a>> {
        use diesel::prelude::*;
        use index::schema_diesel::symbols;

        // Filter on the child/current symbol's type
        Ok(query.filter(symbols::dsl::symbol_type.eq(self.symbol_type_id)))
    }

    fn filter_has_children<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::mixins::HasChildrenQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::mixins::HasChildrenQuery<'a>> {
        use diesel::prelude::*;
        use index::schema_diesel::symbols;

        // Filter on the parent/current symbol's type
        Ok(query.filter(symbols::dsl::symbol_type.eq(self.symbol_type_id)))
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
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        vec![Box::new(DefaultSymbolTypeMixin::new(
            self.symbol_type_ids.clone(),
        ))]
    }
}

impl Display for DefaultTypeFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DefaultTypeFilter({:?})", self.symbol_type_ids)
    }
}

// ============================================================================
// GenericSelector — @select
// ============================================================================

#[derive(Debug)]
pub struct GenericSelector {
    span: Span,
    captured_filters: OnceLock<Vec<Arc<dyn Verb>>>,
}

impl GenericSelector {
    pub const NAME: &'static str = "select";

    pub fn new(
        span: Span,
        _positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        Ok(Arc::new(Self {
            span,
            captured_filters: OnceLock::new(),
        }))
    }
}

impl Verb for GenericSelector {
    fn name(&self) -> &str {
        GenericSelector::NAME
    }

    fn span(&self) -> pest::Span<'_> {
        self.span.as_pest_span()
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn requires_name_constraint(&self) -> bool {
        true
    }

    fn has_name_constraint(&self) -> bool {
        self.captured_filters
            .get()
            .map(|filters| filters.iter().any(|v| v.has_name_constraint()))
            .unwrap_or(false)
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        // Capture filter verbs from the command at this point (before we're added).
        // This gives each @select its own positional filter set.
        let filters = ctx.get_filter_verbs();
        let _ = self.captured_filters.set(filters);
        Ok(false)
    }
}

#[async_trait(?Send)]
impl Selector for GenericSelector {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        _search_mixins: Vec<Box<dyn SymbolSearchMixin>>,
    ) -> Result<Option<Selection>> {
        // Ignore incoming search_mixins (command-wide filters including DefaultTypeFilter).
        // Instead, use only the captured filter verbs' mixins.
        let mut search_mixins: Vec<Box<dyn SymbolSearchMixin>> = Vec::new();
        if let Some(captured) = self.captured_filters.get() {
            for verb in captured {
                if let Ok(filter) = verb.as_filter() {
                    search_mixins.extend(filter.get_filter_mixins());
                }
            }
        }
        let selection = cfg.index.find_symbol(&mut search_mixins).await?;
        Ok(Some(selection))
    }
}

impl Display for GenericSelector {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GenericSelector")
    }
}

// ============================================================================
// GenericFilter — @filter
// ============================================================================

fn parse_symbol_types(s: &str) -> Result<Vec<i32>> {
    s.split(',')
        .map(|part| {
            let part = part.trim();
            match part {
                "func" => Ok(SYMBOL_TYPE_FUNCTION),
                "mod" => Ok(SYMBOL_TYPE_MODULE),
                "file" => Ok(SYMBOL_TYPE_FILE),
                "dir" => Ok(SYMBOL_TYPE_DIRECTORY),
                other => bail!("Unknown symbol type: '{}'", other),
            }
        })
        .collect()
}

#[derive(Debug, Clone)]
enum FilterKind {
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

    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        match self {
            FilterKind::Type { symbol_type_ids } => {
                if symbol_type_ids.len() == 1 {
                    vec![Box::new(SymbolTypeMixin::new(symbol_type_ids[0]))]
                } else {
                    vec![Box::new(DefaultSymbolTypeMixin::new(
                        symbol_type_ids.clone(),
                    ))]
                }
            }
            FilterKind::ExactName { value } => {
                vec![Box::new(ExactNameMixin::new(value))]
            }
            FilterKind::CompoundName { value } => {
                vec![Box::new(CompoundNameMixin::new(value))]
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
            .ok_or_else(|| anyhow!("@filter requires a kind as first argument"))?;

        let value = positional
            .get(1)
            .ok_or_else(|| anyhow!("@filter requires a value as second argument"))?;

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
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        self.kind.get_filter_mixins()
    }
}

impl Display for GenericFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "GenericFilter({})", self.kind)
    }
}

/// DefaultSymbolTypeMixin - filters symbols by multiple type IDs (OR condition).
///
/// Applied to all five query axes:
/// - `filter_current`: constrains which symbols this command matches
/// - `filter_parents`/`filter_children`: constrains the *caller* side of refs queries
///   to this command's types. This is intentional — by default, only functions appear
///   as callers. To see module-level refs, use `@module { "foo" }` which sets the
///   inherited default types to [MODULE, FUNCTION].
/// - `filter_has_parents`/`filter_has_children`: constrains containment queries
#[derive(Debug, Clone)]
pub struct DefaultSymbolTypeMixin {
    pub symbol_type_ids: Vec<i32>,
}

impl DefaultSymbolTypeMixin {
    pub fn new(symbol_type_ids: Vec<i32>) -> Self {
        Self { symbol_type_ids }
    }
}

impl SymbolSearchMixin for DefaultSymbolTypeMixin {
    fn filter_current<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::CurrentQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::CurrentQuery<'a>> {
        use diesel::prelude::*;
        use index::schema_diesel::symbols;

        let types = self.symbol_type_ids.clone();
        Ok(query.filter(symbols::dsl::symbol_type.eq_any(types)))
    }

    /// Filter parent_symbols in the parents query (who calls me) to this command's types.
    /// This ensures only instances of matching types can "own" refs.
    fn filter_parents<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::mixins::ParentsQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::mixins::ParentsQuery<'a>> {
        use diesel::prelude::*;
        use index::db_diesel::mixins::PARENT_SYMBOLS_ALIAS;
        use index::schema_diesel::symbols;

        let types = self.symbol_type_ids.clone();
        Ok(query.filter(
            PARENT_SYMBOLS_ALIAS
                .field(symbols::dsl::symbol_type)
                .eq_any(types),
        ))
    }

    /// Filter parent_symbols in the children query (my callees) to this command's types.
    /// This ensures only instances of matching types can "own" refs.
    fn filter_children<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::mixins::ChildrenQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::mixins::ChildrenQuery<'a>> {
        use diesel::prelude::*;
        use index::db_diesel::mixins::PARENT_SYMBOLS_ALIAS;
        use index::schema_diesel::symbols;

        let types = self.symbol_type_ids.clone();
        Ok(query.filter(
            PARENT_SYMBOLS_ALIAS
                .field(symbols::dsl::symbol_type)
                .eq_any(types),
        ))
    }

    fn filter_has_parents<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::mixins::HasParentsQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::mixins::HasParentsQuery<'a>> {
        use diesel::prelude::*;
        use index::schema_diesel::symbols;

        let types = self.symbol_type_ids.clone();
        Ok(query.filter(symbols::dsl::symbol_type.eq_any(types)))
    }

    fn filter_has_children<'a>(
        &self,
        _connection: &mut index::db_diesel::Connection,
        query: index::db_diesel::mixins::HasChildrenQuery<'a>,
    ) -> anyhow::Result<index::db_diesel::mixins::HasChildrenQuery<'a>> {
        use diesel::prelude::*;
        use index::schema_diesel::symbols;

        let types = self.symbol_type_ids.clone();
        Ok(query.filter(symbols::dsl::symbol_type.eq_any(types)))
    }
}


