use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, ParserContext, Rule};
use crate::symbols::{SymbolChild, SymbolId};
use anyhow::{anyhow, bail, Result};
use core::fmt::Debug;
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;

fn build_generic_verb(
    _ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Verb>> {
    let mut pair = pair.into_inner();
    let ident = pair.next().unwrap();
    let args = pair
        .map(NamedArgument::build)
        .collect::<Result<Vec<_>, _>>()?;

    let positional = vec![];
    let mut named = HashMap::new();
    for arg in args.into_iter() {
        named.insert(arg.name.0, arg.value.0);
    }

    let span = ident.as_span();
    match Identifier::build(ident)?.0.as_str() {
        FilterVerb::NAME => FilterVerb::new(&positional, &named),
        unknown => Err(anyhow!("Unknown filter: {}", unknown)),
    }
}

pub fn build_verb(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Verb>, Error<Rule>> {
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
            FilterVerb::new(&positional, &named)
        },
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
    Filter,
    Derive,
    Children,
    Resolution,
    Forced
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

pub trait Verb: Debug {
    fn is_role(&self, _role: VerbRole) -> bool {
        true
    }

    fn resolution(&self) -> Resolution {
        Resolution::Weak
    }

    fn derive(&self, _cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<Vec<SymbolId>> {
        Some(vec![symbol.clone()])
    }

    fn derive_children(
        &self,
        _cfg: &ControlFlowGraph,
        _symbol: &SymbolId,
    ) -> Option<Vec<SymbolChild>> {
        None
    }

    fn filter(
        &self,
        _cfg: &ControlFlowGraph,
        symbols: Vec<SymbolId>,
    ) -> Option<Vec<SymbolId>> {
        Some(symbols)
    }

    fn update_context(&self, _ctx: &mut ParserContext) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct CompoundVerb {
    verbs: Vec<Box<dyn Verb>>,
}

impl CompoundVerb {
    const NAME: &'static str = "verb";

    fn verb_role(&self, role: VerbRole) -> &Box<dyn Verb> {
        for v in self.verbs.iter() {
            if v.is_role(role) {
                return v;
            }
        }

        panic!("Role {:?} does not exist", role);
    }

    pub fn new(verbs: Vec<Box<dyn Verb>>) -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self { verbs }))
    }
}

impl Verb for CompoundVerb {
    fn derive(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<Vec<SymbolId>> {
        self.verb_role(VerbRole::Derive).derive(cfg, symbol)
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

    fn filter(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolId>,
    ) -> Option<Vec<SymbolId>> {
        self.verbs
            .iter()
            .filter(|v| v.is_role(VerbRole::Filter))
            .try_fold(symbols, |symbols, verb| {
                verb.filter(cfg, symbols)
            })
    }
}

#[derive(Debug)]
struct FilterVerb {
    name: String,
}

impl FilterVerb {
    const NAME: &'static str = "filter";

    fn new(_positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Box<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Box::new(Self { name: name.clone() }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for FilterVerb {
    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Filter ||
        role == VerbRole::Resolution
    }

    fn resolution(&self) -> Resolution {
        Resolution::Strong
    }

    fn filter(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolId>,
    ) -> Option<Vec<SymbolId>> {
        let res: Vec<_> = symbols
            .into_iter()
            .filter_map(|s| {
                if self.name == cfg.get_symbol(&s).unwrap().name {
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

    fn new(_positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Box<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Box::new(Self { name: name.clone() }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for ForcedVerb {
    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Forced ||
        role == VerbRole::Filter ||
        role == VerbRole::Resolution
    }

    fn resolution(&self) -> Resolution {
        Resolution::Weak
    }

    fn filter(
        &self,
        cfg: &ControlFlowGraph,
        _symbols: Vec<SymbolId>,
    ) -> Option<Vec<SymbolId>> {
        let id = SymbolId::new(self.name.clone());
        if let Some(_) = cfg.get_symbol(&id) {
            Some(vec![id])
        } else {
            None
        }
    }
}

/// Returns the same symbols as it have received
#[derive(Debug)]
pub struct UnitVerb {}

impl UnitVerb {
    pub fn new() -> Box<dyn Verb> {
        Box::new(Self {})
    }
}

impl Verb for UnitVerb {}

#[derive(Debug)]
pub struct ChildrenVerb {}

impl ChildrenVerb {
    pub fn new() -> Box<dyn Verb> {
        Box::new(Self {})
    }
}

impl Verb for ChildrenVerb {
    fn is_role(&self, role: VerbRole) -> bool {
        role == VerbRole::Children || role == VerbRole::Derive
    }

    fn derive(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> Option<Vec<SymbolId>> {
        Some(
            cfg.symbols
                .get_children(symbol)
                .into_iter()
                .map(|s| s.id)
                .collect(),
        )
    }

    fn derive_children(
        &self,
        cfg: &ControlFlowGraph,
        symbol: &SymbolId,
    ) -> Option<Vec<SymbolChild>> {
        Some(cfg.symbols.get_children(symbol))
    }
}
