use crate::cfg::{ControlFlowGraph, EdgeList, HasEdge, HasEdgeList, NodeList, SymbolNodeId};
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
use crate::verb::{build_verb, DefaultTypeFilter, NotificationContext, VerbTag};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{CompositeFilter, ScopeContext, Selection};
use index::symbols::{SymbolInstanceId, FileId, Occurrence, SymbolId};
use std::collections::HashMap;
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
                if !sub_ctx.has_relationship_modifier() {
                    // No explicit has/refs — default to both so parent-child
                    // works regardless of whether the edge is containment or reference.
                    sub_ctx.set_relationship_type_default(RelationshipType::REFS | RelationshipType::HAS);
                }

                // Allow all symbol types for children — empty vec means no type filtering.
                sub_ctx.set_default_symbol_types(vec![]);

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

    // Restore this statement's own relationship_type (how it relates to its parent).
    // This is the INHERITED value, not the value after verbs (like has/func) modified it.
    // The verb modifications only affect children (via the scope built above).
    sub_ctx.set_relationship_type_default(inherited_rel_type);

    if let Some(pair) = iter.next() {
        return Err(Error::new_from_span(
            pest::error::ErrorVariant::CustomError {
                message: format!("Unexpected token after scope: {}", pair),
            },
            pair.as_span(),
        ));
    }

    // If no explicit type selector was used, add a DefaultTypeFilter.
    // None → no inherited default, all types (no filtering needed, skip verb).
    // Some(vec![]) → explicitly set to "all types" (no filtering needed, skip verb).
    // Some(types) → filter by those types.
    if !sub_ctx.has_type_selector() {
        let default_types = inherited_default_types.unwrap_or_default();
        if !default_types.is_empty() {
            sub_ctx.extend_verb(DefaultTypeFilter::new(statement_span.clone(), default_types));
        }
    }

    let command = sub_ctx.command(statement_span);
    let relationship_type = sub_ctx.get_relationship_type();
    let unnest = command.has_verb_tag(&VerbTag::Unnest);
    let statement = Statement::new_full(command, scope.clone(), relationship_type, unnest);
    scope.set_parent(Rc::downgrade(&statement));

    Ok(statement)
}

pub fn build_empty_statement(ctx: Rc<ParserContext>, span: Span) -> Rc<Statement> {
    let scope: Rc<dyn Scope> = Rc::new(EmptyScope::new());
    let sub_ctx = ParserContext::derive(ctx.clone(), span.clone());
    // Keep the inherited relationship type (Has or Refs).
    // For has {}, we want to use Has relationship (containment).
    // For {} without has, the parent context already reset to Refs.
    // The relationship type is correctly set by build_statement before calling build_scope.

    // Empty statements have no explicit type selector — add DefaultTypeFilter if needed.
    // None → all types (no filtering needed, skip verb). Some(vec![]) → same, skip.
    let default_types = sub_ctx.get_default_symbol_types().unwrap_or_default();
    if !default_types.is_empty() {
        sub_ctx.extend_verb(DefaultTypeFilter::new(span.clone(), default_types));
    }

    let command = sub_ctx.command(span);
    let relationship_type = sub_ctx.get_relationship_type();
    let statement = Statement::new_with_relationship(command, scope.clone(), relationship_type);
    scope.set_parent(Rc::downgrade(&statement));
    return statement;
}

pub struct ExecutionResult {
    pub nodes: NodeList,
    pub edges: EdgeList,
    pub has_edges: HasEdgeList,
    pub warnings: Vec<pest::error::Error<Rule>>,
}

