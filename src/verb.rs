use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, ParserContext, Rule};
use crate::scope::ScopeFactory;
use crate::symbols::{SymbolChild, SymbolId};
use anyhow::{anyhow, bail, Result};
use core::fmt::Debug;
use log::debug;
use pest::error::Error;
use pest::error::ErrorVariant::CustomError;
use std::collections::HashMap;

fn build_generic_verb(
    ctx: &ParserContext,
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

pub trait Verb: Debug {
    fn filter(&self, _cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        symbols
    }

    fn update_context(&self, _ctx: &mut ParserContext) -> bool {
        false
    }
}

#[derive(Debug)]
pub struct CompoundVerb {
    filter_verb: Vec<Box<dyn Verb>>,
}

impl CompoundVerb {
    const NAME: &'static str = "verb";

    pub fn new(verbs: Vec<Box<dyn Verb>>) -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self {
            filter_verb: verbs,
        }))
    }
}

impl Verb for CompoundVerb {
    fn filter(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        self.filter_verb
            .iter()
            .fold(symbols, |symbols, verb| verb.filter(cfg, symbols))
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
    fn filter(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolChild>) -> Vec<SymbolChild> {
        symbols
            .into_iter()
            .filter_map(|s| {
                if self.name == cfg.get_symbol(&s.symbol_id).unwrap().name {
                    return Some(s);
                }
                None
            })
            .collect()
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

impl Verb for UnitVerb {
}
