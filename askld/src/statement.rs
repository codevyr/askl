use crate::cfg::{ControlFlowGraph, EdgeList, NodeList, SymbolDeclId};
use crate::command::{Command, LabeledStatements};
use crate::execution_context::ExecutionContext;
use crate::execution_state::{
    DependencyRole, ExecutionState, StatementDependency, StatementDependent,
};
use crate::hierarchy::Hierarchy;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::scope::{build_scope, EmptyScope, Scope, StatementIter};
use crate::verb::build_verb;
use anyhow::{bail, Result};
use core::fmt::Debug;
use index::db_diesel::Selection;
use index::symbols::{DeclarationId, DeclarationRefs, FileId, Occurrence, SymbolId};
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

    let command = sub_ctx.command();
    let statement = Statement::new(command, scope.clone());
    scope.set_parent(Rc::downgrade(&statement));

    Ok(statement)
}

pub fn build_empty_statement(ctx: Rc<ParserContext>) -> Rc<Statement> {
    let scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    let sub_ctx = ParserContext::derive(ctx);
    let command = sub_ctx.command();
    let statement = Statement::new(command, scope.clone());
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

    pub fn dependency_ready(&self, dependency_role: DependencyRole) -> bool {
        self.command()
            .selectors()
            .all(|selector| selector.dependency_ready(dependency_role))
    }

    // A lower score means higher priority for execution
    fn statement_score(&self, ctx: &ExecutionContext) -> usize {
        let mandatory_completed = self
            .get_state()
            .dependencies
            .iter()
            .all(|dep| dep.dependency.dependency_ready(dep.dependency_role));
        if !mandatory_completed {
            return usize::MAX;
        }

        let mut total_score: Option<usize> = None;
        ctx.registry
            .for_each_selector(self.command().selectors(), |selector, state| {
                match (total_score, selector.score(state)) {
                    (None, Some(score)) => {
                        total_score = Some(score);
                    }
                    (Some(current_score), Some(selection)) => {
                        total_score = Some(current_score.saturating_add(selection));
                    }
                    (Some(_), None) => {
                        total_score = Some(usize::MAX);
                    }
                    (None, None) => {
                        total_score = Some(usize::MAX);
                    }
                }
            });
        total_score.or(Some(usize::MAX)).unwrap()
    }

    pub fn get_selection(&self, ctx: &ExecutionContext) -> Option<Selection> {
        let mut selection: Option<Selection> = None;
        ctx.registry
            .for_each_selector(self.command().selectors(), |selector, state| {
                if let Some(sel) = selector.get_selection(state) {
                    if let Some(current_selection) = &mut selection {
                        current_selection.extend(sel.clone());
                    } else {
                        selection = Some(sel.clone());
                    }
                } else {
                    selection = None;
                }
            });
        selection
    }

    fn is_selection_some(&self, ctx: &ExecutionContext) -> bool {
        let mut is_some = true;
        ctx.registry
            .for_each_selector(self.command().selectors(), |selector, sel_state| {
                if selector.get_selection(sel_state).is_none() {
                    is_some = false;
                }
            });
        is_some
    }

    // Statements that have dependencies resolved and are ready to execute
    async fn compute_selectors(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        statements: &Vec<Rc<Statement>>,
    ) {
        let _select_nodes: tracing::span::EnteredSpan =
            tracing::info_span!("select_nodes").entered();

        for statement in statements.iter() {
            statement.command().compute_selected(ctx, cfg).await;

            if !statement.command().has_selectors() {
                statement.get_state_mut().completed = true;
            }
        }
    }

    fn init_dependencies(&self, labeled_statements_map: &LabeledStatements) -> Result<()> {
        crate::scope::visit(self.scope(), &mut |statement| -> Result<bool> {
            init_dependencies(statement, &labeled_statements_map)?;
            Ok(true)
        })?;
        Ok(())
    }

    /// Mark weak statements.
    ///
    /// A statement is weak if it contains only unit verbs AND one of:
    /// - Has no parent
    /// - Has no children
    /// - All its children are weak
    /// - Its parent is weak
    fn mark_weak_statements(&self, statements: &Vec<Rc<Statement>>) {
        // This iterative algorithm is inefficient but the dependency
        // graph is expected to be small.
        let mut changed = true;
        while changed {
            changed = false;
            for statement in statements.iter() {
                let mut state = statement.get_state_mut();
                if state.weak {
                    continue;
                }

                let is_unit_statement = statement.command().is_unit();

                if !is_unit_statement {
                    continue;
                }

                let parent_weak = if let Some(parent_weak) = statement
                    .parent()
                    .and_then(|p| p.upgrade())
                    .map(|p| p.get_state().weak)
                {
                    parent_weak
                } else {
                    true
                };

                let all_children_weak = statement.children().all(|child| child.get_state().weak);

                if parent_weak || all_children_weak {
                    state.weak = true;
                    changed = true;
                }
            }
        }
    }

    async fn compute_nodes(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<Vec<Rc<Statement>>> {
        let mut labeled_statements = LabeledStatements::new();
        let mut statements = vec![];
        crate::scope::visit(self.scope(), &mut |statement| {
            statements.push(statement.clone());
            labeled_statements.remember(statement);
            Ok(true)
        })?;

        // First, execute all selectors
        self.compute_selectors(ctx, cfg, &statements).await;

        self.init_dependencies(&labeled_statements)?;

        self.mark_weak_statements(&statements);

        while !statements.iter().all(|s| s.get_state().completed) {
            let _statement_iteration: tracing::span::EnteredSpan =
                tracing::info_span!("statement_iteration").entered();

            // Find an uncompleted statement with the least number of nodes. If
            // there are no such statements, pick any uncompleted statement.
            statements.sort_by_key(|s| s.statement_score(ctx));
            let current_statement = statements
                .iter_mut()
                .filter(|s| !s.get_state().completed)
                .next()
                .expect("No uncompleted statements found, this should not happen");

            ctx.registry.for_each_selector_mut(
                current_statement.command().selectors(),
                |selector, state| {
                    selector.update_state(state);

                    if let Some(selection) = selector.get_selection_mut(state) {
                        current_statement.command().filter(selection);
                    }
                },
            );

            current_statement.get_state_mut().completed = true;
            // TODO: The default case for statement not to have selection is
            // something like a standalone unit verb. But if the query cannot be
            // resolved, it also results in no selection. We should distinguish
            // these two cases when implementing error reporting.
            if !current_statement.is_selection_some(ctx) {
                continue;
            };

            // Notify dependents
            let dependents = current_statement.get_state().dependents.clone();
            for dependent in dependents {
                current_statement.notify(ctx, cfg, &dependent).await?;
            }
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
            all_nodes.extend(
                statement
                    .get_selection(ctx)
                    .iter()
                    .map(|s| s.nodes.clone())
                    .flatten(),
            );
        }

        let complete_selection = all_nodes.clone();

        let all_nodes = HashSet::<DeclarationId>::from_iter(
            all_nodes
                .iter()
                .map(|node| DeclarationId::new(node.declaration.id)),
        );

        let mut all_references = EdgeList::new();
        for statement in &statements {
            let current = if let Some(current) = statement.get_selection(ctx) {
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
                    SymbolDeclId {
                        symbol_id: SymbolId::new(child.parent_symbol.id),
                        declaration_id: DeclarationId::new(child.symbol_ref.from_decl),
                    },
                    SymbolDeclId {
                        symbol_id: SymbolId::new(child.symbol_ref.to_symbol),
                        declaration_id: DeclarationId::new(child.declaration.id),
                    },
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
                    file: parent.symbol_ref.from_file.unwrap().into(),
                    line_start: parent.symbol_ref.from_line,
                    column_start: parent.symbol_ref.from_col_start,
                    line_end: parent.symbol_ref.from_line,
                    column_end: parent.symbol_ref.from_col_end,
                };
                all_references.add_reference(
                    SymbolDeclId {
                        symbol_id: SymbolId::new(parent.from_declaration.symbol),
                        declaration_id: DeclarationId::new(parent.symbol_ref.from_decl),
                    },
                    SymbolDeclId {
                        symbol_id: SymbolId::new(parent.to_symbol.id),
                        declaration_id: DeclarationId::new(parent.to_declaration.id),
                    },
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

    /// Notify the dependent statement's execution state about change in the
    /// state of a dependency.
    pub async fn notify(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        dependent: &StatementDependent,
    ) -> Result<()> {
        let _update_dependency: tracing::span::EnteredSpan =
            tracing::info_span!("notify").entered();
        let changed = dependent
            .statement
            .command()
            .accept_notification(ctx, &cfg.index, self, dependent.dependency_role)
            .await?;

        if changed {
            dependent.statement.get_state_mut().completed = false;
        }
        Ok(())
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

pub fn init_dependencies(
    statement: Rc<Statement>,
    labeled_statements_map: &LabeledStatements,
) -> Result<()> {
    let mut state = statement.get_state_mut();
    if let Some(parent) = statement.parent().and_then(|p| p.upgrade()) {
        // Add a parent as a dependent
        state.dependents.push(StatementDependent::new(
            parent.clone(),
            DependencyRole::Parent,
        ));

        // Add ourself as a dependency to the parent
        parent
            .get_state_mut()
            .dependencies
            .push(StatementDependency::new(
                statement.clone(),
                DependencyRole::Parent,
            ));
    }

    for child in statement.children() {
        state.dependents.push(StatementDependent::new(
            child.clone(),
            DependencyRole::Child,
        ));

        // Add ourself as a dependency to the child
        child
            .get_state_mut()
            .dependencies
            .push(StatementDependency::new(
                statement.clone(),
                DependencyRole::Child,
            ));
    }

    // For every user verb, add current statement as dependent to the labeled statements
    for user in statement.command().selectors() {
        let Some(label) = user.get_label() else {
            continue;
        };
        let labeled_statements =
            if let Some(labeled_statements) = labeled_statements_map.get_statements(&label) {
                labeled_statements
            } else {
                bail!("Label '{}' not found for user selector", label);
            };

        for labeled_statement in labeled_statements {
            labeled_statement
                .get_state_mut()
                .dependents
                .push(StatementDependent::new_user(
                    statement.clone(),
                    label.as_str(),
                ));

            // Add ourself as a dependency to the labeled statement
            state.dependencies.push(StatementDependency::new(
                labeled_statement.clone(),
                DependencyRole::User,
            ));
        }
    }

    Ok(())
}
