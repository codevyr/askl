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
    CompoundNameMixin, IgnoreFilterMixin, Index, ParentReference,
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
        NameSelector::NAME => NameSelector::new(verb_span, &positional, &named),
        IgnoreVerb::NAME => IgnoreVerb::new(verb_span, &positional, &named),
        ProjectFilter::NAME => ProjectFilter::new(verb_span, &positional, &named),
        ForcedVerb::NAME => ForcedVerb::new(verb_span, &positional, &named),
        IsolatedScope::NAME => IsolatedScope::new(verb_span, &positional, &named),
        LabelVerb::NAME => LabelVerb::new(verb_span, &positional, &named),
        UserVerb::NAME => UserVerb::new(verb_span, &positional, &named),
        PreambleVerb::NAME => PreambleVerb::new(verb_span, &positional, &named),
        HasModifier::NAME => HasModifier::new(verb_span, &positional, &named),
        RefsModifier::NAME => RefsModifier::new(verb_span, &positional, &named),
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
    pub(super) const NAME: &'static str = "select";

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

    async fn derive_from_ref_parent(
        &self,
        ctx: &mut ExecutionContext,
        _index: &Index,
        _selector_filters: &[&dyn Filter],
        parent: &Statement,
    ) -> Result<Option<Selection>> {
        let parent_selection = match parent.get_selection(ctx) {
            Some(selection) => selection,
            None => return Ok(None),
        };

        let cached_selection = self.selection.get().cloned();

        let mut normal_selection = match cached_selection {
            Some(selection) => selection,
            None => {
                println!(
                    "ForcedVerb: No symbols found with name {}",
                    self.name.as_str()
                );
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
        ctx.set_relationship_type(RelationshipType::Has);
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
        ctx.set_relationship_type(RelationshipType::Refs);
        Ok(true) // consumed - don't add to command
    }
}

impl Display for RefsModifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "RefsModifier")
    }
}

/// TypeSelector - selects symbols by type (@function, @file, @module, @directory)
/// Optionally filters by name pattern
#[derive(Debug)]
pub(super) struct TypeSelector {
    span: Span,
    symbol_type_id: i32,
    name_pattern: Option<String>,
}

impl TypeSelector {
    pub(super) const NAME_FUNCTION: &'static str = "function";
    pub(super) const NAME_FILE: &'static str = "file";
    pub(super) const NAME_MODULE: &'static str = "module";
    pub(super) const NAME_DIRECTORY: &'static str = "directory";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        _named: &HashMap<String, String>,
        symbol_type_id: i32,
    ) -> Result<Arc<dyn Verb>> {
        let name_pattern = positional.first().cloned();
        Ok(Arc::new(Self {
            span,
            symbol_type_id,
            name_pattern,
        }))
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

    /// Set default symbol types for child scopes.
    /// When @module is used, children should include both module and function types by default.
    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        use crate::parser_context::SYMBOL_TYPE_FUNCTION;

        // Set default types for children: parent's type + function
        let mut default_types = vec![self.symbol_type_id];
        if self.symbol_type_id != SYMBOL_TYPE_FUNCTION {
            default_types.push(SYMBOL_TYPE_FUNCTION);
        }
        ctx.set_default_symbol_types(default_types);

        // Don't consume - still add this verb to the command
        Ok(false)
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
        let mut search_mixins = search_mixins;
        search_mixins.push(Box::new(SymbolTypeMixin::new(self.symbol_type_id)));
        if let Some(ref name) = self.name_pattern {
            search_mixins.push(Box::new(CompoundNameMixin::new(name)));
        }
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

/// DefaultSymbolTypeMixin - filters symbols by multiple type IDs (OR condition)
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

        // Clone to avoid lifetime issues with eq_any
        let types = self.symbol_type_ids.clone();
        // Filter by any of the symbol types (OR condition)
        Ok(query.filter(symbols::dsl::symbol_type.eq_any(types)))
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
