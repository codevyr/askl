use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::Rule;
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::symbols::Location;
use crate::verb::{build_verb, AllVerb, Verb};
use core::fmt::Debug;
use log::debug;
use pest::error::Error;

#[derive(Debug)]
pub struct DefaultStatement {
    pub verb: Box<dyn Verb>,
    pub scope: Box<dyn Scope>,
}

pub fn build_statement<'a>(
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut verb: Box<dyn Verb> = AllVerb::new_default();
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    for pair in pair.into_inner() {
        match pair.as_rule() {
            Rule::verb => {
                verb = build_verb(pair)?;
            }
            Rule::scope => {
                scope = build_scope(pair)?;
            }
            _ => Err(Error::new_from_span(
                pest::error::ErrorVariant::ParsingError {
                    positives: vec![Rule::verb, Rule::scope],
                    negatives: vec![pair.as_rule()],
                },
                pair.as_span(),
            ))?,
        }
    }

    Ok(Box::new(DefaultStatement {
        verb: verb,
        scope: scope,
    }))
}

pub trait Statement: Debug {
    fn find_matches<'a>(
        &self,
        cfg: &'a ControlFlowGraph,
        parent: &'a Location,
    ) -> (NodeList<'a>, EdgeList<'a>);
    fn run<'a>(&self, cfg_in: &'a ControlFlowGraph) -> (NodeList<'a>, EdgeList<'a>);
}

impl Statement for DefaultStatement {
    fn find_matches<'a>(
        &self,
        cfg: &'a ControlFlowGraph,
        parent: &'a Location,
    ) -> (NodeList<'a>, EdgeList<'a>) {
        let matches = cfg
            .get_symbol(parent)
            .iter()
            .filter(|symbol| self.verb.mark(symbol))
            .map(| _s| parent)
            .collect::<Vec<_>>();

        let mut combined_sources = NodeList(vec![]);
        let mut combined_edges = EdgeList(vec![]);
        for parent_match in matches.iter() {
            let (new_sources, new_edges) = self.scope.find_matches(cfg, parent_match);
            combined_sources.0.extend(new_sources.0.into_iter());
            combined_edges.0.extend(new_edges.0.into_iter());
        }
        (combined_sources, combined_edges)
    }

    fn run<'a>(&self, cfg: &'a ControlFlowGraph) -> (NodeList<'a>, EdgeList<'a>) {
        let matches = cfg
            .iter_symbols()
            .filter(|(_location, symbol)| self.verb.mark(symbol))
            .map(|(l, _s)| l)
            .collect::<Vec<_>>();

        let (scope_sources, scope_edges) = self.scope.run(cfg);
        let (combined_matches, mut combined_edges) =
            self.scope.combine(cfg, NodeList(matches), scope_sources);
        // debug!(
        //     "NODES: {:#?}",
        //     combined_matches
        //         .0
        //         .iter()
        //         .map(|n| cfg.get_symbol(n).unwrap().name.clone())
        //         .collect::<Vec<_>>()
        // );
        // debug!(
        //     "EDGES: {:#?}",
        //     combined_edges
        //         .0
        //         .iter()
        //         .map(|e| format!(
        //             "{}->{}",
        //             cfg.get_symbol(e.0).unwrap().name.clone(),
        //             cfg.get_symbol(e.1).unwrap().name.clone()
        //         ))
        //         .collect::<Vec<_>>()
        // );
        combined_edges.0.extend(scope_edges.0.iter());
        (combined_matches, combined_edges)
    }
}
