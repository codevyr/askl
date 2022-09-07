use crate::cfg::ControlFlowGraph;
use crate::parser::{Identifier, NamedArgument, Rule};
use crate::symbols::Location;
use anyhow::{anyhow, bail, Result};
use core::fmt::Debug;
use itertools::Itertools;
use pest::error::Error;
use std::collections::{HashMap, HashSet};

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
    fn apply(&self, cfg: &ControlFlowGraph) -> ControlFlowGraph;
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
    fn apply(&self, cfg: &ControlFlowGraph) -> ControlFlowGraph {
        println!("VERB: {:#?}", self);
        let mut matched_sinks = HashSet::new();
        let mut result = ControlFlowGraph::new();
        for sink in cfg.iter_sink() {
            if self.name != "" {
                matched_sinks.insert(sink);
            }
        }

        for source in cfg.iter_source() {
            for sink in matched_sinks.iter() {
                for path in cfg.find_paths::<Vec<Location>>(source, *sink, None) {
                    path.iter()
                        .tuple_windows()
                        .map(|(from, to)| {
                            result.add_edge(*from, *to);
                        })
                        .collect()
                }
            }
        }
        result
    }
}
