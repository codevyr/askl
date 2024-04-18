use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
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
    ) -> Option<(Vec<SymbolChild>, NodeList, EdgeList)>;
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
    ) -> Option<(Vec<SymbolChild>, NodeList, EdgeList)> {
        let verb_passed_symbols: Vec<SymbolChild> = symbols
            .iter()
            .filter(|s| self.verb().symbols(cfg, &s.symbol_id))
            .map(|s| s.clone())
            .collect();

        if verb_passed_symbols.len() == 0 {
            return None;
        }

        log::debug!(
            "Default statement scope {:?} symbol {:?}",
            self.scope,
            symbols
        );
        let mut passed_symbols = vec![];
        let mut res_edges = EdgeList(vec![]);
        let mut res_nodes = NodeList(vec![]);
        for passed_symbol in verb_passed_symbols.into_iter() {
            let children = self.scope().get_children(cfg, &passed_symbol.symbol_id);
            let (scoped_children, nodes, edges) = self.scope.run(cfg, &children);
            log::debug!(
                "Default statement matched {:?} symbol {:?}",
                scoped_children,
                edges
            );
            scoped_children.iter().for_each(|c| {
                if let Some(occurence) = &c.occurence {
                    res_edges.0.push((
                        passed_symbol.symbol_id.clone(),
                        c.symbol_id.clone(),
                        occurence.clone(),
                    ))
                }
            });
            passed_symbols.push(passed_symbol);
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
        }

        log::debug!("Statement return {:?}", passed_symbols);
        return Some((passed_symbols, res_nodes, res_edges));
    }

    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
