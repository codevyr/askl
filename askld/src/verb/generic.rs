use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::hierarchy::Hierarchy;
use crate::parser::{Identifier, NamedArgument, PositionalArgument, Rule};
use crate::parser_context::ParserContext;
use crate::statement::Statement;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use index::db_diesel::{ChildReference, ParentReference, Selection};
use index::models_diesel::SymbolRef;
use index::symbols::{self, package_match};
use index::symbols::{clean_and_split_string, partial_name_match, DeclarationId, SymbolId};
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;
use std::rc::Rc;
use std::sync::Arc;

use super::labels::{LabellerVerb, UserVerb};
use super::preamble::PreambleVerb;
use super::{DeriveMethod, Deriver, Filter, Selector, Verb};

pub(crate) fn build_generic_verb(
    _ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>, Error<Rule>> {
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
        NameSelector::NAME => NameSelector::new(&positional, &named),
        IgnoreVerb::NAME => IgnoreVerb::new(&positional, &named),
        ModuleFilter::NAME => ModuleFilter::new(&positional, &named),
        ForcedVerb::NAME => ForcedVerb::new(&positional, &named),
        IsolatedScope::NAME => IsolatedScope::new(&positional, &named),
        LabellerVerb::NAME => LabellerVerb::new(&positional, &named),
        UserVerb::NAME => UserVerb::new(&positional, &named),
        PreambleVerb::NAME => PreambleVerb::new(&positional, &named),
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
    pub name: String,
}

impl NameSelector {
    pub(super) const NAME: &'static str = "select";

    pub fn new(
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self { name: name.clone() }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for NameSelector {
    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Selector for NameSelector {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<Selection> {
        let symbols = cfg.find_symbol_by_name(self.name.as_str()).await;

        symbols
    }
}

#[derive(Debug)]
pub(super) struct ForcedVerb {
    name: String,
}

impl ForcedVerb {
    pub(super) const NAME: &'static str = "forced";

    pub fn new(
        _positional: &Vec<String>,
        named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self { name: name.clone() }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for ForcedVerb {
    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Deriver for ForcedVerb {
    async fn derive_children_impl(
        &self,
        statement: &Statement,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        _children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        let parent_statement = match statement.parent() {
            Some(parent) => parent,
            None => return None,
        };
        let parent_statement = match parent_statement.upgrade() {
            Some(parent) => parent,
            None => return None,
        };
        let parent_state = parent_statement.get_state();
        let parent_selection = match parent_state.current.as_ref() {
            Some(selection) => selection,
            None => return None,
        };

        let mut normal_selection = match cfg.find_symbol_by_name(self.name.as_str()).await {
            Ok(selection) => selection,
            Err(_) => {
                println!(
                    "ForcedVerb: No symbols found with name {}",
                    self.name.as_str()
                );
                return None;
            }
        };

        let mut fake_parent_references = Vec::<ParentReference>::new();
        for parent_node in parent_selection.nodes.iter() {
            for child_node in normal_selection.nodes.iter() {
                let reference = ParentReference {
                    to_symbol: child_node.symbol.clone(),
                    to_declaration: child_node.declaration.clone(),
                    symbol_ref: SymbolRef {
                        rowid: 0,
                        from_decl: parent_node.declaration.id,
                        to_symbol: child_node.symbol.id,
                        from_file: parent_node.file.id.into(),
                        from_line: parent_node.declaration.line_start as i32,
                        from_col_start: parent_node.declaration.col_start as i32,
                        from_col_end: parent_node.declaration.col_end as i32,
                    },
                };
                fake_parent_references.push(reference);
            }
        }

        normal_selection.parents = fake_parent_references;

        Some(normal_selection)
    }

    async fn derive_parents_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _statement: &Statement,
        _cfg: &ControlFlowGraph,
        _parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        unimplemented!("ForcedVerb does not support derive_parents");
    }

    fn constrain_by_parents_impl(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        _references: &Vec<ChildReference>,
    ) {
        selection.nodes.retain(|s| self.name == s.symbol.name);

        self.constrain_references(cfg, selection);
    }
}

impl Filter for ForcedVerb {
    fn filter_impl(&self, _cfg: &ControlFlowGraph, _selection: &mut Selection) {}
}

#[derive(Debug)]
pub struct UnitVerb {}

impl UnitVerb {
    pub fn new() -> Arc<dyn Verb> {
        Arc::new(Self {})
    }
}

impl Verb for UnitVerb {
    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }
}

#[async_trait(?Send)]
impl Deriver for UnitVerb {
    async fn derive_children_impl(
        &self,
        _statement: &Statement,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        None
    }

    async fn derive_parents_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _statement: &Statement,
        _cfg: &ControlFlowGraph,
        _parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        None
    }
}

#[derive(Debug)]
pub struct ChildrenVerb {}

impl ChildrenVerb {
    pub fn new() -> Arc<dyn Verb> {
        Arc::new(Self {})
    }
}

impl Verb for ChildrenVerb {
    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }
}

#[async_trait(?Send)]
impl Deriver for ChildrenVerb {
    async fn derive_children_impl(
        &self,
        _statement: &Statement,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        let decl_ids = children
            .iter()
            .map(|p| DeclarationId::new(p.declaration.id))
            .collect::<Vec<_>>();

        let children_selection = cfg.index.find_symbol_by_declid(&decl_ids).await.ok()?;

        Some(children_selection)
    }

    async fn derive_parents_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _statement: &Statement,
        cfg: &ControlFlowGraph,
        parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        let decl_ids = parents
            .iter()
            .map(|p| DeclarationId::new(p.symbol_ref.from_decl))
            .collect::<Vec<_>>();
        let parent_selection = cfg.index.find_symbol_by_declid(&decl_ids).await.ok()?;

        Some(parent_selection)
    }
}

#[derive(Debug)]
pub(super) struct IgnoreVerb {
    name: Option<String>,
    package: Option<String>,
}

impl IgnoreVerb {
    pub(super) const NAME: &'static str = "ignore";

    pub fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        let mut verb = Self {
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
    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }
}

impl Filter for IgnoreVerb {
    fn filter_impl(&self, _cfg: &ControlFlowGraph, selection: &mut Selection) {
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

#[derive(Debug)]
pub(super) struct ModuleFilter {
    module: String,
}

impl ModuleFilter {
    pub(super) const NAME: &'static str = "module";

    pub fn new(
        positional: &Vec<String>,
        _named: &HashMap<String, String>,
    ) -> Result<Arc<dyn Verb>> {
        if let Some(module) = positional.iter().next() {
            Ok(Arc::new(Self {
                module: module.clone(),
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for ModuleFilter {
    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        Ok(self)
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }
}

impl Filter for ModuleFilter {
    fn filter_impl(&self, _cfg: &ControlFlowGraph, selection: &mut Selection) {
        selection
            .nodes
            .retain(|s| self.module == s.module.module_name);
    }
}

#[derive(Debug)]
pub(super) struct IsolatedScope {
    _isolated: bool,
}

impl IsolatedScope {
    pub(super) const NAME: &'static str = "scope";

    pub fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
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
            _isolated: isolated,
        }))
    }
}

impl Verb for IsolatedScope {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Deriver for IsolatedScope {
    async fn derive_children_impl(
        &self,
        _statement: &Statement,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        None
    }

    async fn derive_parents_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _statement: &Statement,
        _cfg: &ControlFlowGraph,
        _parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        None
    }
}