impl ExecutionResult {
    pub fn new(
        nodes: NodeList,
        edges: EdgeList,
        has_edges: HasEdgeList,
        warnings: Vec<pest::error::Error<Rule>>,
    ) -> ExecutionResult {
        ExecutionResult {
            nodes,
            edges,
            has_edges,
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
    /// Whether this statement uses unnest mode for scope derivation.
    /// When false (default), derive_from_parent filters to direct children only
    /// and upward HAS derivation returns only innermost parents.
    /// When true (unnest verb), all transitive levels are included.
    pub unnest: bool,
}

impl Statement {
    pub fn new(command: Command, scope: Rc<dyn Scope>) -> Rc<Statement> {
        Statement::new_full(command, scope, RelationshipType::REFS, false)
    }

    pub fn new_with_relationship(
        command: Command,
        scope: Rc<dyn Scope>,
        relationship_type: RelationshipType,
    ) -> Rc<Statement> {
        Statement::new_full(command, scope, relationship_type, false)
    }

    pub fn new_full(
        command: Command,
        scope: Rc<dyn Scope>,
        relationship_type: RelationshipType,
        unnest: bool,
    ) -> Rc<Statement> {
        Rc::new(Statement {
            command,
            scope,
            parent: RefCell::new(None),
            execution_state: RefCell::new(ExecutionState::new()),
            relationship_type,
            unnest,
        })
    }

    pub fn is_unnest(&self) -> bool {
        self.unnest
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
                    // Selector has no selection (not yet resolved, filter-only, etc.).
                    // Reset accumulation — a later selector with Some can still overwrite.
                    // This ensures that e.g. [NameSel=Some, UserVerb=None] → None
                    // (statement not ready until UserVerb resolves), while
                    // [TypeSel(filter_only)=None, NameSel=Some] → Some (filter-only
                    // is overwritten by the real selector that follows).
                    selection = None;
                }
            });
        selection
    }

    /// Check if this statement's selectors have been computed (added to registry).
    fn is_computed(&self, ctx: &ExecutionContext) -> bool {
        self.command().selectors().all(|s| ctx.registry.contains(&s.id()))
    }

    fn is_selection_some(&self, ctx: &ExecutionContext) -> bool {
        let mut has_selector = false;
        let mut has_any_selection = false;
        ctx.registry
            .for_each_selector(self.command().selectors(), |selector, sel_state| {
                has_selector = true;
                if selector.get_selection(sel_state).is_some() {
                    has_any_selection = true;
                }
            });
        !has_selector || has_any_selection
    }

    // Statements that have dependencies resolved and are ready to execute
    async fn compute_selectors(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        statements: &Vec<Rc<Statement>>,
    ) -> Result<(), pest::error::Error<Rule>> {
        let _select_nodes: tracing::span::EnteredSpan =
            tracing::info_span!("select_nodes").entered();

        for statement in statements.iter() {
            ctx.current_statement_span = Some(statement.command().span().clone());
            let parent_scope = build_parent_scope(statement, ctx);
            let children_scope = build_children_scope(statement, ctx);
            let warnings = statement.command().compute_selected(ctx, cfg, parent_scope, children_scope).await?;

            statement.get_state_mut().warnings.extend(warnings);
            if !statement.command().has_selectors() {
                statement.get_state_mut().completed = true;
            }
        }
        Ok(())
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

                let is_non_constraining = statement.command().is_non_constraining();

                if !is_non_constraining {
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
        self.compute_selectors(ctx, cfg, &statements).await?;

        self.init_dependencies(&labeled_statements)?;

        self.mark_weak_statements(&statements);

        while !statements.iter().all(|s| s.get_state().completed) {
            // Yield to the runtime so tokio::time::timeout can fire if the
            // query has exceeded its deadline.
            tokio::task::yield_now().await;

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

    /// Collect reference edges between all selected symbols.
    ///
    /// Three sources of edges:
    /// 1. **Forced edges**: synthetic edges from ForcedVerb / UserVerb(forced=true),
    ///    identified by symbol_ref.id == 0. Not in DB, must be preserved.
    /// 2. **Explicit edges**: from scoped parents/children in Selection.
    /// 3. **Implicit edges**: discovered via DB query between all selected instances.
    ///
    /// Deduplication key: (from_instance_id, to_instance_id, symbol_ref_id).
    async fn collect_ref_edges(
        statements: &[Rc<Statement>],
        ctx: &ExecutionContext,
        all_nodes: &HashSet<SymbolInstanceId>,
        index: &index::db_diesel::Index,
    ) -> EdgeList {
        let mut all_references = EdgeList::new();
        // Dedup key: (from_instance_id, to_symbol_id, occurrence)
        // Instance-level on from side prevents suppressing edges between different
        // source instances of the same symbol. Symbol-level on to side deduplicates
        // when the target symbol has multiple selected instances (e.g., definition
        // + declaration of f), since a single reference should produce one edge.
        let mut seen_edges: HashSet<(SymbolInstanceId, SymbolId, Option<Occurrence>)> = HashSet::new();

        // 1. Collect forced edges (symbol_ref.id == 0) from Selection.parents
        for statement in statements {
            let current = if let Some(current) = statement.get_selection(ctx) {
                current
            } else {
                continue;
            };
            for parent in &current.parents {
                if parent.symbol_ref.id != 0 {
                    continue; // Not a forced edge
                }
                if !all_nodes.contains(&SymbolInstanceId::new(parent.from_instance.id))
                    || !all_nodes.contains(&SymbolInstanceId::new(parent.to_instance.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: parent.from_instance.object_id.into(),
                    offset_range: range_bounds_to_offsets(&parent.symbol_ref.from_offset_range)
                        .unwrap(),
                };

                let from_inst = SymbolInstanceId::new(parent.from_instance.id);
                let to_inst = SymbolInstanceId::new(parent.to_instance.id);
                let to_sym = SymbolId::new(parent.to_symbol.id);
                let edge_key = (from_inst, to_sym.clone(), Some(occurrence.clone()));
                if !seen_edges.insert(edge_key) {
                    continue;
                }

                all_references.add_reference(
                    SymbolNodeId {
                        symbol_id: SymbolId::new(parent.from_instance.symbol),
                        instance_id: from_inst,
                    },
                    SymbolNodeId {
                        symbol_id: to_sym,
                        instance_id: to_inst,
                    },
                    Some(occurrence),
                );
            }
        }

        // 2. Collect explicit edges from Selection.parents/children (non-forced)
        for statement in statements {
            let current = if let Some(current) = statement.get_selection(ctx) {
                current
            } else {
                continue;
            };
            for child in &current.children {
                if !all_nodes.contains(&SymbolInstanceId::new(child.from_instance.id))
                    || !all_nodes.contains(&SymbolInstanceId::new(child.symbol_instance.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: FileId::new(child.from_object.id),
                    offset_range: range_bounds_to_offsets(&child.symbol_ref.from_offset_range)
                        .unwrap(),
                };

                let from_inst = SymbolInstanceId::new(child.from_instance.id);
                let to_inst = SymbolInstanceId::new(child.symbol_instance.id);
                let to_sym = SymbolId::new(child.symbol_ref.to_symbol);
                let edge_key = (from_inst, to_sym.clone(), Some(occurrence.clone()));
                if !seen_edges.insert(edge_key) {
                    continue;
                }

                all_references.add_reference(
                    SymbolNodeId {
                        symbol_id: SymbolId::new(child.parent_symbol.id),
                        instance_id: from_inst,
                    },
                    SymbolNodeId {
                        symbol_id: to_sym,
                        instance_id: to_inst,
                    },
                    Some(occurrence),
                );
            }

            for parent in &current.parents {
                if parent.symbol_ref.id == 0 {
                    continue; // Already handled as forced edge
                }
                if !all_nodes.contains(&SymbolInstanceId::new(parent.from_instance.id))
                    || !all_nodes.contains(&SymbolInstanceId::new(parent.to_instance.id))
                {
                    continue;
                }

                let occurrence = Occurrence {
                    file: parent.from_instance.object_id.into(),
                    offset_range: range_bounds_to_offsets(&parent.symbol_ref.from_offset_range)
                        .unwrap(),
                };

                let from_inst = SymbolInstanceId::new(parent.from_instance.id);
                let to_inst = SymbolInstanceId::new(parent.to_instance.id);
                let to_sym = SymbolId::new(parent.to_symbol.id);
                let edge_key = (from_inst, to_sym.clone(), Some(occurrence.clone()));
                if !seen_edges.insert(edge_key) {
                    continue;
                }

                all_references.add_reference(
                    SymbolNodeId {
                        symbol_id: SymbolId::new(parent.from_instance.symbol),
                        instance_id: from_inst,
                    },
                    SymbolNodeId {
                        symbol_id: to_sym,
                        instance_id: to_inst,
                    },
                    Some(occurrence),
                );
            }
        }

        // 3. Discover implicit edges via DB query
        // Build instance→(symbol_id, instance_id) lookup for mapping DB results to nodes.
        let mut instance_to_node: HashMap<i32, (SymbolId, SymbolInstanceId)> = HashMap::new();
        for statement in statements {
            if let Some(sel) = statement.get_selection(ctx) {
                for node in &sel.nodes {
                    let inst_id = SymbolInstanceId::new(node.symbol_instance.id);
                    if all_nodes.contains(&inst_id) {
                        instance_to_node.insert(
                            node.symbol_instance.id,
                            (SymbolId::new(node.symbol.id), inst_id),
                        );
                    }
                }
            }
        }

        let all_ids: Vec<i32> = all_nodes.iter().map(|id| Into::<i32>::into(*id)).collect();
        if let Ok(implicit_edges) = index.find_edges_between(&all_ids).await {
            for edge in implicit_edges {
                let from_node = instance_to_node.get(&edge.from_instance_id);
                let to_node = instance_to_node.get(&edge.to_instance_id);
                if let (Some((from_sym, from_inst)), Some((_to_sym, to_inst))) = (from_node, to_node) {
                    let occurrence = Occurrence {
                        file: FileId::new(edge.from_object),
                        offset_range: range_bounds_to_offsets(&edge.from_offset_range)
                            .unwrap(),
                    };

                    let to_symbol = SymbolId::new(edge.to_symbol);
                    let edge_key = (*from_inst, to_symbol.clone(), Some(occurrence.clone()));
                    if !seen_edges.insert(edge_key) {
                        continue; // Already seen from explicit/forced
                    }

                    all_references.add_reference(
                        SymbolNodeId {
                            symbol_id: from_sym.clone(),
                            instance_id: *from_inst,
                        },
                        SymbolNodeId {
                            symbol_id: to_symbol,
                            instance_id: *to_inst,
                        },
                        Some(occurrence),
                    );
                }
            }
        }

        all_references
    }

    fn collect_has_edges(
        statements: &[Rc<Statement>],
        ctx: &ExecutionContext,
        all_nodes: &HashSet<SymbolInstanceId>,
    ) -> HasEdgeList {
        // For each child instance, track only the best (tightest) parent.
        // Key: child instance ID, Value: (HasEdge, parent_span) where smaller span = tighter container.
        let mut best_per_child: HashMap<SymbolInstanceId, (HasEdge, i64)> = HashMap::new();

        for statement in statements {
            let current = if let Some(current) = statement.get_selection(ctx) {
                current
            } else {
                continue;
            };

            // Both has_children and has_parents express the same relationship
            // (parent contains child) but from different traversal directions.
            // Unify them into a single iterator of (parent_instance, child_instance,
            // parent_symbol_id, child_symbol_id).
            let from_children = current.has_children.iter().map(|h| {
                (&h.parent_instance, &h.child_instance, h.parent_symbol.id, h.child_symbol.id)
            });
            let from_parents = current.has_parents.iter().map(|h| {
                (&h.parent_instance, &h.child_instance, h.parent_symbol.id, h.child_symbol.id)
            });

            for (parent_inst, child_inst, parent_sym, child_sym) in from_children.chain(from_parents) {
                let parent_id = SymbolInstanceId::new(parent_inst.id);
                let child_id = SymbolInstanceId::new(child_inst.id);

                if !all_nodes.contains(&parent_id) || !all_nodes.contains(&child_id) {
                    continue;
                }

                let parent_span = range_bounds_to_offsets(&parent_inst.offset_range)
                    .map(|(s, e)| (e - s) as i64)
                    .unwrap_or(i64::MAX);

                let edge = HasEdge {
                    parent: SymbolId::new(parent_sym),
                    child: SymbolId::new(child_sym),
                    parent_instance: parent_id,
                    child_instance: child_id,
                };

                best_per_child
                    .entry(child_id)
                    .and_modify(|(existing_edge, existing_span)| {
                        if parent_span < *existing_span {
                            *existing_edge = edge.clone();
                            *existing_span = parent_span;
                        }
                    })
                    .or_insert((edge, parent_span));
            }
        }

        let mut result = HasEdgeList::new();
        for (_, (edge, _)) in best_per_child {
            result.add(edge);
        }

        result
    }

    pub async fn execute(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
    ) -> Result<ExecutionResult, pest::error::Error<Rule>> {
        let statements = self.compute_nodes(ctx, cfg).await?;

        let warnings = self.gather_warnings(&statements);

        let mut complete_selection = Vec::new();
        for statement in &statements {
            complete_selection.extend(
                statement
                    .get_selection(ctx)
                    .iter()
                    .map(|s| s.nodes.clone())
                    .flatten(),
            );
        }

        let all_nodes = HashSet::<SymbolInstanceId>::from_iter(
            complete_selection
                .iter()
                .map(|node| SymbolInstanceId::new(node.symbol_instance.id)),
        );

        let ref_edges = Self::collect_ref_edges(&statements, ctx, &all_nodes, &cfg.index).await;
        let has_edges = Self::collect_has_edges(&statements, ctx, &all_nodes);

        Ok(ExecutionResult::new(
            NodeList(complete_selection.into_iter().collect()),
            ref_edges,
            has_edges,
            warnings,
        ))
    }

    /// Notify the dependent statement's execution state about change in the
    /// state of a dependency.
    ///
    /// When a child notifies its parent (role=Parent), we defer the constraint
    /// until ALL children have resolved. This prevents over-constraining the
    /// parent when multiple sibling children exist (e.g., `has { dir ; file }`).
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
                if should_skip_in_parent_merge(&child) {
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

            let unnest = dependent.statement.is_unnest();
            let parent_scope = build_parent_scope(&dependent.statement, ctx);
            let children_scope = ScopeContext::Scope {
                ids: merged.get_instance_ids(),
                filter: None,
            };
            let res = dependent
                .statement
                .command()
                .notify_from_selection(
                    ctx,
                    &cfg.index,
                    &merged,
                    DependencyRole::Parent,
                    rel_type,
                    unnest,
                    parent_scope,
                    children_scope,
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
        // Resolve rel_type at the single call site instead of duplicating
        // the role-based match in each accept_notification implementation.
        let rel_type = match dependent.dependency_role {
            DependencyRole::Child => dependent.statement.get_relationship_type(),
            DependencyRole::Parent | DependencyRole::User => self.get_relationship_type(),
        };
        let notif_ctx = NotificationContext {
            role: dependent.dependency_role,
            rel_type,
            unnest: dependent.statement.is_unnest(),
        };
        let parent_scope = build_parent_scope(&dependent.statement, ctx);
        let children_scope = build_children_scope(&dependent.statement, ctx);
        let res = dependent
            .statement
            .command()
            .accept_notification(ctx, &cfg.index, self, notif_ctx, parent_scope, children_scope)
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

/// Whether a child should be excluded from the bottom-up parent merge.
///
/// A bare `{}` (weak UnitVerb) can acquire a selection in two ways:
///
/// 1. **Top-down echo** — a strong ancestor above derived data downward
///    through weak intermediaries.  Including this in the parent merge would
///    feed the parent's own data back to it, diluting constraints.
///
/// 2. **Bottom-up signal** — a non-weak descendant (e.g. a NameSelector)
///    originated data that propagated upward through weak intermediaries.
///    This is real constraining data that the parent needs.
///
/// We distinguish the two structurally: if every descendant of the child is
/// weak, no node below could have originated data, so any selection is
/// necessarily a top-down echo (case 1) — skip it.  If a non-weak descendant
/// exists, real data could have flowed up (case 2) — include it.
///
/// A direct-children check (`child.children().all(weak)`) is insufficient
/// because `mark_weak_statements` propagates weakness downward via the
/// `parent_weak` rule: a statement can be weak (from its parent) while having
/// a non-weak child of its own.  So a weak grandchild may still carry data
/// from a non-weak great-grandchild.  We therefore recurse the full subtree.
fn should_skip_in_parent_merge(child: &Statement) -> bool {
    child.get_state().weak && child.command().is_unit() && all_descendants_weak(child)
}

fn all_descendants_weak(stmt: &Statement) -> bool {
    stmt.children()
        .all(|child| child.get_state().weak && all_descendants_weak(&child))
}

/// Build scope context for the parent side of a statement's parent query.
/// If the parent already has a selection, use its instance IDs.
/// If no parent exists, return Skip.
/// If the parent hasn't been selected yet, fall back to mixin-based scoping.
fn build_parent_scope(statement: &Statement, ctx: &ExecutionContext) -> ScopeContext {
    match statement.parent().and_then(|p| p.upgrade()) {
        Some(parent) => {
            if parent.is_computed(ctx) {
                match parent.get_selection(ctx) {
                    Some(sel) => ScopeContext::Scope { ids: sel.get_instance_ids(), filter: None },
                    // None = parent has no opinion (filter-only, unit, or no selectors).
                    // Run unscoped — the parent is transparent.
                    None => ScopeContext::Unscoped,
                }
            } else {
                // Parent not yet computed — fall back to filter-based scoping
                match parent.command().get_selector_composite_filter() {
                    Some(f) => ScopeContext::Scope { ids: vec![], filter: Some(f) },
                    None => ScopeContext::Unscoped,
                }
            }
        },
        None => ScopeContext::Unscoped, // Root-level: run parents unscoped
    }
}

/// Build scope context for the children side of a statement's children query.
/// Collects instance IDs from already-selected children + filters from unselected children.
/// If no children exist, return Skip.
fn build_children_scope(statement: &Statement, ctx: &ExecutionContext) -> ScopeContext {
    let mut has_children = false;
    let mut any_uncomputed = false;
    let mut any_transparent = false;
    let mut selected_ids: Vec<i32> = Vec::new();
    let mut unselected_filters: Vec<CompositeFilter> = Vec::new();

    for child in statement.children() {
        has_children = true;
        if child.is_computed(ctx) {
            match child.get_selection(ctx) {
                Some(sel) => selected_ids.extend(sel.get_instance_ids()),
                None => any_transparent = true,
            }
        } else {
            any_uncomputed = true;
            if let Some(f) = child.command().get_selector_composite_filter() {
                unselected_filters.push(f);
            }
        }
    }

    if !has_children {
        return ScopeContext::Skip;
    }

    let combined_filter = if unselected_filters.is_empty() {
        None
    } else {
        Some(CompositeFilter::or(unselected_filters))
    };

    if selected_ids.is_empty() && combined_filter.is_none() {
        if any_uncomputed || any_transparent {
            ScopeContext::Unscoped
        } else {
            ScopeContext::Skip
        }
    } else {
        ScopeContext::Scope { ids: selected_ids, filter: combined_filter }
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
