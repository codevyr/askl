use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, ParserContext, PositionalArgument, Rule};
use crate::symbols::{SymbolId, SymbolRefs};
use anyhow::{anyhow, bail, Result};
use core::fmt::Debug;
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::{HashMap, HashSet};
use std::sync::Arc;

fn build_generic_verb(
    _ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Arc<dyn Verb>> {
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

    let _span = ident.as_span();
    match Identifier::build(ident)?.0.as_str() {
        NameSelector::NAME => NameSelector::new(&positional, &named),
        IgnoreVerb::NAME => IgnoreVerb::new(&positional, &named),
        unknown => Err(anyhow!("Unknown filter: {}", unknown)),
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

    match verb.as_rule() {
        Rule::generic_verb => build_generic_verb(ctx, verb),
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

#[derive(PartialEq, Debug, Copy, Clone)]
pub enum Resolution {
    None,
    Weak,
    Strong,
}

impl Resolution {
    pub fn max(self, other: Resolution) -> Self {
        if self == Resolution::Strong || other == Resolution::Strong {
            return Resolution::Strong;
        }

        if self == Resolution::Weak || other == Resolution::Weak {
            return Resolution::Weak;
        }

        Resolution::None
    }
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

pub trait Verb: Debug {
    fn resolution(&self) -> Resolution {
        Resolution::Weak
    }

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
        bail!("Not a filter verb")
    }
}

pub trait Filter: Debug {
    fn filter(&self, _cfg: &ControlFlowGraph, symbols: SymbolRefs) -> SymbolRefs {
        symbols
    }
}

pub trait Selector: Debug {
    fn select(&self, _cfg: &ControlFlowGraph, symbols: SymbolRefs) -> Option<SymbolRefs> {
        Some(symbols)
    }
}

pub trait Deriver: Debug {
    fn derive_symbols(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> Option<SymbolRefs>;

    fn derive_children(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> Option<SymbolRefs>;
}

#[derive(Debug)]
struct NameSelector {
    name: String,
}

impl NameSelector {
    const NAME: &'static str = "select";

    fn new(_positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
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

    fn resolution(&self) -> Resolution {
        Resolution::Strong
    }
}

impl Selector for NameSelector {
    fn select(&self, cfg: &ControlFlowGraph, symbols: SymbolRefs) -> Option<SymbolRefs> {
        let res: SymbolRefs = symbols
            .into_iter()
            .filter(|(s, _)| self.name == cfg.get_symbol(&s).unwrap().name)
            .collect();

        if res.len() == 0 {
            return None;
        }
        Some(res)
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

    fn resolution(&self) -> Resolution {
        Resolution::Weak
    }
}

impl Deriver for ForcedVerb {
    fn derive_symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<SymbolRefs> {
        Some(cfg.symbols.get_children(symbol).clone())
    }

    fn derive_children(&self, cfg: &ControlFlowGraph, _symbol: &SymbolId) -> Option<SymbolRefs> {
        let sym_refs: SymbolRefs = cfg
            .get_symbol_by_name(&self.name)
            .iter()
            .fold(SymbolRefs::new(), |mut acc, refs| {
                acc.insert(refs.id, HashSet::new());
                acc
            });
        if sym_refs.is_empty() {
            return None;
        }

        Some(sym_refs)
    }
}

impl Selector for ForcedVerb {
    fn select(&self, cfg: &ControlFlowGraph, _symbols: SymbolRefs) -> Option<SymbolRefs> {
        let sym_refs: SymbolRefs = cfg
            .get_symbol_by_name(&self.name)
            .iter()
            .fold(SymbolRefs::new(), |mut acc, refs| {
                acc.insert(refs.id, HashSet::new());
                acc
            });
        if sym_refs.is_empty() {
            return None;
        }

        Some(sym_refs)
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

impl Deriver for UnitVerb {
    fn derive_children(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> Option<SymbolRefs> {
        None
    }

    fn derive_symbols(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> Option<SymbolRefs> {
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

impl Deriver for ChildrenVerb {
    fn derive_symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<SymbolRefs> {
        Some(cfg.symbols.get_children(symbol).clone())
    }

    fn derive_children(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<SymbolRefs> {
        Some(cfg.symbols.get_children(symbol).clone())
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
    fn filter(&self, cfg: &ControlFlowGraph, symbols: SymbolRefs) -> SymbolRefs {
        symbols
            .into_iter()
            .filter(|(s, _)| self.name != cfg.get_symbol(s).unwrap().name)
            .collect()
    }
}
