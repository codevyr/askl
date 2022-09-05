use crate::cfg::ControlFlowGraph;
use crate::parser::{ Identifier, NamedArgument, Rule};
use core::fmt::Debug;
use pest::error::Error;
use std::collections::HashMap;

pub fn build_verb(pair: pest::iterators::Pair<Rule>) -> Result<Box<dyn Verb>, Error<Rule>> {
    let mut pair = pair.into_inner();
    let ident = pair.next().unwrap();
    let args = pair.map(NamedArgument::build).collect::<Result<Vec<_>, _>>()?;

    let positional = vec![];
    let mut named = HashMap::new();
    for arg in args.into_iter() {
        named.insert(arg.name.0, arg.value.0);
    }

    let span = ident.as_span();
    match Identifier::build(ident)?.0.as_str() {
        FilterVerb::NAME => Ok(Box::new(FilterVerb::new(positional, named))),
        unknown => Err(Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!("Unknown filter: {}", unknown),
            },
            span,
        ))?,
    }
}

pub trait Verb: Debug {
    fn apply(&self, cfg: &ControlFlowGraph) -> ControlFlowGraph;
}

#[derive(Debug)]
struct FilterVerb {
    positional: Vec<String>,
    named: HashMap<String, String>,
}

impl FilterVerb {
    const NAME: &'static str = "filter";

    fn new(positional: Vec<String>, named: HashMap<String, String>) -> Self {
        Self { positional, named }
    }
}

impl Verb for FilterVerb {
    fn apply(&self, cfg: &ControlFlowGraph) -> ControlFlowGraph {
        println!("VERB: {:#?}", self);
        cfg.clone()
    }
}
