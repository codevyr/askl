use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::execution_state::DependencyRole;
use crate::parser::{Identifier, NamedArgument, PositionalArgument, Rule};
use crate::parser_context::ParserContext;
use crate::span::Span;
use crate::statement::Statement;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use index::db_diesel::{
    CompoundNameMixin, Index, ModuleFilterMixin, ParentReference, ProjectFilterMixin, Selection,
    SymbolSearchMixin,
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
        ModuleFilter::NAME => ModuleFilter::new(verb_span, &positional, &named),
        ForcedVerb::NAME => ForcedVerb::new(verb_span, &positional, &named),
        IsolatedScope::NAME => IsolatedScope::new(verb_span, &positional, &named),
        LabelVerb::NAME => LabelVerb::new(verb_span, &positional, &named),
        UserVerb::NAME => UserVerb::new(verb_span, &positional, &named),
        PreambleVerb::NAME => PreambleVerb::new(verb_span, &positional, &named),
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

    async fn derive_from_parent(
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
                    to_declaration: child_node.declaration.clone(),
                    from_declaration: parent_node.declaration.clone(),
                    symbol_ref: SymbolRef {
                        rowid: 0,
                        to_symbol: child_node.symbol.id,
                        from_file: parent_node.file.id,
                        from_offset_start: parent_node.declaration.start_offset as i32,
                        from_offset_end: parent_node.declaration.end_offset as i32,
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
pub(super) struct ModuleFilter {
    span: Span,
    module: String,
}

impl ModuleFilter {
    pub(super) const NAME: &'static str = "module";

    pub fn new(
        span: Span,
        positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        let filter = if let Some(module) = positional.iter().next() {
            Arc::new(Self {
                span,
                module: module.clone(),
            })
        } else {
            bail!("Expected a positional argument");
        };
        Ok(filter)
    }
}

impl Verb for ModuleFilter {
    fn name(&self) -> &str {
        ModuleFilter::NAME
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
        Some(VerbTag::ModuleFilter)
    }

    fn add_verb(&self, existing_verbs: Vec<Arc<dyn Verb>>) -> Vec<Arc<dyn Verb>> {
        self.replace_verb(existing_verbs)
    }
}

impl Filter for ModuleFilter {
    fn get_filter_mixins(&self) -> Vec<Box<dyn SymbolSearchMixin>> {
        vec![Box::new(ModuleFilterMixin::new(&self.module))]
    }
}

impl Display for ModuleFilter {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "ModuleFilter(module={})", self.module)
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
