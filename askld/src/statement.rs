use crate::cfg::{ControlFlowGraph, EdgeList, NodeList, SymbolDeclId};
use crate::command::{Command, LabeledStatements};
use crate::execution_context::ExecutionContext;
use crate::execution_state::{
    DependencyRole, ExecutionState, RelationshipType, StatementDependency, StatementDependent,
};
use crate::hierarchy::Hierarchy;
use crate::offset_range::range_bounds_to_offsets;
use crate::parser::Rule;
use crate::parser_context::ParserContext;
use crate::scope::{build_scope, EmptyScope, Scope, StatementIter};
use crate::span::Span;
use crate::verb::{build_verb, DefaultTypeFilter};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::Selection;
use index::symbols::{DeclarationId, FileId, Occurrence, SymbolId};
use pest::error::Error;
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashSet;
use std::rc::{Rc, Weak};

pub fn build_statement<'a>(
    ctx: Rc<ParserContext>,
    pair: pest::iterators::Pair<Rule>,
) -> Result<Rc<Statement>, Error<Rule>> {
    let statement_span = Span::from_pest(pair.as_span(), ctx.source());
    let mut iter = pair.into_inner();
    let sub_ctx = ParserContext::derive(ctx, statement_span.clone());
    let mut scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    // Track relationship type BEFORE any verbs run
    let inherited_rel_type = sub_ctx.get_relationship_type();
    // Track inherited default symbol types
    let inherited_default_types = sub_ctx.get_default_symbol_types();

    for pair in iter.by_ref() {
        match pair.as_rule() {
            Rule::verb => {
                build_verb(sub_ctx.clone(), pair)?;
            }
            Rule::scope => {
                // Check if a relationship modifier (@has or @refs) was explicitly used
                // in this statement's verbs
                if !sub_ctx.has_relationship_modifier() {
                    // No relationship modifier in this statement's verbs
                    // Reset to Refs for the scope's children
                    sub_ctx.set_relationship_type_default(RelationshipType::Refs);
                }
                // else: @has/@refs was used, keep the relationship type for children

                scope = build_scope(sub_ctx.clone(), pair)?;

                // Restore this statement's own relationship_type (how it relates to its parent)
                // This is the INHERITED value, not the value after @has/@refs modified it
                sub_ctx.set_relationship_type_default(inherited_rel_type);
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

    // If no explicit type selector was used, add a DefaultTypeFilter.
    // Use inherited default types if available, otherwise default to [FUNCTION].
    if !sub_ctx.has_type_selector() {
        let default_types = inherited_default_types
            .filter(|t| !t.is_empty())
            .unwrap_or_else(|| vec![crate::parser_context::SYMBOL_TYPE_FUNCTION]);
        sub_ctx.extend_verb(DefaultTypeFilter::new(statement_span.clone(), default_types));
    }

    let command = sub_ctx.command(statement_span);
    let relationship_type = sub_ctx.get_relationship_type();
    let statement = Statement::new_with_relationship(command, scope.clone(), relationship_type);
    scope.set_parent(Rc::downgrade(&statement));

    Ok(statement)
}

pub fn build_empty_statement(ctx: Rc<ParserContext>, span: Span) -> Rc<Statement> {
    let scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    let sub_ctx = ParserContext::derive(ctx.clone(), span.clone());
    // Keep the inherited relationship type (Has or Refs).
    // For @has {}, we want to use Has relationship (containment).
    // For {} without @has, the parent context already reset to Refs.
    // The relationship type is correctly set by build_statement before calling build_scope.

    // Empty statements have no explicit type selector — always add DefaultTypeFilter.
    // Use inherited default types if available, otherwise default to [FUNCTION].
    let default_types = sub_ctx
        .get_default_symbol_types()
        .filter(|t| !t.is_empty())
        .unwrap_or_else(|| vec![crate::parser_context::SYMBOL_TYPE_FUNCTION]);
    sub_ctx.extend_verb(DefaultTypeFilter::new(span.clone(), default_types));

    let command = sub_ctx.command(span);
    let relationship_type = sub_ctx.get_relationship_type();
    let statement = Statement::new_with_relationship(command, scope.clone(), relationship_type);
    scope.set_parent(Rc::downgrade(&statement));
    return statement;
}

pub struct ExecutionResult {
    pub nodes: NodeList,
    pub edges: EdgeList,
    pub warnings: Vec<pest::error::Error<Rule>>,
}

impl ExecutionResult {
    pub fn new(
        nodes: NodeList,
        edges: EdgeList,
        warnings: Vec<pest::error::Error<Rule>>,
    ) -> ExecutionResult {
        ExecutionResult {
            nodes,
            edges,
            warnings,
        }
    }
}

#[derive(Debug)]
pub struct Statement {
    pub command: Command,
    pub scope: Rc<dyn Scope>,
    pub parent: RefCell<Option<Weak<Statement>>>,
    pub execution_state: RefCell<ExecutionState>,
    /// The relationship type for this statement's relationship to its parent.
    /// - Refs (default): Reference-based traversal (calls)
    /// - Has: Containment-based traversal (composition)
    pub relationship_type: RelationshipType,
}

impl Statement {
    pub fn new(command: Command, scope: Rc<dyn Scope>) -> Rc<Statement> {
        Statement::new_with_relationship(command, scope, RelationshipType::Refs)
    }

    pub fn new_with_relationship(
        command: Command,
        scope: Rc<dyn Scope>,
        relationship_type: RelationshipType,
    ) -> Rc<Statement> {
        Rc::new(Statement {
            command: command,
            scope: scope,
            parent: RefCell::new(None),
            execution_state: RefCell::new(ExecutionState::new()),
            relationship_type,
        })
    }

    pub fn get_relationship_type(&self) -> RelationshipType {
        self.relationship_type
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
            let warnings = statement.command().compute_selected(ctx, cfg).await;

            statement.get_state_mut().warnings.extend(warnings);
            if !statement.command().has_selectors() {
                statement.get_state_mut().completed = true;
            }
        }
    }

    fn init_dependencies(
        &self,
        labeled_statements_map: &LabeledStatements,
    ) -> Result<(), pest::error::Error<Rule>> {
        crate::scope::visit(self.scope(), &mut |statement| -> Result<
            bool,
            pest::error::Error<Rule>,
        > {
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
    ) -> Result<Vec<Rc<Statement>>, pest::error::Error<Rule>> {
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

    /// Gather all warnings from the statement and its scope. If some warnings
    /// have overlapping spans, include only the innermost ones.
    pub fn gather_warnings(
        &self,
        statements: &Vec<Rc<Statement>>,
    ) -> Vec<pest::error::Error<Rule>> {
        let mut all_warnings = vec![];
        for statement in statements.iter() {
            let state = statement.get_state();
            all_warnings.extend(state.warnings.clone());
        }

        all_warnings.sort_by_key(|e| match e.location {
            pest::error::InputLocation::Pos(pos) => pos,
            pest::error::InputLocation::Span((start, _end)) => start,
        });
        let mut filtered_warnings: Vec<Error<Rule>> = vec![];
        for warning in all_warnings.iter() {
            if let Some(last) = filtered_warnings.last() {
                let last_end_pos = match last.location {
                    pest::error::InputLocation::Pos(pos) => pos,
                    pest::error::InputLocation::Span((_, end)) => end,
                };
                let cur_start_pos = match warning.location {
                    pest::error::InputLocation::Pos(pos) => pos,
                    pest::error::InputLocation::Span((start, _)) => start,
                };
                if last_end_pos >= cur_start_pos {
                    continue;
                }
            }
            filtered_warnings.push(warning.clone());
        }

        filtered_warnings
    }

    pub async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<ExecutionResult, pest::error::Error<Rule>> {
        let statements = self.compute_nodes(ctx, cfg).await?;

        let warnings = self.gather_warnings(&statements);

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
                .map(|node| DeclarationId::new(node.symbol_instance.id)),
        );

        let mut all_references = EdgeList::new();
        // Track seen edges to deduplicate before returning.
        // Key: (from_symbol_id, to_symbol_id, occurrence)
        // This prevents duplicate edges when target symbol has multiple instances
        // (e.g., module with 15 files creates 15 identical-looking edges for one import)
        let mut seen_edges: HashSet<(SymbolId, SymbolId, Option<Occurrence>)> = HashSet::new();

        for statement in &statements {
            let current = if let Some(current) = statement.get_selection(ctx) {
                current
            } else {
                continue;
            };
            for child in &current.children {
                if !all_nodes.contains(&DeclarationId::new(child.from_instance.id))
                    || !all_nodes.contains(&DeclarationId::new(child.symbol_instance.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: FileId::new(child.from_object.id),
                    offset_range: range_bounds_to_offsets(&child.symbol_ref.from_offset_range)
                        .unwrap(),
                };

                // Deduplicate edges that would appear identical in output
                let from_symbol = SymbolId::new(child.parent_symbol.id);
                let to_symbol = SymbolId::new(child.symbol_ref.to_symbol);
                let edge_key = (from_symbol.clone(), to_symbol.clone(), Some(occurrence.clone()));
                if !seen_edges.insert(edge_key) {
                    continue; // Already seen this edge
                }

                all_references.add_reference(
                    SymbolDeclId {
                        symbol_id: from_symbol,
                        declaration_id: DeclarationId::new(child.from_instance.id),
                    },
                    SymbolDeclId {
                        symbol_id: to_symbol,
                        declaration_id: DeclarationId::new(child.symbol_instance.id),
                    },
                    Some(occurrence),
                );
            }

            for parent in &current.parents {
                if !all_nodes.contains(&DeclarationId::new(parent.from_instance.id))
                    || !all_nodes.contains(&DeclarationId::new(parent.to_instance.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: parent.from_instance.object_id.into(),
                    offset_range: range_bounds_to_offsets(&parent.symbol_ref.from_offset_range)
                        .unwrap(),
                };

                // Deduplicate edges that would appear identical in output
                let from_symbol = SymbolId::new(parent.from_instance.symbol);
                let to_symbol = SymbolId::new(parent.to_symbol.id);
                let edge_key = (from_symbol.clone(), to_symbol.clone(), Some(occurrence.clone()));
                if !seen_edges.insert(edge_key) {
                    continue; // Already seen this edge
                }

                all_references.add_reference(
                    SymbolDeclId {
                        symbol_id: from_symbol,
                        declaration_id: DeclarationId::new(parent.from_instance.id),
                    },
                    SymbolDeclId {
                        symbol_id: to_symbol,
                        declaration_id: DeclarationId::new(parent.to_instance.id),
                    },
                    Some(occurrence),
                );
            }
        }

        return Ok(ExecutionResult::new(
            NodeList(complete_selection.into_iter().collect()),
            all_references,
            warnings,
        ));
    }

    /// Notify the dependent statement's execution state about change in the
    /// state of a dependency.
    ///
    /// When a child notifies its parent (role=Parent), we defer the constraint
    /// until ALL children have resolved. This prevents over-constraining the
    /// parent when multiple sibling children exist (e.g., `@has { @directory ; @file }`).
    /// The parent is constrained against the **union** of all children's selections,
    /// so it retains nodes that match ANY child.
    pub async fn notify(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        dependent: &StatementDependent,
    ) -> Result<(), pest::error::Error<Rule>> {
        let _update_dependency: tracing::span::EnteredSpan =
            tracing::info_span!("notify").entered();

        if dependent.dependency_role == DependencyRole::Parent {
            // Child notifying parent — defer until all children have selections.
            let all_children_resolved = dependent
                .statement
                .children()
                .all(|child| child.is_selection_some(ctx));
            if !all_children_resolved {
                return Ok(());
            }

            // Merge all children's selections into one (union).
            let mut merged = Selection::new();
            let mut any_has_selection = false;
            for child in dependent.statement.children() {
                if !child.command().has_selectors() {
                    continue;
                }
                if child.get_state().weak {
                    continue;
                }
                if let Some(sel) = child.get_selection(ctx) {
                    merged.extend(sel.clone());
                    any_has_selection = true;
                }
            }

            if !any_has_selection {
                return Ok(());
            }

            // Use the notifying child's relationship type (all siblings share it).
            let rel_type = self.get_relationship_type();

            let res = dependent
                .statement
                .command()
                .notify_from_selection(
                    ctx,
                    &cfg.index,
                    &merged,
                    DependencyRole::Parent,
                    rel_type,
                )
                .await?;

            if res.changed {
                dependent.statement.get_state_mut().completed = false;
            }
            dependent
                .statement
                .get_state_mut()
                .warnings
                .extend(res.warnings);
            return Ok(());
        }

        // Original flow for Child and User roles.
        let receiver_rel_type = dependent.statement.get_relationship_type();
        let res = dependent
            .statement
            .command()
            .accept_notification(ctx, &cfg.index, self, dependent.dependency_role, receiver_rel_type)
            .await?;

        if res.changed {
            dependent.statement.get_state_mut().completed = false;
        }
        dependent
            .statement
            .get_state_mut()
            .warnings
            .extend(res.warnings);
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
) -> Result<(), pest::error::Error<Rule>> {
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
                return Err(Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!("Label '{}' not found for user selector", label),
                    },
                    user.span(),
                ));
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
