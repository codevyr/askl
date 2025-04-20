use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::command::Command;
use crate::execution_context::ExecutionContext;
use crate::parser::{ParserContext, Rule};
use crate::scope::{build_scope, EmptyScope, Scope};
use crate::verb::build_verb;
use async_trait::async_trait;
use core::fmt::Debug;
use index::symbols::{DeclarationId, DeclarationRefs};
use pest::error::Error;
use std::collections::HashSet;

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

#[async_trait(?Send)]
pub trait Statement: Debug {
    async fn update_edges(&self, cfg: &ControlFlowGraph, nodes: &NodeList) -> EdgeList {
        let mut edges = EdgeList::new();
        for node_i in nodes.0.iter() {
            for node_j in nodes.0.iter() {
                if node_i == node_j {
                    continue;
                };
                let derived = self
                    .command()
                    .derive_parents(cfg, *node_i)
                    .await
                    .or(Some(DeclarationRefs::new()))
                    .unwrap();

                derived
                    .into_iter()
                    .for_each(|(declaration_id, occurences)| {
                        if declaration_id == *node_j {
                            for occ in occurences.into_iter() {
                                edges.add_reference(declaration_id, *node_i, Some(occ))
                            }
                        }
                    })
            }
        }

        edges
    }

    async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<DeclarationRefs>,
        ignored_symbols: &HashSet<DeclarationId>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)>;
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

    async fn execute_for_all(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> (DeclarationRefs, NodeList, EdgeList) {
        let mut res_edges = EdgeList::new();
        let mut res_nodes = NodeList::new();
        let mut res_declarations = DeclarationRefs::new();

        if let Some((resolved_declarations, nodes, edges)) = self.scope().run(ctx, cfg, None).await
        {
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
            res_nodes
                .0
                .extend(resolved_declarations.iter().map(|(s, _)| s.clone()));
            for (resolved_declaration_id, _) in resolved_declarations {
                let resolved_declaration = cfg
                    .symbols
                    .declarations
                    .get(&resolved_declaration_id)
                    .unwrap();
                let derived_declarations = self
                    .command()
                    .derive_parents(cfg, resolved_declaration_id)
                    .await;

                let derived_declarations = if let Some(declarations) = derived_declarations {
                    declarations
                } else {
                    continue;
                };

                let filtered_declarations = self.command().filter(cfg, derived_declarations);
                let selected_declarations = self.command().select(ctx, cfg, filtered_declarations);

                if let Some(selected_declarations) = selected_declarations {
                    for (selected_declaration_id, occurrences) in selected_declarations {
                        res_nodes.add(selected_declaration_id);
                        res_declarations.insert(selected_declaration_id, occurrences.clone());
                        for occurrence in occurrences {
                            res_edges.add_reference(
                                selected_declaration_id,
                                resolved_declaration.id,
                                Some(occurrence),
                            );
                        }
                    }
                }
            }
        }

        return (res_declarations, res_nodes, res_edges);
    }
}

#[async_trait(?Send)]
impl Statement for DefaultStatement {
    async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        parent_declarations: Option<DeclarationRefs>,
        ignored_symbols: &HashSet<DeclarationId>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
        let mut res_edges = EdgeList::new();
        let mut res_nodes = NodeList::new();
        let mut res_symbols = DeclarationRefs::new();

        let filtered_symbols = if let Some(parent_declarations) = parent_declarations {
            let parent_declaration_ids: HashSet<_> =
                parent_declarations.into_iter().map(|(d, _)| d).collect();
            let derived_references = self
                .command()
                .derive_children(ctx, cfg, parent_declaration_ids)
                .await;
            let mut derived_ids = DeclarationRefs::new();
            for d in derived_references.iter() {
                let declarations = cfg.get_declarations_from_symbols(&vec![d.to]);
                for declaration in declarations {
                    derived_ids.insert(declaration.0, HashSet::new());
                }
            }

            let selected_symbols =
                if let Some(selected) = self.command().select(ctx, cfg, Some(derived_ids)) {
                    selected
                } else {
                    return Some((res_symbols, res_nodes, res_edges));
                };

            let filtered_symbols = self.command().filter(cfg, selected_symbols).unwrap();
            let filtered_symbols: DeclarationRefs = filtered_symbols
                .into_iter()
                .filter(|(id, _)| !ignored_symbols.contains(id))
                .collect();
            for reference in derived_references {
                let declarations = cfg.index.symbol_declarations(reference.to).await.unwrap();
                for declaration in declarations {
                    if filtered_symbols.contains_key(&declaration.id) {
                        let declarations_to =
                            cfg.index.symbol_declarations(reference.to).await.unwrap();
                        for declaration_to in declarations_to {
                            res_edges.add_reference(
                                reference.from,
                                declaration_to.id,
                                reference.occurrence.clone(),
                            );
                            break;
                        }
                    }
                }
            }
            filtered_symbols
        } else {
            if let Some(selected) = self.command().select(ctx, cfg, None) {
                self.command().filter(cfg, selected).unwrap()
            } else {
                return Some(self.execute_for_all(ctx, cfg).await);
            }
        };

        let filtered_ids = filtered_symbols.iter().map(|(id, _)| *id).collect();

        if let Some((resolved_symbols, nodes, edges)) =
            self.scope().run_symbols(ctx, cfg, filtered_ids).await
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
            .extend(self.update_edges(cfg, &res_nodes).await.0.into_iter());

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

#[async_trait(?Send)]
impl Statement for GlobalStatement {
    async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        symbols: Option<DeclarationRefs>,
        _ignored_symbols: &HashSet<DeclarationId>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
        let mut res_edges = EdgeList::new();
        let mut res_nodes = NodeList::new();
        if let Some((_, nodes, edges)) = self.scope().run(ctx, cfg, symbols.clone()).await {
            res_nodes.0.extend(nodes.0.into_iter());
            res_edges.0.extend(edges.0.into_iter());
        }

        res_edges
            .0
            .extend(self.update_edges(cfg, &res_nodes).await.0.into_iter());
        return Some((DeclarationRefs::new(), res_nodes, res_edges));
    }

    fn command(&self) -> &Command {
        &self.command
    }

    fn scope(&self) -> &dyn Scope {
        &*self.scope
    }
}
