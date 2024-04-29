use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::parser::{ParserContext, Rule};
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::symbols::{SymbolChild, SymbolId};
use crate::verb::{build_verb, ChildrenVerb, CompoundVerb, Resolution, UnitVerb, Verb};
use core::fmt::Debug;
use std::sync::Arc;
use pest::error::Error;

pub fn build_statement<'a>(
    ctx: &ParserContext,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Box<dyn Statement>, Error<Rule>> {
    let mut scope: Box<dyn Scope> = Box::new(EmptyScope::new());

    let mut iter = pair.into_inner();
    let mut sub_ctx = ctx.derive();
    for pair in iter.by_ref() {
        match pair.as_rule() {
            Rule::verb => {
                let new_verb = build_verb(&sub_ctx, pair)?;
                if let Some(new_verb) = sub_ctx.consume(new_verb) {
                    sub_ctx.extend_verb(new_verb);
                }
            }
            Rule::scope => {
                scope = build_scope(&sub_ctx, pair)?;
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

    if let Some(pair) = iter.next() {
        return Err(Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!("Unexpected token after scope: {}", pair),
            },
            pair.as_span(),
        ));
    }

    Ok(DefaultStatement::new(sub_ctx.verb().into(), scope))
}

pub fn build_empty_statement<'a>(
    ctx: &ParserContext,
) -> Box<dyn Statement> {
    let scope: Box<dyn Scope> = Box::new(EmptyScope::new());
    let sub_ctx = ctx.derive();
    let verb = sub_ctx.verb();

    DefaultStatement::new(verb.into(), scope)
}

pub trait Statement: Debug {
    fn update_edges(&self, cfg: &ControlFlowGraph, nodes: &NodeList) -> EdgeList {
        let mut edges = EdgeList::new();
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

        edges
    }

    fn execute_all(&self, cfg: &ControlFlowGraph, symbols: Vec<SymbolId>) -> (NodeList, EdgeList) {
        let mut res_nodes = NodeList(vec![]);
        let mut res_edges = EdgeList(vec![]);

        let symbols = symbols
            .into_iter()
            .map(|s| SymbolChild {
                id: s,
                occurence: None,
            })
            .collect();

        if let Some((resolution, resolved_symbols, nodes, edges)) =
            self.execute(cfg, symbols, Resolution::Weak)
        {
            if resolution == Resolution::Strong {
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
        symbols: Vec<SymbolChild>,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, Vec<SymbolChild>, NodeList, EdgeList)>;
    fn verb(&self) -> &dyn Verb;
    fn scope(&self) -> &dyn Scope;
}

#[derive(Debug)]
pub struct DefaultStatement {
    pub verb: Arc<dyn Verb>,
    pub scope: Box<dyn Scope>,
}

impl DefaultStatement {
    fn new(verb: Arc<dyn Verb>, scope: Box<dyn Scope>) -> Box<dyn Statement> {
        Box::new(DefaultStatement {
            verb: verb,
            scope: scope,
        })
    }
}

impl Statement for DefaultStatement {
    fn execute(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, Vec<SymbolChild>, NodeList, EdgeList)> {
        let filtered_symbols = self.verb().filter(cfg, symbols);

        let selected_symbols = if let Some(sym) = self.verb().select(cfg, filtered_symbols) {
            sym
        } else {
            return None;
        };

        let child_resolution = parent_resolution.max(self.verb().resolution());

        let mut res_edges = EdgeList(vec![]);
        let mut res_nodes = NodeList(vec![]);
        let mut res_symbols = vec![];
        let mut res_resolution = child_resolution;

        for selected_symbol in selected_symbols.into_iter() {
            let derived_symbols = if let Some(derived) = self.verb().derive_symbols(cfg, &selected_symbol.id) {
                derived
            } else {
                return None;
            };

            if let Some((scope_resolution, resolved_symbols, nodes, edges)) =
                self.scope().run(cfg, derived_symbols, child_resolution)
            {
                if scope_resolution == Resolution::Strong {
                    res_nodes.0.extend(nodes.0.into_iter());
                    res_edges.0.extend(edges.0.into_iter());
                    res_resolution = res_resolution.max(scope_resolution);
                    res_nodes.0.extend(resolved_symbols.iter().map(|s|s.id.clone()));
                    res_nodes.0.push(selected_symbol.id.clone());
                    res_symbols.push(selected_symbol.clone());

                    for resolved_symbol in resolved_symbols {
                        res_edges.0.push((selected_symbol.id.clone(), resolved_symbol.id, resolved_symbol.occurence));
                    }
                }
            }
        }

        // Sort and deduplicate the sources
        res_nodes.0.sort();
        res_nodes.0.dedup();

        res_edges
            .0
            .extend(self.update_edges(cfg, &res_nodes).0.into_iter());
        res_edges.0.sort();
        res_edges.0.dedup();
        return Some((res_resolution, res_symbols, res_nodes, res_edges));
    }

    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}

#[derive(Debug)]
pub struct GlobalStatement {
    pub verb: Arc<dyn Verb>,
    pub scope: Box<dyn Scope>,
}

impl GlobalStatement {
    pub fn new(scope: Box<dyn Scope>) -> Box<dyn Statement> {
        let verb: Arc<dyn Verb> = CompoundVerb::new().unwrap().into();
        Box::new(GlobalStatement {
            verb: verb,
            scope: scope,
        })
    }
}

impl Statement for GlobalStatement {
    fn execute(
        &self,
        cfg: &ControlFlowGraph,
        symbols: Vec<SymbolChild>,
        parent_resolution: Resolution,
    ) -> Option<(Resolution, Vec<SymbolChild>, NodeList, EdgeList)> {
        let mut res_edges = EdgeList(vec![]);
        let mut res_nodes = NodeList(vec![]);
        let child_resolution = parent_resolution.max(self.verb().resolution());
        let mut res_resolution = child_resolution;
        if let Some((scope_resolution, _, nodes, edges)) =
            self.scope().run(cfg, symbols, child_resolution)
        {
            if scope_resolution == Resolution::Strong {
                res_nodes.0.extend(nodes.0.into_iter());
                res_edges.0.extend(edges.0.into_iter());
                res_resolution = res_resolution.max(scope_resolution);
            }
        }

        // Sort and deduplicate the sources
        res_nodes.0.sort();
        res_nodes.0.dedup();

        res_edges
            .0
            .extend(self.update_edges(cfg, &res_nodes).0.into_iter());
        res_edges.0.sort();
        res_edges.0.dedup();
        return Some((res_resolution, vec![], res_nodes, res_edges));
    }

    fn verb(&self) -> &dyn Verb {
        &*self.verb
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
