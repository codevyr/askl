use crate::cfg::{ControlFlowGraph, EdgeList};
use crate::parser::Rule;
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::symbols::{Symbol, SymbolChild, SymbolId};
use crate::verb::{build_verb, UnitVerb, Verb};
use core::fmt::Debug;
use pest::error::Error;

pub fn build_statement<'a>(
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut verb: Box<dyn Verb> = UnitVerb::new();
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    for pair in pair.into_inner() {
        match pair.as_rule() {
            Rule::verb => {
                verb = build_verb(verb, pair)?;
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
    fn execute(
        &self,
        cfg: &ControlFlowGraph,
        symbol: &Vec<SymbolChild>,
    ) -> Option<(Vec<SymbolChild>, EdgeList)>;
    fn verb(&self) -> &dyn Verb;
    fn scope(&self) -> &dyn Scope;
}

#[derive(Debug)]
pub struct DefaultStatement {
    pub verb: Box<dyn Verb>,
    pub scope: Box<dyn Scope>,
}

impl Statement for DefaultStatement {
    fn execute(
        &self,
        cfg: &ControlFlowGraph,
        symbols: &Vec<SymbolChild>,
    ) -> Option<(Vec<SymbolChild>, EdgeList)> {
        let passed_children: Vec<SymbolChild> = symbols
            .iter()
            .filter(|s| self.verb().symbols(cfg, &s.symbol_id))
            .map(|s| s.clone())
            .collect();

        if passed_children.len() == 0 {
            return None;
        }

        log::debug!(
            "Default statement scope {:?} symbol {:?}",
            self.scope,
            symbols
        );
        if let Some((nodes, edges)) = self.scope.run(
            cfg,
            &passed_children
                .iter()
                .map(|s| s.symbol_id.clone())
                .collect(),
        ) {
            log::debug!("Default statement matched {:?} symbol {:?}", nodes, edges);

            return Some((passed_children, edges));
        }

        None
    }

    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
