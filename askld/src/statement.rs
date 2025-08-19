use crate::cfg::{ControlFlowGraph, EdgeList, NodeList};
use crate::command::Command;
use crate::execution_context::ExecutionContext;
use crate::execution_state::ExecutionState;
use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::scope::{build_scope, EmptyScope, Scope, StatementIter};
use crate::verb::build_verb;
use anyhow::Result;
use core::fmt::Debug;
use core::panic;
use index::symbols::{DeclarationId, DeclarationRefs, FileId, Occurrence};
use pest::error::Error;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashSet;
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

    pub fn command(&self) -> &Command {
        &self.command
    }

    pub fn scope(&self) -> Rc<dyn Scope> {
        self.scope.clone()
    }

    pub fn get_state_mut(&self) -> RefMut<'_, ExecutionState> {
        self.execution_state.borrow_mut()
    }

    pub fn get_state(&self) -> Ref<'_, ExecutionState> {
        self.execution_state.borrow()
    }

    async fn update_parents(&self, ctx: &mut ExecutionContext, cfg: &ControlFlowGraph) {
        if let Some(parent) = self.parent() {
            let parent = parent.upgrade().unwrap();
            let has_current = {
                let state = parent.get_state();
                state.current.is_some()
            }; // Drop the immutable borrow here

            if has_current {
                let current_declarations = {
                    let current_state = self.get_state();
                    current_state.current.as_ref().unwrap().parents.clone()
                }; // Drop the borrow here

                let mut state = parent.get_state_mut();
                let parent_selection = state.current.as_mut().unwrap();

                let old_decl_ids = parent_selection.get_decl_ids();
                // If the parent already has a current selection, filter it by the
                // current declarations.

                parent.command().constrain_by_children(
                    cfg,
                    parent_selection,
                    &current_declarations,
                );

                if old_decl_ids != parent_selection.get_decl_ids() {
                    state.completed = false
                }
            } else {
                // If the parent does not have a current selection, derive it from
                // the current declarations.
                let current_declarations = {
                    let current_state = self.get_state();
                    current_state.current.as_ref().unwrap().parents.clone()
                }; // Drop the borrow here

                let parents_selection = parent
                    .command()
                    .derive_parents(ctx, &parent, cfg, &current_declarations)
                    .await;

                if parents_selection.is_none() {
                    return;
                }
                let mut parents_selection = parents_selection.unwrap();

                parent.command().filter(cfg, &mut parents_selection);
                let mut state = parent.get_state_mut();
                state.current = Some(parents_selection);
            }
        }
    }

    async fn update_children(&self, ctx: &mut ExecutionContext, cfg: &ControlFlowGraph) {
        for child in self.children() {
            let has_current = {
                let state = child.get_state();
                state.current.is_some()
            }; // Drop the immutable borrow here

            if has_current {
                let current_declarations = {
                    let current_state = self.get_state();
                    current_state.current.as_ref().unwrap().children.clone()
                }; // Drop the borrow here

                let mut state = child.get_state_mut();
                let child_selection = state.current.as_mut().unwrap();

                let old_decl_ids = child_selection.get_decl_ids();

                child
                    .command()
                    .constrain_by_parents(cfg, child_selection, &current_declarations);
                if old_decl_ids != child_selection.get_decl_ids() {
                    state.completed = false
                }
            } else {
                let child: &Statement = child.as_ref();
                let current_declarations = {
                    let current_state = self.get_state();
                    current_state.current.as_ref().unwrap().children.clone()
                }; // Drop the borrow here

                let children_selection = child
                    .command()
                    .derive_children(child, ctx, cfg, &current_declarations)
                    .await;

                if children_selection.is_none() {
                    continue;
                }
                let mut children_selection = children_selection.unwrap();

                child.command().filter(cfg, &mut children_selection);
                let mut state = child.get_state_mut();
                state.current = Some(children_selection);
            }
        }
    }

    async fn compute_nodes(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<Vec<Rc<Statement>>> {
        let mut statements = vec![];
        crate::scope::visit(self.scope(), &mut |statement| {
            statements.push(statement);
            true
        })?;

        println!(
            "Executing global statement with {} statements",
            statements.len()
        );

        // First, execute all selectors
        for statement in statements.iter_mut() {
            statement
                .get_state_mut()
                .select_nodes(ctx, cfg, statement.as_ref())
                .await;
        }

        while !statements.iter().all(|s| s.get_state().completed) {
            // Find an uncompleted statement with the least number of nodes. If
            // there are no such statements, pick any uncompleted statement.
            statements.sort_by_key(|s| {
                s.get_state()
                    .current
                    .as_ref()
                    .map_or(usize::MAX, |refs| refs.nodes.len())
            });

            let mut uncompleted_statements: Vec<_> = statements
                .iter_mut()
                .filter(|s| !s.get_state().completed)
                .collect();
            if uncompleted_statements.is_empty() {
                panic!("No uncompleted statements found, this should not happen");
            }

            let current_node = &mut *uncompleted_statements[0];

            if let Some(selection) = &mut current_node.get_state_mut().current {
                current_node.command().filter(cfg, selection);
                current_node.command().constrain_references(cfg, selection);
            }

            if current_node.get_state().current.as_ref().is_some() {
                current_node.update_parents(ctx, cfg).await;
                current_node.update_children(ctx, cfg).await;
            };

            current_node.get_state_mut().completed = true;
        }

        Ok(statements)
    }

    pub async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        _symbols: Option<DeclarationRefs>,
        _ignored_symbols: &HashSet<DeclarationId>,
    ) -> Option<(DeclarationRefs, NodeList, EdgeList)> {
        let statements = self.compute_nodes(ctx, cfg).await.ok()?;

        let mut all_nodes = Vec::new();
        for statement in &statements {
            all_nodes.extend(statement.get_state_mut().nodes_iter().cloned());
        }

        let complete_selection = all_nodes.clone();

        let all_nodes = HashSet::<DeclarationId>::from_iter(
            all_nodes
                .iter()
                .map(|node| DeclarationId::new(node.declaration.id)),
        );

        let mut all_references = EdgeList::new();
        for statement in &statements {
            let state = statement.get_state_mut();

            let current = if let Some(current) = state.current.as_ref() {
                current
            } else {
                continue;
            };
            for child in &current.children {
                if !all_nodes.contains(&DeclarationId::new(child.symbol_ref.from_decl))
                    || !all_nodes.contains(&DeclarationId::new(child.declaration.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: FileId::new(child.from_file.id),
                    line_start: child.symbol_ref.from_line,
                    column_start: child.symbol_ref.from_col_start,
                    line_end: child.symbol_ref.from_line,
                    column_end: child.symbol_ref.from_col_end,
                };
                all_references.add_reference(
                    DeclarationId::new(child.symbol_ref.from_decl),
                    DeclarationId::new(child.declaration.id),
                    Some(occurrence),
                );
            }

            for parent in &current.parents {
                if !all_nodes.contains(&DeclarationId::new(parent.symbol_ref.from_decl))
                    || !all_nodes.contains(&DeclarationId::new(parent.to_declaration.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: FileId::new(parent.from_file.id),
                    line_start: parent.symbol_ref.from_line,
                    column_start: parent.symbol_ref.from_col_start,
                    line_end: parent.symbol_ref.from_line,
                    column_end: parent.symbol_ref.from_col_end,
                };
                all_references.add_reference(
                    DeclarationId::new(parent.symbol_ref.from_decl),
                    DeclarationId::new(parent.to_declaration.id),
                    Some(occurrence),
                );
            }
        }

        return Some((
            DeclarationRefs::new(),
            NodeList(complete_selection.into_iter().collect()),
            all_references,
        ));
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
