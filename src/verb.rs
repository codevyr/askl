use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, Rule};
use crate::symbols::{SymbolId, SymbolChild};
use anyhow::{anyhow, bail, Result};
use log::debug;
use core::fmt::Debug;
use pest::error::Error;
use std::collections::HashMap;

fn build_generic_verb(prev_verb: Box<dyn Verb>, pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Verb>> {
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

pub fn build_verb(prev_verb: Box<dyn Verb>, pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Verb>, Error<Rule>> {
    let span = pair.as_span();
    let verb = pair.into_inner()
        .try_fold(
            prev_verb,
            |prev_verb, pair| -> Result<Box<dyn Verb>, Error<Rule>> {
            debug!("Build verb {:#?}", pair);
            match pair.as_rule() {
                Rule::generic_verb => build_generic_verb(prev_verb, pair),
                Rule::plain_filter => {
                    let ident = pair.into_inner().next().unwrap();
                    let positional = vec![];
                    let mut named = HashMap::new();
                    named.insert("name".into(), ident.as_str().into());
                    CompoundVerb::new(&positional, &named)
                },
                _ => unreachable!("Unknown rule: {:#?}", pair.as_rule()),
            }
            .map_err(|e| {
                Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!("Failed to create filter: {}", e),
                    },
                    span,
                )
            })
        });

    return verb;
}

pub trait Verb: Debug {
    fn symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> bool;

    fn filter(&self, _cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        symbols
    }

    fn derive(&self, _cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild> {
        vec![symbol.clone()]
    }
}

#[derive(Debug)]
struct CompoundVerb {
    filter_verb: Box<dyn Verb>,
    derive_verb: Box<dyn Verb>,
}

impl CompoundVerb {
    const NAME: &'static str = "verb";

    fn new(positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self {
            filter_verb: FilterVerb::new(&positional, &named)?,
            derive_verb: ChildrenVerb::new(&positional, &named)?,
        }))
    }
}

impl Verb for CompoundVerb {
    fn symbols(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> bool {
        true
    }

    fn filter(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        self.filter_verb.filter(cfg, symbols)
    }

    fn derive(&self, cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild> {
        self.derive_verb.derive(cfg, symbol)
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
            Ok(Box::new(Self {
                name: name.clone()
            }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for FilterVerb {
    fn symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> bool {
        self.name == cfg.get_symbol(symbol).unwrap().name
    }

    fn filter(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        symbols.into_iter()
        .filter_map(|s| {
            if self.name == cfg.get_symbol(&s.symbol_id).unwrap().name {
                return Some(s)
            }
            None
        })
        .collect()
    }
}

#[derive(Debug)]
struct ChildrenVerb {
}

impl ChildrenVerb {
    const NAME: &'static str = "children";

    fn new(_positional: &Vec<String>, named: &HashMap<String, String>) -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self {}))
    }
}


impl Verb for ChildrenVerb {
    fn symbols(&self, cfg: &ControlFlowGraph, symbol: &SymbolId) -> bool {
        true
    }

    fn derive(&self, cfg: &ControlFlowGraph, symbol: &SymbolChild) -> Vec<SymbolChild> {
        cfg.symbols.get_children(&symbol.symbol_id)
    }
}

/// Returns the same symbols as it have received
#[derive(Debug)]
pub struct UnitVerb {}

impl UnitVerb {
    pub fn new() -> Box<dyn Verb> {
        Box::new(Self{})
    }
}

impl Verb for UnitVerb {
    fn symbols(&self, _cfg: &ControlFlowGraph, _symbol: &SymbolId) -> bool {
        true
    }
}