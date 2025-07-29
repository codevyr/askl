use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::parser::{Identifier, NamedArgument, ParserContext, PositionalArgument, Rule};
use anyhow::{anyhow, bail, Result};
use async_trait::async_trait;
use core::fmt::Debug;
use index::symbols::{
    exact_name_match, partial_name_match, DeclarationId, DeclarationRefs, Occurrence, Reference,
};
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn build_generic_verb(
    _ctx: &ParserContext,
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
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>, Error<Rule>> {
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

    if let Rule::generic_verb = verb.as_rule() {
        return build_generic_verb(ctx, verb);
    }

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
    })
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

    fn update_context(&self, _ctx: &mut ParserContext) -> bool {
        false
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
    fn filter(&self, _cfg: &ControlFlowGraph, symbols: DeclarationRefs) -> DeclarationRefs {
        symbols
    }
}

pub trait Selector: Debug {
    fn select(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: DeclarationRefs,
    ) -> Option<DeclarationRefs>;

    fn select_from_all(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Option<DeclarationRefs>;
}

#[async_trait]
pub trait Deriver: Debug {
    async fn derive_children(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference>;

    async fn derive_parents(
        &self,
        cfg: &ControlFlowGraph,
        declaration: DeclarationId,
    ) -> Option<DeclarationRefs>;
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

impl Selector for NameSelector {
    fn select(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: DeclarationRefs,
    ) -> Option<DeclarationRefs> {
        let res: DeclarationRefs = declarations
            .into_iter()
            .filter_map(|(id, refs)| {
                let d = cfg.get_declaration(id).unwrap();
                let s = cfg.get_symbol(d.symbol).unwrap();
                if let Some(_) = partial_name_match(&self.name)((&d.symbol, &s)) {
                    Some((id, refs.clone()))
                } else {
                    None
                }
            })
            .collect();

        if res.len() == 0 {
            return None;
        }
        Some(res)
    }

    fn select_from_all(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Option<DeclarationRefs> {
        let symbols = cfg.symbols.find_all(partial_name_match(&self.name));
        if symbols.len() == 0 {
            return None;
        }

        Some(cfg.get_declarations_from_symbols(&symbols.into_iter().map(|s| s.id).collect()))
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
    fn as_selector<'a>(&'a self) -> Result<&'a dyn Selector> {
        Ok(self)
    }

    fn as_deriver<'a>(&'a self) -> Result<&'a dyn Deriver> {
        Ok(self)
    }
}

#[async_trait]
impl Deriver for ForcedVerb {
    async fn derive_children(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference> {
        let mut references = HashSet::new();
        let symbols = cfg
            .symbols
            .find_all(exact_name_match(&self.name))
            .into_iter()
            .map(|s| s.id)
            .collect();
        for parent_declaration_id in declarations {
            for (child_declaration_id, _) in cfg.get_declarations_from_symbols(&symbols) {
                let child_symbol = cfg.symbols.declarations.get(&child_declaration_id).unwrap();
                references.insert(Reference::new(parent_declaration_id, child_symbol.symbol));
            }
        }

        references
    }

    async fn derive_parents(
        &self,
        _cfg: &ControlFlowGraph,
        _symbol: DeclarationId,
    ) -> Option<DeclarationRefs> {
        None
    }
}

impl Selector for ForcedVerb {
    fn select(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        _declarations: DeclarationRefs,
    ) -> Option<DeclarationRefs> {
        let symbols = cfg
            .symbols
            .find_all(exact_name_match(&self.name))
            .into_iter()
            .map(|s| s.id)
            .collect();
        let sym_refs: DeclarationRefs = cfg.get_declarations_from_symbols(&symbols).iter().fold(
            DeclarationRefs::new(),
            |mut acc, refs| {
                acc.insert(*refs.0, HashSet::new());
                acc
            },
        );
        if sym_refs.is_empty() {
            return None;
        }

        Some(sym_refs)
    }

