use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::hierarchy::Hierarchy;
use crate::parser::{Identifier, NamedArgument, PositionalArgument, Rule};
use crate::parser_context::ParserContext;
use crate::statement::Statement;
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use core::fmt::Debug;
use index::db_diesel::{ChildReference, ParentReference, Selection};
use index::models_diesel::SymbolRef;
use index::symbols;
use index::symbols::{
    clean_and_split_string, partial_name_match, DeclarationId, DeclarationRefs, SymbolId,
};
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::{HashMap, HashSet};
use std::rc::Rc;
use std::sync::Arc;
use std::vec;

fn build_generic_verb(
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
            pest::error::ErrorVariant::CustomError {
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
            pest::error::ErrorVariant::CustomError {
                message: format!("Failed to create a generic verb: {}", err),
            },
            span,
        )),
    }
}

pub fn build_verb(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<(), Error<Rule>> {
    let span = pair.as_span();
    debug!("Build verb {:#?}", pair);
    let verb = if let Some(verb) = pair.into_inner().next() {
        verb
    } else {
        return Err(Error::new_from_span(
            CustomError {
                message: format!("Expected a specific rule"),
            },
            span,
        ));
    };

    let verb = if let Rule::generic_verb = verb.as_rule() {
        build_generic_verb(ctx.clone(), verb)?
    } else {
        match verb.as_rule() {
            Rule::plain_filter => {
                let ident = verb.into_inner().next().unwrap();
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".into(), ident.as_str().into());
                NameSelector::new(&positional, &named)
            }
            Rule::forced_verb => {
                let ident = verb.into_inner().next().unwrap();
                let positional = vec![];
                let mut named = HashMap::new();
                named.insert("name".into(), ident.as_str().into());
                ForcedVerb::new(&positional, &named)
            }
            _ => unreachable!("Unknown rule: {:#?}", verb.as_rule()),
        }
        .map_err(|e| {
            Error::new_from_span(
                pest::error::ErrorVariant::CustomError {
                    message: format!("Failed to create filter: {}", e),
                },
                span,
            )
        })?
    };

    let verb = ctx.consume(verb).map_err(|e| {
        Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!("Failed to consume verb: {}", e),
            },
            span,
        )
    })?;

    if let Some(verb) = verb {
        ctx.extend_verb(verb)
    };

    Ok(())
}

pub fn derive_verb(verb: &Arc<dyn Verb>) -> Option<Arc<dyn Verb>> {
    match verb.derive_method() {
        DeriveMethod::Clone => Some(verb.clone()),
        DeriveMethod::Skip => None,
    }
}

pub enum DeriveMethod {
    Clone,
    Skip,
}

pub trait Verb: Debug + Sync {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn update_context(&self, _ctx: &ParserContext) -> Result<bool> {
        Ok(false)
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        bail!("Not a selector verb")
    }

    fn as_filter<'a>(&'a self) -> Result<&'a dyn Filter> {
        bail!("Not a filter verb")
    }

    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        bail!("Not a deriver verb")
    }

    fn as_marker<'a>(&'a self) -> Result<&'a dyn Marker> {
        bail!("Not a marker verb")
    }
}

pub trait Filter: Debug {
    fn filter(&self, cfg: &ControlFlowGraph, selection: &mut Selection) {
        let filter_name = format!("{:?}", self);
        let _filter = tracing::info_span!("filter",
            name = %filter_name,
        )
        .entered();
        self.filter_impl(cfg, selection);
    }

    fn filter_impl(&self, cfg: &ControlFlowGraph, selection: &mut Selection);
}

