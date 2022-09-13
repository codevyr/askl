use crate::parser::{Identifier, NamedArgument, Rule};
use crate::symbols::Symbol;
use anyhow::{anyhow, bail, Result};
use core::fmt::Debug;
use pest::error::Error;
use std::collections::{HashMap};

pub fn build_verb(pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Verb>, Error<Rule>> {
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
        FilterVerb::NAME => FilterVerb::new(positional, named),
        AllVerb::NAME => AllVerb::new(positional, named),
        unknown => Err(anyhow!("Unknown filter: {}", unknown)),
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
    fn mark(&self, symbol: &Symbol) -> bool;
}

#[derive(Debug)]
struct FilterVerb {
    name: String,
}

impl FilterVerb {
    const NAME: &'static str = "filter";

    fn new(_positional: Vec<String>, named: HashMap<String, String>) -> Result<Box<dyn Verb>> {
        if let Some(name) = named.get("name") {
            Ok(Box::new(Self { name: name.clone() }))
        } else {
            bail!("Must contain name field");
        }
    }
}

impl Verb for FilterVerb {
    fn mark(&self, symbol: &Symbol) -> bool {
        self.name == symbol.name
    }
}

#[derive(Debug)]
pub struct AllVerb {}

impl AllVerb {
    const NAME: &'static str = "all";

    pub fn new(_positional: Vec<String>, _named: HashMap<String, String>) -> Result<Box<dyn Verb>> {
        Ok(Box::new(Self {}))
    }

    pub fn new_default() -> Box<dyn Verb> {
        Box::new(Self {})
    }
}

impl Verb for AllVerb {
    fn mark(&self, _symbol: &Symbol) -> bool {
        true
    }
}
