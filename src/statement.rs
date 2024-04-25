use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::{ParserContext, Rule};
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::symbols::SymbolId;
use crate::verb::{build_verb, ChildrenVerb, CompoundVerb, Resolution, UnitVerb, Verb};
use core::fmt::Debug;
use pest::error::Error;

pub fn build_statement<'a>(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    let mut iter = pair.into_inner();
    let mut sub_ctx = ctx.derive();
    let mut verbs = vec![ChildrenVerb::new()];
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

    Ok(DefaultStatement::new(verb, scope))
}

pub trait Statement: Debug {
    fn update_edges(&self, edges: EdgeList, cfg: &ControlFlowGraph, nodes: &NodeList) -> EdgeList {
        let mut edges = edges;
        for node_i in nodes.0.iter() {
            for node_j in nodes.0.iter() {
                if node_i == node_j {
                    continue;
                };
                let derived = self
                    .verb()
                    .derive_children(cfg, node_i)
                    .or(Some(vec![]))
                    .unwrap();

                derived.into_iter().for_each(|s| {
                    if s.id == *node_j {
                        edges.0.push((node_i.clone(), s.id, s.occurence))
                    }
                })
            }
        }

        edges.0.sort();
        edges.0.dedup();
        edges
    }

    fn execute_all(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolId>) -> (NodeList, EdgeList) {
        let mut res_nodes = NodeList(vec![]);
        let mut res_edges = EdgeList(vec![]);

        for symbol in symbols.into_iter() {
            if let Some((resolution, nodes, edges)) = self.execute(cfg, &symbol, Resolution::Weak) {
                if resolution != Resolution::Strong {
                    continue;
                }

                res_nodes.0.extend(nodes.0.into_iter());
                res_edges.0.extend(edges.0.into_iter());
            }
        }

        res_edges.0.sort();
        res_edges.0.dedup();
        res_nodes.0.sort();
        res_nodes.0.dedup();
        (res_nodes, res_edges)
    }

    fn execute(
        &self,
        cfg: &ControlFlowGraph,
        symbol: &SymbolId,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, NodeList, EdgeList)>;
    fn verb(&self) -> &dyn Verb;
    fn scope(&self) -> &dyn Scope;
}

#[derive(Debug)]
pub struct DefaultStatement {
    pub verb: Box<dyn Verb>,
    pub scope: Box<dyn Scope>,
}

impl DefaultStatement {
    fn new(verb: Box<dyn Verb>, scope: Box<dyn Scope>) -> Box<dyn Statement> {
        Box::new(DefaultStatement {
            verb: verb,
            scope: scope,
        })
    }

    pub fn new_main(scope: Box<dyn Scope>) -> Box<dyn Statement> {
        let verbs = vec![UnitVerb::new()];
        let verb: Box<dyn Verb> = CompoundVerb::new(verbs).unwrap();
        Self::new(verb, scope)
    }
}

impl Statement for DefaultStatement {
    fn execute(
        &self,
        cfg: &ControlFlowGraph,
        symbol: &SymbolId,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, NodeList, EdgeList)> {
        let symbols = vec![symbol.clone()];
        let filtered_symbols = if let Some(sym) = self.verb().filter(cfg, symbols) {
            sym
        } else {
            return None;
        };

        let derived_symbols = if let Some(derived) = self.verb().derive(cfg, &filtered_symbols[0]) {
            derived
        } else {
            return None;
        };

        let mut res_edges = EdgeList(vec![]);
        let mut res_nodes = NodeList(vec![]);
        let child_resolution = parent_resolution.max(self.verb().resolution());
        let mut res_resolution = child_resolution;
        for derived_symbol in derived_symbols {
            if let Some((scope_resolution, nodes, edges)) =
                self.scope().run(cfg, &derived_symbol, child_resolution)
            {
                if scope_resolution == Resolution::Strong {
                    res_nodes.0.extend(nodes.0.into_iter());
                    res_edges.0.extend(edges.0.into_iter());
                    res_resolution = res_resolution.max(scope_resolution);
                }
            }
        }

        if res_resolution == Resolution::Strong {
            res_nodes.0.extend(filtered_symbols);
        }
        // Sort and deduplicate the sources
        res_nodes.0.sort();
        res_nodes.0.dedup();

        log::debug!("Statement return {:?}", res_nodes);
        res_edges = self.update_edges(res_edges, cfg, &res_nodes);
        return Some((res_resolution, res_nodes, res_edges));
    }

    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