    fn select_from_all(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Option<DeclarationRefs> {
        let symbols = cfg
            .symbols
            .find_all(exact_name_match(&self.name))
            .into_iter()
            .map(|s| s.id)
            .collect();
        Some(cfg.get_declarations_from_symbols(&symbols))
    }
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

#[async_trait]
impl Deriver for UnitVerb {
    async fn derive_children(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference> {
        HashSet::new()
    }

    async fn derive_parents(
        &self,
        _cfg: &ControlFlowGraph,
        _symbol: DeclarationId,
    ) -> Option<DeclarationRefs> {
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

#[async_trait]
impl Deriver for ChildrenVerb {
    async fn derive_children(
        &self,
        _ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference> {
        let mut references = HashSet::new();
        for parent_declaration_id in declarations {
            let parent_declaration = cfg
                .symbols
                .declarations
                .get(&parent_declaration_id)
                .unwrap();
            for reference in cfg.index.get_children(parent_declaration_id).await.unwrap() {
                references.insert(Reference::new_occurrence(
                    parent_declaration_id,
                    reference.to_symbol,
                    Occurrence {
                        line_start: reference.from_line as i32,
                        line_end: reference.from_line as i32,
                        column_start: reference.from_col_start as i32,
                        column_end: reference.from_col_end as i32,
                        file: parent_declaration.file_id,
                    },
                ));
            }
        }

        references
    }

    async fn derive_parents(
        &self,
        cfg: &ControlFlowGraph,
        child_declaration_id: DeclarationId,
    ) -> Option<DeclarationRefs> {
        let mut references = DeclarationRefs::new();
        for reference in cfg.index.get_parents(child_declaration_id).await.unwrap() {
            let parent_declaration = cfg.symbols.declarations.get(&reference.from_decl).unwrap();
            let occ = Occurrence {
                line_start: reference.from_line as i32,
                line_end: reference.from_line as i32,
                column_start: reference.from_col_start as i32,
                column_end: reference.from_col_end as i32,
                file: parent_declaration.file_id,
            };

            references
                .entry(reference.from_decl)
                .and_modify(|s| {
                    s.insert(occ.clone());
                })
                .or_insert_with(|| HashSet::from([occ]));
        }

        Some(references)
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
    fn filter(&self, cfg: &ControlFlowGraph, declarations: DeclarationRefs) -> DeclarationRefs {
        declarations
            .into_iter()
            .filter(|(declaration_id, _)| {
                let declaration = cfg.get_declaration(*declaration_id).unwrap();
                self.name != cfg.get_symbol(declaration.symbol).unwrap().name
            })
            .collect()
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
    fn filter(&self, cfg: &ControlFlowGraph, declarations: DeclarationRefs) -> DeclarationRefs {
        let module = if let Some(module) = cfg.find_module(&self.module) {
            module
        } else {
            return DeclarationRefs::new();
        };

        declarations
            .into_iter()
            .filter(|(declaration_id, _)| {
                let declaration = cfg.get_declaration(*declaration_id).unwrap();
                let file = cfg.get_file(declaration.file_id).unwrap();
                module.id == file.module
            })
            .collect()
    }
}

#[derive(Debug)]
struct IsolatedScope {}

impl IsolatedScope {
    const NAME: &'static str = "scope";

    fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        if positional.len() > 0 {
            bail!("Unexpected positional arguments");
        }

        if named.len() > 0 {
            bail!("Unexpected named arguments");
        }

        Ok(Arc::new(Self {}))
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

#[async_trait]
impl Deriver for IsolatedScope {
    async fn derive_children(
        &self,
        _ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
        _declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference> {
        HashSet::new()
    }

    async fn derive_parents(
        &self,
        _cfg: &ControlFlowGraph,
        _declaration: DeclarationId,
    ) -> Option<DeclarationRefs> {
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

#[async_trait]
impl Deriver for UserVerb {
    async fn derive_children(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: HashSet<DeclarationId>,
    ) -> HashSet<Reference> {
        let mut references = HashSet::new();
        for parent_declaration_id in declarations {
            for child_declaration_id in ctx.saved_labels.get(&self.label).unwrap() {
                let child_declaration =
                    cfg.symbols.declarations.get(&child_declaration_id).unwrap();
                references.insert(Reference::new(
                    parent_declaration_id,
                    child_declaration.symbol,
                ));
            }
        }

        references
    }

    async fn derive_parents(
        &self,
        _cfg: &ControlFlowGraph,
        _declaration: DeclarationId,
    ) -> Option<DeclarationRefs> {
        None
    }
}

impl Selector for UserVerb {
    fn select_from_all(
        &self,
        ctx: &mut ExecutionContext,
        _cfg: &ControlFlowGraph,
    ) -> Option<DeclarationRefs> {
        if let Some(ids) = ctx.saved_labels.get(&self.label) {
            Some(ids.into_iter().map(|id| (*id, HashSet::new())).collect())
        } else {
            // bail!("Label {} does not exist", self.label);
            None
        }
    }

    fn select(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        declarations: DeclarationRefs,
    ) -> Option<DeclarationRefs> {
        if self.forced {
            return self.select_from_all(ctx, cfg);
        }

        let saved_ids = if let Some(saved) = ctx.saved_labels.get(&self.label) {
            saved
        } else {
            return None;
        };

        Some(
            declarations
                .into_iter()
                .filter(|(id, _)| saved_ids.contains(id))
                .collect(),
        )
    }
}
