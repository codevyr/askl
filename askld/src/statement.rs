use crate::cfg::{self, ControlFlowGraph, EdgeList, NodeList};
use crate::command::Command;
use crate::execution_context::ExecutionContext;
use crate::execution_state::ExecutionState;
use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::scope::{build_scope, EmptyScope, Scope, StatementIter};
use crate::verb::build_verb;
use core::fmt::Debug;
use core::panic;
use index::symbols::{DeclarationId, DeclarationRefs};
use pest::error::Error;
use std::cell::{RefCell, RefMut};
use std::collections::HashSet;
use std::ptr;
use std::rc::{Rc, Weak};

pub fn build_statement<'a>(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Rc<Statement>, Error<Rule>> {
    let mut iter = pair.into_inner();
    let sub_ctx = ParserContext::derive(ctx);
    let mut scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    for pair in iter.by_ref() {
        match pair.as_rule() {
            Rule::verb => {
                build_verb(sub_ctx.clone(), pair)?;
            }
            Rule::scope => {
                scope = build_scope(sub_ctx.clone(), pair)?;
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

    let statement = Statement::new(sub_ctx.command(), scope.clone());
    scope.set_parent(Rc::downgrade(&statement));

    Ok(statement)
}

pub fn build_empty_statement(ctx: Rc<ParserContext>) -> Rc<Statement> {
    let scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    let sub_ctx = ParserContext::derive(ctx);
    let verb = sub_ctx.command();
    let statement = Statement::new(verb.into(), scope.clone());
    scope.set_parent(Rc::downgrade(&statement));
    return statement;
}

#[derive(Debug)]
pub struct Statement {
    pub command: Command,
    pub scope: Rc<dyn Scope>,
    pub parent: RefCell<Option<Weak<Statement>>>,
    pub execution_state: RefCell<ExecutionState>,
}

impl Statement {
    pub fn new(command: Command, scope: Rc<dyn Scope>) -> Rc<Statement> {
        Rc::new(Statement {
            command: command,
            scope: scope,
            parent: RefCell::new(None),
            execution_state: RefCell::new(ExecutionState::new()),
        })
    }

    fn id(&self) -> usize {
        ptr::addr_of!(self) as usize
    }

    pub fn command(&self) -> &Command {
        &self.command
    }

    pub fn scope(&self) -> Rc<dyn Scope> {
        self.scope.clone()
    }

    pub fn get_state(&self) -> RefMut<ExecutionState> {
        self.execution_state.borrow_mut()
    }

    pub async fn update_edges(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        nodes: &NodeList,
    ) -> EdgeList {
        let mut edges = EdgeList::new();
        for node_i in nodes.0.iter() {
            for node_j in nodes.0.iter() {
                if node_i == node_j {
                    continue;
                };
                let derived = self
                    .command()
                    .derive_parents(ctx, self, cfg, *node_i)
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

    async fn compute_nodes(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Option<HashSet<DeclarationId>> {
        let mut statements = vec![];
        crate::scope::visit(self.scope(), &mut |statement| {
            statements.push(statement);
            true
        })
        .ok()?;

        println!(
            "Executing global statement with {} statements",
            statements.len()
        );

        // First, execute all selectors
        statements.iter_mut().for_each(|statement| {
            statement
                .get_state()
                .select_nodes(ctx, cfg, statement.as_ref());
        });

        while !statements.iter().all(|s| s.get_state().completed) {
            // Find an uncompleted statement with the least number of nodes. If
            // there are no such statements, pick any uncompleted statement.
            statements.sort_by_key(|s| {
                s.get_state()
                    .current
                    .as_ref()
                    .map_or(usize::MAX, |refs| refs.len())
            });

            println!("All statements:");
            statements.iter().for_each(|s| {
                let ss = s.get_state();
                println!(
                    "Statement: {:?}, completed: {}, current: {:?}",
                    s.command(),
                    ss.completed,
                    ss.current
                );
            });

            let mut uncompleted_statements: Vec<_> = statements
                .iter_mut()
                .filter(|s| !s.get_state().completed)
                .collect();
            if uncompleted_statements.is_empty() {
                panic!("No uncompleted statements found, this should not happen");
            }

            let current_node = &mut uncompleted_statements[0];

            if let Some(current_state) = current_node.get_state().current.as_ref() {
                let current_declarations = current_state
                    .iter()
                    .map(|id| *id)
                    .collect::<HashSet<DeclarationId>>();

                current_node
                    .update_parents(ctx, cfg, &current_declarations)
                    .await;

                current_node
                    .update_children(ctx, cfg, &current_declarations)
                    .await;
            };

            current_node.get_state().completed = true;
        }

        statements.iter_mut().for_each(|statement| {
            println!(
                "Final state for statement {:?}: {:?}",
                statement.command(),
                statement.get_state().current
            );
        });

        let mut all_nodes = HashSet::new();
        for statement in &statements {
            all_nodes.extend(statement.get_state().nodes_iter().copied());
        }

        Some(all_nodes)
    }

    pub async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        _symbols: Option<DeclarationRefs>,
        _ignored_symbols: &HashSet<DeclarationId>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
        let all_nodes = self.compute_nodes(ctx, cfg).await?;
        println!("All nodes: {:?}", all_nodes);

        let mut statements = vec![];
        crate::scope::visit(self.scope(), &mut |statement| {
            statements.push(statement);
            true
        })
        .ok()?;

        let mut all_references = EdgeList::new();
        for statement in statements.iter() {
            for node in statement.get_state().nodes_iter() {
                let parent_references = statement
                    .command()
                    .derive_parents(ctx, statement.as_ref(), cfg, *node)
                    .await;
                println!(
                    "Node: {:?}, parent references: {:?}",
                    node, parent_references
                );
                if parent_references.is_none() {
                    continue;
                }

                let parent_references = parent_references.unwrap();
                for (parent_id, occurrences) in parent_references {
                    if !all_nodes.contains(&parent_id) {
                        continue;
                    }
                    for occurrence in occurrences.iter() {
                        all_references.add_reference(parent_id, *node, Some(occurrence.clone()));
                    }
                }
            }
        }

        for (from, to, occurrences) in all_references.as_vec().iter() {
            println!(
                "All references: {:?} -> {:?} @ {:?}",
                from,
                to,
                occurrences.iter().collect::<Vec<_>>()
            );
        }

        return Some((
            DeclarationRefs::new(),
            cfg::NodeList(all_nodes),
            all_references,
        ));
    }

    async fn update_parents(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        current_declarations: &HashSet<DeclarationId>,
    ) {
        let mut all_parent_references = HashSet::new();
        for current in current_declarations.iter() {
            let parent_references = self
                .command()
                .derive_parents(ctx, self, cfg, *current)
                .await;
            println!(
                "Current node: {:?}, parent references: {:?}",
                current, parent_references
            );
            if let Some(parent_references) = parent_references {
                for (parent_id, _) in parent_references {
                    all_parent_references.insert(parent_id);
                }
            }
        }

        if let Some(parent) = self.parent() {
            let filtered_declarations = parent
                .upgrade()
                .unwrap()
                .command()
                .filter_nodes(cfg, all_parent_references);
            println!(
                "Retaining state for parent: {:?} with references: {:?}",
                parent, filtered_declarations
            );
            parent
                .upgrade()
                .unwrap()
                .get_state()
                .retain(ctx, &filtered_declarations);
        }
    }

    async fn update_children(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        current_declarations: &HashSet<DeclarationId>,
    ) {
        let children_references = self
            .command()
            .derive_children(self, ctx, cfg, current_declarations.clone())
            .await;
        let mut children_ids = HashSet::new();
        for d in children_references.iter() {
            let declarations = cfg.get_declarations_from_symbols(&vec![d.to]);
            for declaration in declarations {
                children_ids.insert(declaration.0);
            }
        }

        for child in self.children() {
            println!(
                "Current node: {:?}, child references: {:?}",
                child, children_ids
            );
            let filtered_ids = child.command().filter_nodes(cfg, children_ids.clone());
            println!(
                "Retaining state for child: {:?} with references: {:?}",
                child, filtered_ids
            );
            child.get_state().retain(ctx, &filtered_ids);
        }
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
                    .derive_parents(ctx, self, cfg, resolved_declaration_id)
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

    async fn execute_old(
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
                .derive_children(self, ctx, cfg, parent_declaration_ids)
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
            .extend(self.update_edges(ctx, cfg, &res_nodes).await.0.into_iter());

        self.command().mark(ctx, cfg, &res_symbols).unwrap();
        return Some((res_symbols, res_nodes, res_edges));
    }
}

impl Hierarchy for Statement {
    fn parent(&self) -> Option<Weak<Statement>> {
        self.parent.borrow().clone()
    }

    fn set_parent(&self, parent: Weak<Statement>) {
        *self.parent.borrow_mut() = Some(parent);
    }

    fn children(&self) -> StatementIter {
        self.scope().statements()
    }
}
