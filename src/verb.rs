use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, ParserContext, PositionalArgument, Rule};
use crate::symbols::{SymbolChild, SymbolId};
use anyhow::{anyhow, bail, Result};
use core::fmt::Debug;
use std::sync::Arc;
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;

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
        SelectVerb::NAME => SelectVerb::new(&positional, &named),
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
            SelectVerb::new(&positional, &named)
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

#[derive(Debug, Copy, Clone, PartialEq)]
pub enum VerbRole {
    Select,
    Derive,
    Children,
    Resolution,
    Forced,
    Filter,
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

pub fn derive_verb(verb: &Arc<dyn Verb>) -> Arc<dyn Verb> {
    match verb.derive_method() {
        DeriveMethod::Clone => verb.clone(),
        DeriveMethod::Derive => verb.derive().into(),
        DeriveMethod::Skip => CompoundVerb::new().unwrap().into(),
    }
}

pub enum DeriveMethod {
    Derive,
    Clone,
    Skip
}

pub trait Verb: Debug {
    fn is_role(&self, _role: VerbRole) -> bool {
        true
    }

    fn extend(&mut self, other: Arc<dyn Verb>) {
        unimplemented!("Only CompoundVerb can extend");
    }

    fn resolution(&self) -> Resolution {
        Resolution::Weak
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Skip
    }

    fn derive(&self) -> Box<dyn Verb> {
        unimplemented!("Only CompoundVerb can derive")
    }

    fn derive_symbols(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> Option<Vec<SymbolChild>> {
        None
    }

    fn derive_children(
        &self,
        _cfg: &ControlFlowGraph,
        _symbol: &SymbolId,
    ) -> Option<Vec<SymbolChild>> {
        None
    }

    fn select(
        &self,
        _cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
    ) -> Option<Vec<SymbolChild>> {
        Some(symbols)
    }

    fn filter(&self, _cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        symbols
    }

    fn update_context(&self, _ctx: &mut ParserContext) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct CompoundVerb {
    verbs: Vec<Arc<dyn Verb>>,
    unit_verb: Arc<dyn Verb>,
}

impl CompoundVerb {
    fn verb_role(&self, role: VerbRole) -> &Arc<dyn Verb> {
        for v in self.verbs.iter() {
            if v.is_role(role) {
                return &v;
            }
        }

        &self.unit_verb
    }

    pub fn new() -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self { verbs: vec![], unit_verb: UnitVerb::new() }))
    }

    fn new_verbs(verbs: Vec<Arc<dyn Verb>>) -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self { verbs , unit_verb: UnitVerb::new()}))
    }

    fn verbs<'a>(&'a self, role: VerbRole) -> Option<Vec<&'a Arc<dyn Verb>>> {
        let role_verbs: Vec<_> = self.verbs.iter().filter(|v| v.is_role(role)).collect();

        if role_verbs.len() == 0 {
            return None;
        }

        Some(role_verbs)
    }
}

impl Verb for CompoundVerb {
    fn derive(&self) -> Box<dyn Verb> {
        let mut res = vec![];
        for verb in self.verbs.iter() {
            match verb.derive_method() {
                DeriveMethod::Clone => res.push(verb.clone()),
                DeriveMethod::Derive => res.push(verb.derive().into()),
                DeriveMethod::Skip => {},
            }
        };

        CompoundVerb::new_verbs(res).unwrap()
    }

    fn extend(&mut self, other: Arc<dyn Verb>) {
        self.verbs.push(other);
    }

    fn derive_symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<Vec<SymbolChild>> {
        if let Some(mut res) = self.verb_role(VerbRole::Derive).derive_symbols(cfg, symbol) {
            res.sort();
            res.dedup();
            Some(res)
        } else {
            None
        }
    }

    fn resolution(&self) -> Resolution {
        let mut res = Resolution::Weak;
        for v in self.verbs.iter() {
            res = res.max(v.resolution());
        }

        res
    }

    fn derive_children(
        &self,
        cfg: &ControlFlowGraph,
        symbol: &SymbolId,
    ) -> Option<Vec<SymbolChild>> {
        self.verb_role(VerbRole::Children)
            .derive_children(cfg, symbol)
    }

    fn filter(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        if let Some(verbs) = self.verbs(VerbRole::Filter) {
            verbs
                .into_iter()
                .fold(symbols, |symbols, verb| verb.filter(cfg, symbols))
        } else {
            symbols
        }
    }

    fn select(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
    ) -> Option<Vec<SymbolChild>> {
        if let Some(verbs) = self.verbs(VerbRole::Select) {
            let mut verb_results = verbs
                .into_iter()
                .filter_map(|v| v.select(cfg, symbols.clone()))
                .peekable();

            if verb_results.peek().is_none() {
                return None;
            }

            Some(verb_results.flatten().collect())
        } else {
            Some(symbols)
        }
    }
}

#[derive(Debug)]
struct SelectVerb {
    name: String,
}

impl SelectVerb {
    const NAME: &'static str = "select";

    fn new(_positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Arc<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Arc::new(Self { name: name.clone() }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for SelectVerb {
    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Select || role == VerbRole::Resolution
    }

    fn resolution(&self) -> Resolution {
        Resolution::Strong
    }

    fn select(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
    ) -> Option<Vec<SymbolChild>> {
        let res: Vec<_> = symbols
            .into_iter()
            .filter_map(|s| {
                if self.name == cfg.get_symbol(&s.id).unwrap().name {
                    return Some(s);
                }
                None
            })
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
    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Select || role == VerbRole::Resolution
    }

    fn resolution(&self) -> Resolution {
        Resolution::Weak
    }

    fn select(
        &self,
        cfg: &ControlFlowGraph,
        _symbols: Vec<SymbolChild>,
    ) -> Option<Vec<SymbolChild>> {
        let id = SymbolId::new(self.name.clone());
        if let Some(_) = cfg.get_symbol(&id) {
            Some(vec![SymbolChild {
                id: id,
                occurence: None,
            }])
        } else {
            None
        }
    }

    fn derive_children(
        &self,
        cfg: &ControlFlowGraph,
        _symbol: &SymbolId,
    ) -> Option<Vec<SymbolChild>> {
        let id = SymbolId::new(self.name.clone());
        if let Some(_) = cfg.get_symbol(&id) {
            Some(vec![SymbolChild {
                id: id,
                occurence: None,
            }])
        } else {
            None
        }
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

impl Verb for UnitVerb {}

#[derive(Debug)]
pub struct ChildrenVerb {}

impl ChildrenVerb {
    pub fn new() -> Arc<dyn Verb> {
        Arc::new(Self {})
    }
}

impl Verb for ChildrenVerb {
    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Children || role == VerbRole::Derive
    }

    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn derive_symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<Vec<SymbolChild>> {
        Some(cfg.symbols.get_children(symbol))
    }

    fn derive_children(
        &self,
        cfg: &ControlFlowGraph,
        symbol: &SymbolId,
    ) -> Option<Vec<SymbolChild>> {
        Some(cfg.symbols.get_children(symbol))
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
    fn derive_method(&self) -> DeriveMethod {
        DeriveMethod::Clone
    }

    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Filter
    }

    fn filter(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        symbols
            .into_iter()
            .filter(|s| self.name != cfg.get_symbol(&s.id).unwrap().name)
            .collect()
    }
}
