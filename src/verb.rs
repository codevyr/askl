use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, Rule};
use crate::symbols::SymbolId;
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
        FilterVerb::NAME => FilterVerb::new(prev_verb, positional, named),
        AllVerb::NAME => Ok(AllVerb::new()),
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
                    FilterVerb::new(prev_verb, positional, named)
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
    fn symbols(&self, cfg: &ControlFlowGraph, symbols: &Vec<SymbolId>) -> Vec<SymbolId>;
}

#[derive(Debug)]
struct FilterVerb {
    prev: Box<dyn Verb>,
    name: String,
}

impl FilterVerb {
    const NAME: &'static str = "filter";

    fn new(prev_verb: Box<dyn Verb>, _positional: Vec<String>, named: HashMap<String, String>) -> Result<Box<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Box::new(Self {
                prev: prev_verb,
                name: name.clone()
            }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for FilterVerb {
    fn symbols(&self, cfg: &ControlFlowGraph, symbols: &Vec<SymbolId>) -> Vec<SymbolId> {
        self.prev
            .symbols(cfg, symbols)
            .into_iter()
            .filter(|s| {
                self.name == cfg.get_symbol(s).unwrap().name
            })
            .collect()
    }
}

#[derive(Debug)]
pub struct AllVerb {
}

impl AllVerb {
    const NAME: &'static str = "all";

    pub fn new() -> Box<dyn Verb> {
        Box::new(Self {})
    }
}

impl Verb for AllVerb {
    fn symbols(&self, cfg: &ControlFlowGraph, _symbols: &Vec<SymbolId>) -> Vec<SymbolId> {
        cfg
            .iter_symbols()
            .map(|(id, _)| id.clone())
            .collect()
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
    fn symbols(&self, _cfg: &ControlFlowGraph, symbols: &Vec<SymbolId>) -> Vec<SymbolId> {
        symbols.clone()
    }
}