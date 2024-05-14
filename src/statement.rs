use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::command::Command;
use crate::execution_context::ExecutionContext;
use crate::parser::{ParserContext, Rule};
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::symbols::{Reference, SymbolId, SymbolRefs};
use crate::verb::build_verb;
use core::fmt::Debug;
use pest::error::Error;
use std::collections::{HashMap, HashSet};

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

    Ok(DefaultStatement::new(sub_ctx.command(), scope))
}

pub fn build_empty_statement<'a>(ctx: &ParserContext) -> Box<dyn Statement> {
    let scope: Box<dyn Scope> = Box::new(EmptyScope::new());
    let sub_ctx = ctx.derive();
    let verb = sub_ctx.command();

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
                    .command()
                    .derive_parents(cfg, *node_i)
                    .or(Some(SymbolRefs::new()))
                    .unwrap();

                derived.into_iter().for_each(|(s, occurences)| {
                    if s == *node_j {
                        for occ in occurences.into_iter() {
                            let reference = Reference::new_occurrence(s, *node_i, occ);
                            edges.add_reference(reference)
                        }
                    }
                })
            }
        }

        edges
    }

    fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<SymbolRefs>,
        ignored_symbols: &HashSet<SymbolId>,
    ) -> Option<(SymbolRefs, NodeList, EdgeList)>;
    fn command(&self) -> &Command;
    fn scope(&self) -> &dyn Scope;
}

#[derive(Debug)]
pub struct DefaultStatement {
    pub command: Command,
    pub scope: Box<dyn Scope>,
}

impl DefaultStatement {
    fn new(command: Command, scope: Box<dyn Scope>) -> Box<dyn Statement> {
        Box::new(DefaultStatement {
            command: command,
            scope: scope,
        })
    }

    fn execute_for_all(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> (SymbolRefs, NodeList, EdgeList) {
        let mut res_edges = EdgeList::new();
        let mut res_nodes = NodeList::new();
        let mut res_symbols = HashMap::new();

        if let Some((resolved_symbols, nodes, edges)) = self.scope().run(ctx, cfg, None) {
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
            res_nodes
                .0
                .extend(resolved_symbols.iter().map(|(s, _)| s.clone()));
            for (resolved_symbol, _) in resolved_symbols {
                let derived_symbols = self.command().derive_parents(cfg, resolved_symbol);

                let derived_symbols = if let Some(symbols) = derived_symbols {
                    symbols
                } else {
                    continue;
                };

                let filtered_symbols = self.command().filter(cfg, derived_symbols);
                let selected_symbols = self.command().select(ctx, cfg, filtered_symbols);

                if let Some(selected_symbols) = selected_symbols {
                    for (selected_symbol, occurrences) in selected_symbols {
                        res_nodes.add(selected_symbol);
                        res_symbols.insert(selected_symbol.clone(), occurrences.clone());
                        for occurrence in occurrences {
                            let reference = Reference::new_occurrence(
                                selected_symbol,
                                resolved_symbol,
                                occurrence,
                            );
                            res_edges.add_reference(reference);
                        }
                    }
                }
            }
        }

        return (res_symbols, res_nodes, res_edges);
    }
}

impl Statement for DefaultStatement {
    fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        parent_symbols: Option<SymbolRefs>,
        ignored_symbols: &HashSet<SymbolId>,
    ) -> Option<(SymbolRefs, NodeList, EdgeList)> {
        let mut res_edges = EdgeList::new();
        let mut res_nodes = NodeList::new();
        let mut res_symbols = HashMap::new();

        let filtered_symbols = if let Some(parent_symbols) = parent_symbols {
            let derived_references = self.command().derive_children(ctx, cfg, parent_symbols.clone());
            let mut derived_ids = SymbolRefs::new();
            for d in derived_references.iter() {
                derived_ids.insert(d.to, HashSet::new());
            }

            let selected_symbols =
                if let Some(selected) = self.command().select(ctx, cfg, Some(derived_ids)) {
                    selected
                } else {
                    return Some((res_symbols, res_nodes, res_edges));
                };

            let filtered_symbols = self.command().filter(cfg, selected_symbols).unwrap();
            let filtered_symbols: SymbolRefs = filtered_symbols
                .into_iter()
                .filter(|(id, _)| !ignored_symbols.contains(id))
                .collect();
            for reference in derived_references {
                if filtered_symbols.contains_key(&reference.to) {
                    res_edges.add_reference(reference);
                }
            }
            filtered_symbols
        } else {
            if let Some(selected) = self.command().select(ctx, cfg, None) {
                self.command().filter(cfg, selected).unwrap()
            } else {
                return Some(self.execute_for_all(ctx, cfg));
            }
        };

        let filtered_ids = filtered_symbols.iter().map(|(id, _)| *id).collect();

        if let Some((resolved_symbols, nodes, edges)) =
            self.scope().run_symbols(ctx, cfg, filtered_ids)
        {
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
            res_nodes.0.extend(resolved_symbols.clone());
            // res_symbols.insert(selected_symbol.clone(), occurrences);
        }

        res_nodes
            .0
            .extend(filtered_symbols.iter().map(|(id, _)| *id));
        res_symbols.extend(filtered_symbols.into_iter());

        res_edges
            .0
            .extend(self.update_edges(cfg, &res_nodes).0.into_iter());

        self.command().mark(ctx, cfg, &res_symbols).unwrap();
        return Some((res_symbols, res_nodes, res_edges));
    }

    fn command(&self) -> &Command {
        &self.command
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}

#[derive(Debug)]
pub struct GlobalStatement {
    pub command: Command,
    pub scope: Box<dyn Scope>,
}

impl GlobalStatement {
    pub fn new(scope: Box<dyn Scope>) -> Box<dyn Statement> {
        Box::new(GlobalStatement {
            command: Command::new(),
            scope: scope,
        })
    }
}

impl Statement for GlobalStatement {
    fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<SymbolRefs>,
        ignored_symbols: &HashSet<SymbolId>,
    ) -> Option<(SymbolRefs, NodeList, EdgeList)> {
        let mut res_edges = EdgeList::new();
        let mut res_nodes = NodeList::new();
        if let Some((_, nodes, edges)) = self.scope().run(ctx, cfg, symbols.clone()) {
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
        }

        res_edges
            .0
            .extend(self.update_edges(cfg, &res_nodes).0.into_iter());
        return Some((SymbolRefs::new(), res_nodes, res_edges));
    }

    fn command(&self) -> &Command {
        &self.command
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