#[async_trait(?Send)]
pub trait Selector: Debug {
    async fn select_from_all(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<Selection> {
        let select_from_all_name = format!("{:?}", self);
        let _select_from_all =
            tracing::info_span!("select_from_all", name = %select_from_all_name).entered();
        self.select_from_all_impl(ctx, cfg).await
    }

    async fn select_from_all_impl(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<Selection>;
}

#[async_trait(?Send)]
pub trait Deriver: Debug {
    async fn derive_children(
        &self,
        statement: &Statement,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        let derive_children_name = format!("{:?}", self);
        let _derive_children =
            tracing::info_span!("derive_children", name = %derive_children_name).entered();
        self.derive_children_impl(statement, ctx, cfg, children)
            .await
    }

    async fn derive_children_impl(
        &self,
        statement: &Statement,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        children: &Vec<ChildReference>,
    ) -> Option<Selection>;

    async fn derive_parents(
        &self,
        ctx: &mut ExecutionContext,
        statement: &Statement,
        cfg: &ControlFlowGraph,
        parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        let derive_parents_name = format!("{:?}", self);
        let _derive_parents =
            tracing::info_span!("derive_parents", name = %derive_parents_name).entered();
        self.derive_parents_impl(ctx, statement, cfg, parents).await
    }

    async fn derive_parents_impl(
        &self,
        ctx: &mut ExecutionContext,
        statement: &Statement,
        cfg: &ControlFlowGraph,
        parents: &Vec<ParentReference>,
    ) -> Option<Selection>;

    fn constrain_references(&self, _cfg: &ControlFlowGraph, selection: &mut Selection) {
        let constrain_references_name = format!("{:?}", self);
        let _constrain_references =
            tracing::info_span!("constrain_references", name = %constrain_references_name)
                .entered();
        self.constrain_references_impl(_cfg, selection)
    }

    fn constrain_references_impl(&self, _cfg: &ControlFlowGraph, selection: &mut Selection) {
        let node_declaration_ids: HashSet<_> = selection
            .nodes
            .iter()
            .map(|s| DeclarationId::new(s.declaration.id))
            .collect();
        selection
            .parents
            .retain(|c| node_declaration_ids.contains(&DeclarationId::new(c.to_declaration.id)));
        selection
            .children
            .retain(|c| node_declaration_ids.contains(&DeclarationId::new(c.symbol_ref.from_decl)));
    }

    fn constrain_by_parents(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ChildReference>,
    ) {
        let constrain_by_parents_name = format!("{:?}", self);
        let _constrain_by_parents =
            tracing::info_span!("constrain_by_parents", name = %constrain_by_parents_name)
                .entered();
        self.constrain_by_parents_impl(cfg, selection, references)
    }

    fn constrain_by_parents_impl(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ChildReference>,
    ) {
        selection.nodes.retain(|s| {
            references
                .iter()
                .any(|r| r.declaration.id == s.declaration.id)
        });

        self.constrain_references(cfg, selection);
    }

    fn constrain_by_children(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ParentReference>,
    ) {
        let constrain_by_children_name = format!("{:?}", self);
        let _constrain_by_children =
            tracing::info_span!("constrain_by_children", name = %constrain_by_children_name)
                .entered();
        self.constrain_by_children_impl(cfg, selection, references)
    }

    fn constrain_by_children_impl(
        &self,
        cfg: &ControlFlowGraph,
        selection: &mut Selection,
        references: &Vec<ParentReference>,
    ) {
        selection.nodes.retain(|s| {
            references
                .iter()
                .any(|r| r.from_declaration.id == s.declaration.id)
        });

        self.constrain_references(cfg, selection);
    }
}

pub trait Marker: Debug {
    fn mark(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: &DeclarationRefs,
    ) -> Result<()>;
}

#[derive(Debug)]
pub struct NameSelector {
    pub name: String,
}

impl NameSelector {
    const NAME: &'static str = "select";

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
struct ForcedVerb {
    name: String,
}

impl ForcedVerb {
    const NAME: &'static str = "forced";

    fn new(_positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
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
        let parent_statement = statement.parent();
        if parent_statement.is_none() {
            return None;
        }
        let parent_statement = parent_statement.unwrap();
        let parent_statement = parent_statement.upgrade().unwrap();
        let parent_state = parent_statement.get_state();
        let parent_selection = parent_state.current.as_ref().unwrap();

        let normal_selection = cfg.find_symbol_by_name(self.name.as_str()).await;
        if normal_selection.is_err() {
            println!(
                "ForcedVerb: No symbols found with name {}",
                self.name.as_str()
            );
            return None;
        }
        let mut normal_selection = normal_selection.unwrap();

        let mut fake_parent_references = Vec::<ParentReference>::new();
        for parent_node in parent_selection.nodes.iter() {
            for child_node in normal_selection.nodes.iter() {
                let reference = ParentReference {
                    from_file: parent_node.file.clone(),
                    from_symbol: parent_node.symbol.clone(),
                    from_declaration: parent_node.declaration.clone(),
                    to_symbol: child_node.symbol.clone(),
                    to_declaration: child_node.declaration.clone(),
                    symbol_ref: SymbolRef {
                        rowid: 0,
                        from_decl: parent_node.declaration.id,
                        to_symbol: child_node.symbol.id,
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
        // let decl_ids = parents
        //     .iter()
        //     .map(|p| DeclarationId::new(p.declaration.id))
        //     .collect::<Vec<_>>();
        // let parent_selection = cfg
        //     .index_diesel
        //     .find_symbol_by_declid(&decl_ids)
        //     .await
        //     .ok()?;

        // Some(parent_selection)
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

/// Returns the same symbols as it have received
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
            .map(|p| DeclarationId::new(p.from_declaration.id))
            .collect::<Vec<_>>();
        let parent_selection = cfg.index.find_symbol_by_declid(&decl_ids).await.ok()?;

        Some(parent_selection)
    }
}

#[derive(Debug)]
struct IgnoreVerb {
    name: String,
}

impl IgnoreVerb {
    const NAME: &'static str = "ignore";

    fn new(positional: &Vec<String>, _named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        if let Some(name) = positional.iter().next() {
            Ok(Arc::new(Self { name: name.clone() }))
        } else {
            bail!("Expected a positional argument");
        }
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
            let matcher = partial_name_match(&self.name);
            let matched_symbol = matcher((id, &index_symbol));
            let mismatch = matched_symbol.is_none();
            mismatch
        });
    }
}

#[derive(Debug)]
struct ModuleFilter {
    module: String,
}

impl ModuleFilter {
    const NAME: &'static str = "module";

    fn new(positional: &Vec<String>, _named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
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
struct IsolatedScope {
    _isolated: bool,
}

impl IsolatedScope {
    const NAME: &'static str = "scope";

    fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        if positional.len() > 0 {
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
            // Default to false if not specified
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

#[derive(Debug)]
struct LabellerVerb {
    label: String,
}

impl LabellerVerb {
    const NAME: &'static str = "label";

    fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        if named.len() > 0 {
            bail!("Unexpected named arguments");
        }

        if let Some(label) = positional.iter().next() {
            Ok(Arc::new(Self {
                label: label.clone(),
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for LabellerVerb {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_marker<'a>(&'a self) -> Result<&'a dyn Marker> {
        Ok(self)
    }
}

impl Marker for LabellerVerb {
    fn mark(
        &self,
        ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        declarations: &DeclarationRefs,
    ) -> Result<()> {
        let ids: HashSet<_> = declarations.iter().map(|(id, _)| *id).collect();

        if ctx.saved_labels.contains_key(&self.label) {
            bail!("Label {} already exists", self.label);
        }

        ctx.saved_labels.insert(self.label.clone(), ids);

        Ok(())
    }
}

#[derive(Debug)]
struct UserVerb {
    label: String,
    forced: bool,
}

impl UserVerb {
    const NAME: &'static str = "use";

    fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        let forced = if let Some(forced) = named.get("forced") {
            if forced == "true" {
                true
            } else if forced == "false" {
                false
            } else {
                bail!("Unexpected value for forced parameter")
            }
        } else {
            true
        };

        if let Some(label) = positional.iter().next() {
            Ok(Arc::new(Self {
                label: label.clone(),
                forced,
            }))
        } else {
            bail!("Expected a positional argument");
        }
    }
}

impl Verb for UserVerb {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }
}

#[async_trait(?Send)]
impl Deriver for UserVerb {
    async fn derive_children_impl(
        &self,
        _statement: &Statement,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _children: &Vec<ChildReference>,
    ) -> Option<Selection> {
        unimplemented!("UserVerb does not support derive_children");
        // let mut references = HashSet::new();
        // for parent_declaration_id in declarations {
        //     for child_declaration_id in ctx.saved_labels.get(&self.label).unwrap() {
        //         let child_declaration =
        //             cfg.symbols.declarations.get(&child_declaration_id).unwrap();
        //         references.insert(Reference::new(
        //             parent_declaration_id,
        //             child_declaration.symbol,
        //         ));
        //     }
        // }

        // references
    }

    async fn derive_parents_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _statement: &Statement,
        _cfg: &ControlFlowGraph,
        _parents: &Vec<ParentReference>,
    ) -> Option<Selection> {
        unimplemented!("UserVerb does not support derive_parents");
        // let parents = if let Some(parent) = statement.parent() {
        //     parent
        //         .upgrade()
        //         .unwrap()
        //         .get_state_mut()
        //         .current
        //         .clone()
        //         .unwrap()
        // } else {
        //     return None;
        // };

        // let declaration_refs = parents
        //     .parents
        //     .iter()
        //     .filter_map(|decl_res| {
        //         let decl_id = DeclarationId::new(decl_res.declaration.id);
        //         let declaration = cfg.get_declaration(decl_id).unwrap();

        //         Some((
        //             decl_id,
        //             vec![Occurrence {
        //                 line_start: declaration.line_start as i32,
        //                 line_end: declaration.line_end as i32,
        //                 column_start: declaration.col_start as i32,
        //                 column_end: declaration.col_end as i32,
        //                 file: declaration.file_id,
        //             }]
        //             .into_iter()
        //             .collect::<HashSet<_>>(),
        //         ))
        //     })
        //     .collect::<DeclarationRefs>();
        // Some(declaration_refs);
    }
}

#[async_trait(?Send)]
impl Selector for UserVerb {
    async fn select_from_all_impl(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
    ) -> Result<Selection> {
        unimplemented!("UserVerb does not support select_from_all");
    }
}

#[derive(Debug)]
struct PreambleVerb {}

impl PreambleVerb {
    const NAME: &'static str = "preamble";

    fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        if positional.len() > 0 {
            bail!("Unexpected positional arguments");
        };

        if named.len() > 0 {
            bail!("Unexpected named arguments");
        };

        Ok(Arc::new(Self {}))
    }
}

impl Verb for PreambleVerb {
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn update_context(&self, ctx: &ParserContext) -> Result<bool> {
        if ctx.get_prev().is_none() {
            panic!("Expected to have global context");
        }

        let prev = ctx.get_prev().unwrap();
        if let Some(_) = prev.upgrade().unwrap().get_prev() {
            bail!("Preamble verb can only be used as the first verb statement in the askl code");
        }

        ctx.set_alternative_context(prev);

        Ok(true)
    }
}
