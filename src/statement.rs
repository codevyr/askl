use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::{ParserContext, Rule};
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::symbols::{Symbol, SymbolChild, SymbolId};
use crate::verb::{build_verb, CompoundVerb, UnitVerb, Verb};
use core::fmt::Debug;
use pest::error::Error;

pub fn build_statement<'a>(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    let mut iter = pair.into_inner();
    let mut sub_ctx = ctx.derive();
    let mut verbs = vec![UnitVerb::new()];
    for pair in iter.by_ref() {
        match pair.as_rule() {
            Rule::verb => {
                let new_verb = build_verb(&sub_ctx, pair)?;
                if let Some(verb) = sub_ctx.consume(new_verb) {
                    verbs.push(verb);
                }
            }
            Rule::scope => {
                scope = build_scope(ctx, pair)?;
                break;
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

    let verb: Box<dyn Verb> = CompoundVerb::new(verbs).unwrap();

    if let Some(pair) = iter.next() {
        return Err(Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!("Unexpected token after scope: {}", pair),
            },
            pair.as_span(),
        ));
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
        let filtered_symbols: Vec<SymbolChild> = self.verb().filter(cfg, symbols.clone());

        if filtered_symbols.len() == 0 {
            return None;
        }

        let mut res_symbols = vec![];
        let mut res_edges = EdgeList(vec![]);
        let mut res_nodes = NodeList(vec![]);
        for filtered_symbol in filtered_symbols {
            let derived_symbols: Vec<_> = self.scope().derive(cfg, &filtered_symbol);
            let (passed_symbols, nodes, edges) = self.scope().run(cfg, &derived_symbols);
            if passed_symbols.len() > 0 {
                passed_symbols.iter().for_each(|c| {
                    if let Some(occurence) = &c.occurence {
                        res_edges.0.push((
                            filtered_symbol.symbol_id.clone(),
                            c.symbol_id.clone(),
                            occurence.clone(),
                        ))
                    }
                });
                res_symbols.push(filtered_symbol);
            }
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
        }

        log::debug!("Statement return {:?}", res_symbols);
        return Some((res_symbols, res_nodes, res_edges));
    }

    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
