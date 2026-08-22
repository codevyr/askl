use crate::cfg::{ControlFlowGraph, EdgeList, HasEdge, HasEdgeList, NodeList, SymbolNodeId};
use crate::command::{Command, ComputeResult, LabeledStatements};
use crate::diagnostic::{Diagnostic, DiagnosticKind};
use crate::execution_context::{statement_key, ExecutionContext, ProbeActivation};
use crate::execution_state::{
    DependencyRole, ExecutionState, RelationshipType, StatementDependency, StatementDependent,
};
use crate::hierarchy::Hierarchy;
use crate::name_pattern::NamePattern;
use crate::offset_range::range_bounds_to_offsets;
use crate::parser::Rule;
use crate::scope::{Scope, StatementIter};
use crate::verb::{name_filter, LabelResolutions, NotificationContext};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{CompositeFilter, EphContext, ScopeContext, Selection};
use index::symbols::{FileId, Occurrence, SymbolId, SymbolInstanceId};
use std::cell::{Ref, RefCell, RefMut};
use std::collections::HashMap;
use std::collections::HashSet;
use std::future::Future;
use std::pin::Pin;
use std::rc::{Rc, Weak};

mod anchor;
mod parse;
mod scope_helpers;

pub use self::parse::{build_dependency_graph, build_empty_statement, build_statement};

use self::scope_helpers::{build_children_scope, build_parent_scope, should_skip_in_parent_merge};

pub struct ExecutionResult {
    pub nodes: NodeList,
    pub edges: EdgeList,
    pub has_edges: HasEdgeList,
    pub warnings: Vec<Diagnostic>,
}

impl ExecutionResult {
    pub fn new(
        nodes: NodeList,
        edges: EdgeList,
        has_edges: HasEdgeList,
        warnings: Vec<Diagnostic>,
    ) -> ExecutionResult {
        ExecutionResult {
            nodes,
            edges,
            has_edges,
            warnings,
        }
    }
}

pub struct PropagationResult {
    pub changed: bool,
}

/// Statement-level enrichment: for each `NoMatch` warning that carries a typed
/// name, decide whether it's a real typo (offer "did you mean?" suggestions) or a
/// name that exists somewhere but was excluded by a filter/scope. Done here,
/// once, after all statements have executed — the index and visible projects are
/// available, and it keeps the DB side-effect out of the selector's compute.
///
/// The check is deliberately **name-only**: "does this name exist *anywhere
/// visible*, ignoring type/project/relationship filters?" We build just the name
/// predicate (`name_filter`, so its case/leaf/compound routing mirrors the
/// selector) and probe with `has_symbol_matching`. If it exists, the empty
/// selection is due to some other filter (not spelling), so we set `name_exists`
/// (the renderer phrases it and points at type/project/relationship) instead of
/// suggesting. Only when the name exists nowhere do we offer fuzzy suggestions.
async fn enrich_no_match_suggestions(
    warnings: &mut [Diagnostic],
    ctx: &ExecutionContext,
    cfg: &ControlFlowGraph,
) {
    const MAX_SUGGESTIONS: usize = 5;
    let project_ids: Vec<i32> = ctx.eph.roots().iter().map(|r| r.project_id).collect();
    if project_ids.is_empty() {
        return;
    }
    let visible_ids = ctx.eph.visible_ids();
    for w in warnings.iter_mut() {
        if w.kind != DiagnosticKind::NoMatch {
            continue;
        }
        let Some(token) = w.token.clone() else {
            continue;
        };

        // Name-only existence: does the name exist anywhere visible, ignoring
        // the type/project/relationship filters? Reuse the selector's own name
        // predicate so case/leaf/compound routing matches exactly; the lean
        // `has_symbol_matching` probe is current-query-only + LIMIT 1.
        let filter = name_filter(&NamePattern::Exact(token.clone()));
        let exists = match cfg.index.has_symbol_matching(&filter, &ctx.eph).await {
            Ok(exists) => exists,
            Err(e) => {
                tracing::debug!("existence check for {:?} failed: {}", token, e);
                continue;
            }
        };

        if exists {
            // The name is real; an excluding filter (type/project/relationship)
            // emptied the selection. Structured flag → the renderer phrases it.
            w.name_exists = true;
        } else {
            match cfg
                .index
                .suggest_symbol_names(&token, &project_ids, &visible_ids, MAX_SUGGESTIONS)
                .await
            {
                Ok(names) => w.suggestions = names,
                Err(e) => tracing::debug!("name suggestions for {:?} failed: {}", token, e),
            }
        }
    }
}

/// Budget gate + scope construction for a statement's READ phase (Phase R).
///
/// Budget-pushdown safety gate: only a statement whose selection no one else
/// consumes may bound its neighbourhood leaves.  Any Parent/Child/User
/// dependency or dependent means another statement's `constrain_*` reads this
/// selection's nodes/parents/children, and a PreSeedLabel DEPENDENT means
/// `compute_roots` reads the selection for `@label` resolution (a truncated,
/// order-arbitrary id set would silently seed the layer AND destabilise its
/// input hash) — so both clear the budget.  Only PreSeedSibling is pure
/// ordering with no data flow.
fn stage_read(
    ctx: &ExecutionContext,
    statement: &Rc<Statement>,
) -> (EphContext, ScopeContext, ScopeContext) {
    let mut eph = ctx.eph.clone();
    {
        let st = statement.get_state();
        let composed = st
            .dependencies
            .iter()
            .any(|d| !d.dependency_role.is_pre_seed())
            || st
                .dependents
                .iter()
                .any(|d| !matches!(d.dependency_role, DependencyRole::PreSeedSibling));
        if composed {
            eph.set_result_budget(index::db_diesel::ResultBudget::UNLIMITED);
        }
    }
    let parent_scope = build_parent_scope(statement, ctx, &eph);
    let children_scope = build_children_scope(statement, ctx, &eph);
    (eph, parent_scope, children_scope)
}

/// Phase-M handoff to the Phase-R batch: one substatement's
/// materialised layer ids, its `(root, layer)` entries (pushed by
/// the executor as part of the TREE's single materialisation), and Phase-M
/// warnings.
struct Staged {
    statement: Rc<Statement>,
    layer_ids: Vec<i64>,
    layers: Vec<(i64, i64)>,
    warnings: Vec<Diagnostic>,
}

/// The sequential prep half of one substatement's Phase M: everything
/// that reads `ExecutionContext` — `@label` resolution and parent-scope
/// construction — done up front so the materialise fan-out itself needs
/// no `ctx` at all (the concurrent futures borrow only `cfg`, the
/// pre-tree snapshot, and this struct).
struct PreparedMaterialise {
    statement: Rc<Statement>,
    resolved: LabelResolutions,
    parent_scope: ScopeContext,
}

/// Prepare one substatement of Phase M: `@label` resolution and scope
/// construction, both cheap immutable reads of `ctx`.
///
/// The parse-time ordering rule guarantees every label dep lives in an
/// EARLIER tree, fully run (M, P, R) before this tree started — so
/// resolution just reads the dependency's completed selection; nothing
/// a tree-mate's materialisation could change.  That independence is
/// what lets the executor prepare ALL of a tree's substatements before
/// fanning their materialisations out concurrently.
fn prepare_materialise(
    ctx: &ExecutionContext,
    statement: &Rc<Statement>,
    pre_tree_eph: &EphContext,
) -> PreparedMaterialise {
    let mut resolved = LabelResolutions::new();
    let label_deps: Vec<(Rc<Statement>, Rc<str>)> = statement
        .get_state()
        .dependencies
        .iter()
        .filter_map(|d| match &d.dependency_role {
            DependencyRole::PreSeedLabel(label) => Some((d.dependency.clone(), label.clone())),
            _ => None,
        })
        .collect();
    for (dep_stmt, label) in label_deps {
        if let Some(sel) = dep_stmt.get_selection(ctx) {
            let ids = canonical_ids(sel.nodes.iter().map(|n| n.symbol.id));
            if ids.is_empty() {
                tracing::warn!(
                    label = %label,
                    "ephemeral verb '@{}' resolved to 0 symbols; \
                     layer will emit no rows",
                    label,
                );
            }
            resolved.insert(label, ids);
        } else {
            tracing::warn!(
                label = %label,
                "ephemeral verb '@{}' labelled statement has no \
                 selection at resolution time; layer will emit no rows",
                label,
            );
        }
    }

    // Condition-based by construction: nothing in this tree has a
    // selection yet, so the fused scope is the parent's filter.
    let parent_scope = build_parent_scope(statement, ctx, pre_tree_eph);
    PreparedMaterialise {
        statement: statement.clone(),
        resolved,
        parent_scope,
    }
}

/// True when a statement may re-probe in a refinement round: every
/// selector is either fusable (its emission is the shared statement
/// query) or non-constraining (unit verbs, bare filter-only type
/// selectors — pure derive-path statements).  Layer-backed, forced, and
/// label statements have their own read semantics and never refine.
fn refinable(command: &Command) -> bool {
    command
        .selectors()
        .all(|s| s.fusable() || s.is_non_constraining_selector())
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

    /// Count total selected nodes across all selectors, without cloning.
    /// Returns None if no selector has a selection.
    fn selection_node_count(&self, ctx: &ExecutionContext) -> Option<usize> {
        let mut total: Option<usize> = None;
        ctx.registry
            .for_each_selector(self.command().selectors(), |selector, state| {
                if let Some(sel) = selector.get_selection(state) {
                    *total.get_or_insert(0) += sel.nodes.len();
                } else {
                    total = None;
                }
            });
        total
    }

    pub fn has_selection(&self, ctx: &ExecutionContext) -> bool {
        self.selection_node_count(ctx).is_some()
    }

    pub fn propagation_priority(&self, ctx: &ExecutionContext) -> usize {
        self.selection_node_count(ctx).unwrap_or(usize::MAX)
    }

    /// Check if this statement's selectors have been computed (added to registry).
    fn is_computed(&self, ctx: &ExecutionContext) -> bool {
        self.command()
            .selectors()
            .all(|s| ctx.registry.contains(&s.id()))
    }

    /// Compute initial selections for every statement — PHASED (M, P, R),
    /// per top-level statement tree, trees in source order.  ONE
    /// materialisation per tree: nesting no longer sequences; `;` is the only
    /// time axis.
    ///
    /// - **Phase M (materialise, concurrent):** runs per tree — a
    ///   visibility snapshot is taken ONCE before any of the tree's
    ///   materialisations, every substatement is prepared sequentially
    ///   ([`prepare_materialise`]: label resolution + scopes, the `ctx`
    ///   reads), and the [`Command::materialise_layers`] calls then run
    ///   CONCURRENTLY against that same PRE-TREE snapshot: no command sees
    ///   a tree-mate's layers at materialise time (intra-tree chaining
    ///   does not exist), so the calls are independent by construction.
    ///   Outcomes are applied in substatement pre-order — completion order
    ///   is unobservable.  The tree's layers then enter visibility as ONE
    ///   materialisation, pushed after the batch; each command keeps its own
    ///   layers
    ///   (attribution by layer id is load-bearing for per-command
    ///   selections).  Tree-mates with IDENTICAL specs converge on the
    ///   same layer rows via the ON CONFLICT + 2-PC `populated` protocol
    ///   (see `Index::create_eph_layer`).
    ///
    ///   **Spine advance rule.**  Every selection-shard-bearing command's
    ///   selection shard parents on the PREVIOUS tree's tip (`tip` of the
    ///   pre-tree snapshot); a tree with ≥2 selection shards makes them
    ///   SIBLINGS — no intra-tree key chaining.  The tree's new tip is, per
    ///   root, the LAST layer of its materialisation in substatement
    ///   pre-order: the final layer-bearing substatement's selection shard,
    ///   or — under an empty pre-tree chain, where NO command materialises a
    ///   selection shard (first-materialisation elision, per command) — that
    ///   substatement's root shard.  This is deterministic: pre-order is
    ///   source order and each command's internal layer order is canonical
    ///   (roots ascending; root shard → layer shards → selection shard,
    ///   sorted by
    ///   [`index::db_diesel::Index::with_partitioned_layers`]), so the tip
    ///   is a pure function of the query text and the pre-tree chain —
    ///   nothing completion-order-dependent feeds it.  A single-selection-
    ///   shard tree parents and tips exactly like the old chained model.
    /// - **Phase P (probe, concurrent):** cardinality probes + refinement
    ///   rounds under the tree-complete visibility — see the inline block
    ///   below for the full contract.
    /// - **Phase R (read, concurrent):** every statement's selection +
    ///   neighbourhood queries run via [`Command::compute_selection`] under
    ///   the tree-complete visibility, overlapped with `join_all` and applied
    ///   in pre-order.  A nested layer creator's rows are therefore visible
    ///   to its ancestors' neighbourhood queries and vice versa — the
    ///   symmetry the composition model assumes (`search("a"){search("b")}`
    ///   composes because each side's Phase R sees the other's layer).
    ///
    /// Trees run sequentially, so cross-tree `@label` deps and layer chaining
    /// always see prior trees fully applied — this subsumes the old
    /// PreSeedSibling pre-drains (the edges still exist for the notification
    /// pass and the budget gate).  A `@label` always references a statement
    /// from an EARLIER tree (enforced at parse time by
    /// `build_dependency_graph`'s ordering rule), which has fully run
    /// (M, P, R) by the time the referencing tree starts — so label
    /// resolution just reads the dependency's completed selection, never
    /// needing a mid-phase read.
    async fn compute_roots(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        _statements: &Vec<Rc<Statement>>,
    ) -> Result<(), pest::error::Error<Rule>> {
        let _span = tracing::info_span!("compute_roots").entered();

        let top_level: Vec<Rc<Statement>> = self.scope().statements().collect();
        for top in top_level {
            let mut tree: Vec<Rc<Statement>> = vec![top.clone()];
            crate::scope::visit(top.scope(), &mut |s| -> Result<
                bool,
                pest::error::Error<Rule>,
            > {
                tree.push(s.clone());
                Ok(true)
            })?;

            // ---- Phase M: every materialisation runs against the PRE-TREE
            // snapshot (no intra-tree chaining; `has_chain` and the
            // eph-referencing guards evaluate against it — correct by
            // construction, since labels may only reference earlier trees).
            //
            // CONCURRENT since the calls are mutually independent: the
            // sequential prep pass below does every `ctx` read (label
            // resolution, parent scopes), then the materialise futures
            // overlap via `join_all` exactly like Phase R's reads.  No
            // explicit fan-out bound, matching Phase R: the DB pool
            // (bb8, default 10 connections) bounds concurrency — each
            // in-flight layer holds ONE connection, blocked hash-race
            // losers wait on a row lock whose winner already owns its
            // connection, and pool-queued tasks hold no locks, so
            // exhaustion queues rather than deadlocks.  Two tree-mates
            // with IDENTICAL specs race on the same `(root, hash)` rows;
            // `create_eph_layer`'s ON CONFLICT + 2-PC `populated`
            // protocol serialises them and the loser becomes a cache
            // hit on the SAME layer id.
            //
            // Results are applied strictly IN PRE-ORDER below, so
            // completion order is unobservable: keys, parents, tip,
            // activation order, and warnings order all follow
            // substatement pre-order. ----
            let pre_tree_eph = ctx.eph.clone();
            let prepared: Vec<PreparedMaterialise> = tree
                .iter()
                .map(|statement| prepare_materialise(ctx, statement, &pre_tree_eph))
                .collect();
            let futs = prepared.iter().map(|p| {
                p.statement.command().materialise_layers(
                    cfg,
                    &pre_tree_eph,
                    &p.resolved,
                    &p.parent_scope,
                )
            });
            let results = futures::future::join_all(futs).await;
            // Check all results before applying any — avoid partial ctx
            // mutation on error (same contract as Phase R).  `join_all`
            // drives every future to completion, so no sibling's layer
            // transaction is cancelled mid-populate by another's error.
            let outcomes = results
                .into_iter()
                .collect::<Result<Vec<_>, pest::error::Error<Rule>>>()?;
            let mut staged: Vec<Staged> = Vec::with_capacity(tree.len());
            for (p, outcome) in prepared.into_iter().zip(outcomes) {
                ctx.layer_activations
                    .extend(outcome.activations.iter().copied());
                staged.push(Staged {
                    statement: p.statement,
                    layer_ids: outcome.layer_ids,
                    layers: outcome.layers,
                    warnings: outcome.warnings,
                });
            }

            // The tree's layers enter visibility as ONE materialisation:
            // substatement pre-order, each command's layers in their canonical
            // internal order — making the last layer per root the tree's new
            // tip (see the spine advance rule in the doc comment above).
            let tree_materialisation: Vec<(i64, i64)> = staged
                .iter()
                .flat_map(|s| s.layers.iter().copied())
                .collect();
            if !tree_materialisation.is_empty() {
                ctx.eph.push_materialisation(&tree_materialisation);
            }

            // ---- Phase P: cardinality probes, concurrent, under
            // tree-complete visibility.
            //
            // Wave 0: every eligible statement (anchored, all selectors
            // fusable, not read early) probes its fused predicate with the
            // request's cap.  Resolved outcomes hold the statement's EXACT
            // current-instance set.
            //
            // Refinement rounds: statements still unresolved re-probe under
            // semi-join roles built from neighbours resolved in earlier
            // rounds.  A neighbour related by BOTH refs and has (the `{ }`
            // default: either edge kind counts) contributes one probe per
            // relationship BRANCH, and the statement resolves iff every
            // branch combination resolves — the union of the combination
            // results is then exact under the either-edge semantics.
            //
            // Sound by monotonicity: a role is at least as permissive as
            // the edge-evidence relation the worklist later narrows by, and
            // a neighbour's resolved set is a superset of its final
            // selection — so a refinement-resolved set is a superset of the
            // statement's final selection, exact in composition.
            //
            // Consumers: Phase-R emission (fused statements fetch by id)
            // and the scope builders (a resolved neighbour contributes ids
            // instead of a condition the index would have to materialise —
            // this is what keeps a broad container from resolving a bare
            // type predicate to millions of ids).
            let run_probe_wave = |entries: Vec<(
                Rc<Statement>,
                CompositeFilter,
                Vec<index::db_diesel::ProbeRole>,
            )>,
                                  cap: usize,
                                  eph: EphContext| {
                async move {
                    let mut futs = Vec::new();
                    let mut probed = Vec::new();
                    for (stmt, predicate, roles) in entries {
                        let eph = eph.clone();
                        let index = &cfg.index;
                        probed.push(stmt);
                        futs.push(async move {
                            index.probe_instance_ids(&predicate, roles, cap, &eph).await
                        });
                    }
                    let results = futures::future::join_all(futs).await;
                    let mut outcomes = Vec::new();
                    for (stmt, outcome) in probed.into_iter().zip(results) {
                        let outcome = outcome.map_err(|e| {
                            pest::error::Error::new_from_span(
                                pest::error::ErrorVariant::CustomError {
                                    message: format!("cardinality probe failed: {e}"),
                                },
                                stmt.command().span().as_pest_span(),
                            )
                        })?;
                        outcomes.push((stmt, outcome));
                    }
                    Ok::<_, pest::error::Error<Rule>>(outcomes)
                }
            };

            // Group per-branch outcomes by statement: resolved iff EVERY
            // branch resolved (ids unioned), else capped.  Records one
            // activation per statement per round.
            let settle = |outcomes: Vec<(Rc<Statement>, index::db_diesel::ProbeOutcome)>,
                          ctx: &mut ExecutionContext,
                          round: usize|
             -> bool {
                let mut order: Vec<usize> = Vec::new();
                let mut groups: HashMap<
                    usize,
                    (Rc<Statement>, Vec<index::db_diesel::ProbeOutcome>),
                > = HashMap::new();
                for (stmt, outcome) in outcomes {
                    let key = statement_key(&stmt);
                    groups
                        .entry(key)
                        .or_insert_with(|| {
                            order.push(key);
                            (stmt, Vec::new())
                        })
                        .1
                        .push(outcome);
                }
                let mut any_resolved = false;
                for key in order {
                    let (stmt, branch_outcomes) = groups.remove(&key).unwrap();
                    let statement_text = stmt.command().span().as_pest_span().as_str().to_string();
                    let mut ids: Vec<i64> = Vec::new();
                    let mut capped = false;
                    for outcome in branch_outcomes {
                        match outcome {
                            index::db_diesel::ProbeOutcome::Resolved(branch) => ids.extend(branch),
                            index::db_diesel::ProbeOutcome::Capped => capped = true,
                        }
                    }
                    if capped {
                        ctx.probe_activations.push(ProbeActivation {
                            query_statement: statement_text,
                            resolved: None,
                            round,
                        });
                    } else {
                        ids.sort_unstable();
                        ids.dedup();
                        ctx.probe_activations.push(ProbeActivation {
                            query_statement: statement_text,
                            resolved: Some(ids.len()),
                            round,
                        });
                        ctx.probe_resolved.insert(key, ids);
                        any_resolved = true;
                    }
                }
                any_resolved
            };

            // Wave 0.
            let mut wave: Vec<(
                Rc<Statement>,
                CompositeFilter,
                Vec<index::db_diesel::ProbeRole>,
            )> = Vec::new();
            for m in &staged {
                let Some(predicate) = m.statement.command().probe_predicate(&ctx.eph) else {
                    continue;
                };
                wave.push((m.statement.clone(), predicate, vec![]));
            }
            let outcomes = run_probe_wave(wave, ctx.probe_cap, ctx.eph.clone()).await?;
            settle(outcomes, ctx, 0);

            // Refinement rounds, to fixpoint.  A candidate re-probes only
            // when a NEW neighbour resolved since its last attempt.
            const MAX_ROLE_BRANCHES: usize = 4;
            let mut last_neighbor_count: HashMap<usize, usize> = HashMap::new();
            for round in 1..=staged.len() {
                let mut wave: Vec<(
                    Rc<Statement>,
                    CompositeFilter,
                    Vec<index::db_diesel::ProbeRole>,
                )> = Vec::new();
                for m in &staged {
                    let stmt = &m.statement;
                    let key = statement_key(stmt);
                    if ctx.probe_resolved.contains_key(&key) {
                        continue;
                    }
                    if !refinable(stmt.command()) {
                        continue;
                    }
                    // Fused statements re-probe their wave-0 predicate
                    // (unconstrained selectors like `all` fold to TRUE there,
                    // which the honest-OR scope condition cannot express);
                    // derive-path statements re-probe their scope condition.
                    // Fused statements re-probe their wave-0 predicate
                    // (unconstrained selectors like `all` fold to TRUE there,
                    // which the honest-OR scope condition cannot express);
                    // otherwise the scope condition; otherwise — for pure
                    // derive-path statements (all selectors non-constraining,
                    // e.g. `func { }`, whose bare type selector is not even a
                    // selector once inherit applies) — the filter conjunction
                    // the derive path itself applies.
                    let all_non_constraining = stmt
                        .command()
                        .selectors()
                        .all(|s| s.is_non_constraining_selector());
                    let pp = stmt.command().probe_predicate(&ctx.eph);
                    let sc = stmt.command().get_selector_composite_filter(&ctx.eph);
                    let cp = if all_non_constraining {
                        stmt.command().constraint_predicate(&ctx.eph)
                    } else {
                        None
                    };
                    let Some(predicate) = pp.or(sc).or(cp) else {
                        continue;
                    };
                    // Bind ONLY the smallest resolved neighbour's roles: a
                    // role's probe cost scales with the bound id set, and a
                    // broad neighbour (a resolved container with hundreds of
                    // instances) can cost more to AND in than it narrows —
                    // measured as 2.7s of the amdgpu worst case.  The result
                    // is a (still sound) superset; the worklist narrows the
                    // rest.  A pure refs or has relationship contributes one
                    // branch, the combined default contributes both (union
                    // semantics).
                    use index::db_diesel::ProbeRole;
                    let mut smallest: Option<(usize, Vec<ProbeRole>)> = None;
                    let mut consider =
                        |ids: &Vec<i64>, rel: RelationshipType, parent_side: bool| {
                            let mut branches = Vec::new();
                            if rel.contains(RelationshipType::REFS) {
                                branches.push(if parent_side {
                                    ProbeRole::RefsChildrenOf(ids.clone())
                                } else {
                                    ProbeRole::RefsParentsOf(ids.clone())
                                });
                            }
                            if rel.contains(RelationshipType::HAS) {
                                branches.push(if parent_side {
                                    ProbeRole::HasChildrenOf(ids.clone())
                                } else {
                                    ProbeRole::HasParentsOf(ids.clone())
                                });
                            }
                            if branches.is_empty() {
                                return; // EMPTY relationship: no constraint
                            }
                            let better = match &smallest {
                                None => true,
                                Some((best_len, _)) => ids.len() < *best_len,
                            };
                            if better {
                                smallest = Some((ids.len(), branches));
                            }
                        };
                    if let Some(parent) = stmt.parent().and_then(|p| p.upgrade()) {
                        if let Some(ids) = ctx.probe_resolved.get(&statement_key(&parent)) {
                            consider(ids, stmt.relationship_type, true);
                        }
                    }
                    for child in stmt.children() {
                        if let Some(ids) = ctx.probe_resolved.get(&statement_key(&child)) {
                            consider(ids, child.relationship_type, false);
                        }
                    }
                    let Some((best_len, branches)) = smallest else {
                        continue;
                    };
                    // Re-probe only when a strictly smaller binding appeared
                    // since the last attempt.
                    if last_neighbor_count
                        .get(&key)
                        .is_some_and(|prev| *prev <= best_len)
                    {
                        continue;
                    }
                    let branch_sets: Vec<Vec<ProbeRole>> = vec![branches];
                    let neighbors = best_len;
                    // Cartesian product of the branch sets: one probe per
                    // combination, ANDing one branch per neighbour.
                    let mut combos: Vec<Vec<ProbeRole>> = vec![Vec::new()];
                    for set in &branch_sets {
                        let mut next = Vec::new();
                        for combo in &combos {
                            for role in set {
                                let mut c = combo.clone();
                                c.push(role.clone());
                                next.push(c);
                            }
                        }
                        combos = next;
                    }
                    if combos.len() > MAX_ROLE_BRANCHES {
                        continue; // pathological fan-in: keep the predicate path
                    }
                    last_neighbor_count.insert(key, neighbors);
                    for combo in combos {
                        wave.push((stmt.clone(), predicate.clone(), combo));
                    }
                }
                if wave.is_empty() {
                    break;
                }
                let outcomes = run_probe_wave(wave, ctx.probe_cap, ctx.eph.clone()).await?;
                if !settle(outcomes, ctx, round) {
                    break;
                }
            }

            // ---- Phase R: concurrent reads under tree-complete
            // visibility. ----
            struct PendingRead<'a> {
                statement: Rc<Statement>,
                future: Pin<
                    Box<dyn Future<Output = Result<ComputeResult, pest::error::Error<Rule>>> + 'a>,
                >,
            }
            let mut pending: Vec<PendingRead<'_>> = Vec::new();
            for m in staged {
                let (layer_ids, warnings) = (m.layer_ids, m.warnings);
                let (eph, parent_scope, children_scope) = stage_read(ctx, &m.statement);
                let probe_ids = ctx
                    .probe_resolved
                    .get(&statement_key(&m.statement))
                    .cloned();
                let stmt = m.statement.clone();
                pending.push(PendingRead {
                    statement: m.statement,
                    future: Box::pin(async move {
                        stmt.command()
                            .compute_selection(
                                cfg,
                                parent_scope,
                                children_scope,
                                &eph,
                                &layer_ids,
                                warnings,
                                probe_ids,
                            )
                            .await
                    }),
                });
            }
            let (stmts, futs): (Vec<_>, Vec<_>) =
                pending.into_iter().map(|p| (p.statement, p.future)).unzip();
            let results = futures::future::join_all(futs).await;
            // Check all results before applying any — avoid partial ctx
            // mutation on error.
            let computed: Result<Vec<_>, _> = stmts
                .into_iter()
                .zip(results)
                .map(|(stmt, r)| r.map(|cr| (stmt, cr)))
                .collect();
            for (stmt, cr) in computed? {
                cr.apply(ctx, &stmt);
            }
        }
        Ok(())
    }

    fn build_dependency_graph(
        &self,
        labeled_statements_map: &LabeledStatements,
    ) -> Result<(), pest::error::Error<Rule>> {
        let _span = tracing::debug_span!("build_dependency_graph").entered();

        // Map every statement to the source-order index of its top-level
        // tree.  `build_dependency_graph` uses it to enforce the label
        // ordering rule: a `@label` may only reference a label defined by
        // an EARLIER top-level statement.
        let top_level: Vec<Rc<Statement>> = self.scope().statements().collect();
        let mut tree_of: HashMap<*const Statement, usize> = HashMap::new();
        for (i, top) in top_level.iter().enumerate() {
            tree_of.insert(Rc::as_ptr(top), i);
            crate::scope::visit(top.scope(), &mut |s| -> Result<
                bool,
                pest::error::Error<Rule>,
            > {
                tree_of.insert(Rc::as_ptr(&s), i);
                Ok(true)
            })?;
        }

        crate::scope::visit(self.scope(), &mut |statement| -> Result<
            bool,
            pest::error::Error<Rule>,
        > {
            build_dependency_graph(statement, &labeled_statements_map, &tree_of)?;
            Ok(true)
        })?;

        // Top-level sibling-ordering edges.
        //
        // Execution order no longer depends on these: trees run
        // sequentially and each tree's layers land as ONE materialisation, so
        // ordering is structural.  The edges survive for their two
        // remaining readers — the notification pass (`notify`
        // short-circuits every `PreSeed*` edge explicitly) and the
        // budget gate in `stage_read` (`PreSeedSibling` is the one
        // dependent role with no data flow, so it does NOT clear the
        // budget).
        //
        // Construction: each top-level statement links to the most
        // recent layer-creating top-level statement before it — O(N)
        // edges, none between layer-free statements.  Nested siblings
        // rely on Parent/Child edges, and a nested layer's `@label`
        // refs point at earlier trees (parse-enforced); a nested
        // `loc(...) ; loc(...)` pair does NOT get a Sibling edge.
        let mut last_layer_creator: Option<usize> = None;
        for j in 0..top_level.len() {
            if let Some(i) = last_layer_creator {
                let s_i = &top_level[i];
                let s_j = &top_level[j];
                s_i.get_state_mut().dependents.push(StatementDependent::new(
                    s_j.clone(),
                    DependencyRole::PreSeedSibling,
                ));
                s_j.get_state_mut()
                    .dependencies
                    .push(StatementDependency::new(
                        s_i.clone(),
                        DependencyRole::PreSeedSibling,
                    ));
            }
            if top_level[j].command().has_layer_spec() {
                last_layer_creator = Some(j);
            }
        }
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
        let _span = tracing::debug_span!("mark_weak_statements").entered();
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

    async fn run_worklist(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        statements: &[Rc<Statement>],
    ) -> Result<(), pest::error::Error<Rule>> {
        let mut worklist = Worklist::new();

        // Seed: every statement that already has a selection after root initialization.
        for stmt in statements {
            if stmt.has_selection(ctx) {
                worklist.schedule(stmt.clone());
            }
        }

        while let Some(current_statement) = worklist.pop_next(ctx) {
            // Let each selector refresh its own state before it propagates
            // (`UserVerb` adopts its forced selection here).  Filtering is
            // NOT re-applied: a command's filters are part of the predicate
            // its rows were selected with, so re-running them Rust-side would
            // be a no-op over rows that already satisfy them.
            ctx.registry.for_each_selector_mut(
                current_statement.command().selectors(),
                |selector, state| selector.update_state(state),
            );

            // No selection → nothing to propagate.
            if !current_statement.has_selection(ctx) {
                continue;
            }

            let stmt_label: String = current_statement
                .command()
                .selectors()
                .map(|s| s.name().to_string())
                .collect::<Vec<_>>()
                .join(", ");
            let _statement_iteration: tracing::span::EnteredSpan =
                tracing::info_span!("statement_iteration", stmt = %stmt_label).entered();

            // Yield to the runtime so tokio::time::timeout can fire if the
            // query has exceeded its deadline.
            tokio::task::yield_now().await;

            // Notify dependents; reschedule those whose selection changed.
            let dependents = current_statement.get_state().dependents.clone();
            for dependent in dependents {
                let result = current_statement.notify(ctx, cfg, &dependent).await?;
                if result.changed {
                    worklist.schedule(dependent.statement.clone());
                }
            }
        }

        Ok(())
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

        // Build the dependency graph first — `compute_roots` reads
        // `DependencyRole::PreSeedLabel` edges to resolve `@label`
        // references (each dependency lives in an earlier, fully-run
        // tree — the parse-time ordering rule), and the budget gate in
        // `stage_read` consults all edges.  See
        // `Self::build_dependency_graph`.
        self.build_dependency_graph(&labeled_statements)?;

        // Every component (top-level tree ∪ label-connected trees) must
        // carry at least one anchor — a component without one cannot
        // produce anything, and used to return empty silently.
        anchor::check_anchor_completeness(self)?;

        self.mark_weak_statements(&statements);

        // Compute initial selections.  `compute_roots` resolves `@label`
        // references via the `PreSeedLabel` edges installed above — each
        // dependency lives in an earlier, fully-run tree, so its
        // completed selection is simply read.
        self.compute_roots(ctx, cfg, &statements).await?;

        self.run_worklist(ctx, cfg, &statements).await?;

        Ok(statements)
    }

    /// Gather all warnings from the statement and its scope, collapsing
    /// span-overlapping ones to a single warning per region (keeps the
    /// earliest by start position). Dedup is **kind-aware**: a bare `Note`
    /// must not shadow a `NoMatch` at the same/overlapping span, since the
    /// `NoMatch` carries the enriched suggestions/`name_exists` — so a
    /// `NoMatch` always wins over an overlapping `Note`.
    pub fn gather_warnings(&self, statements: &Vec<Rc<Statement>>) -> Vec<Diagnostic> {
        let mut all_warnings = vec![];
        for statement in statements.iter() {
            let state = statement.get_state();
            all_warnings.extend(state.warnings.clone());
        }

        // Order by start; among equal starts, NoMatch before Note so it is the
        // one kept when spans coincide.  Truncation warnings sort with Notes
        // but are EXEMPT from the overlap-collapse below.
        let kind_rank = |d: &Diagnostic| match d.kind {
            DiagnosticKind::NoMatch => 0u8,
            DiagnosticKind::Note => 1u8,
            DiagnosticKind::Truncation => 2u8,
        };
        all_warnings.sort_by_key(|d| (d.start(), kind_rank(d)));

        let mut filtered: Vec<Diagnostic> = vec![];
        for warning in all_warnings {
            // Truncation must always surface: it is never dropped by the
            // overlap-collapse and never displaces/gets displaced.  Truncation
            // is acceptable, hidden truncation is not.
            if warning.kind == DiagnosticKind::Truncation {
                filtered.push(warning);
                continue;
            }
            if let Some(last) = filtered
                .iter()
                .rev()
                .find(|d| d.kind != DiagnosticKind::Truncation)
            {
                if last.end() >= warning.start() {
                    // Overlaps the previous kept warning. A NoMatch may still
                    // displace a kept Note (it carries the useful payload);
                    // otherwise drop the overlapping warning.
                    if warning.kind == DiagnosticKind::NoMatch && last.kind == DiagnosticKind::Note
                    {
                        let idx = filtered
                            .iter()
                            .rposition(|d| d.kind != DiagnosticKind::Truncation)
                            .unwrap();
                        filtered[idx] = warning;
                    }
                    continue;
                }
            }
            filtered.push(warning);
        }

        filtered
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
        let _span = tracing::debug_span!("collect_ref_edges").entered();
        let mut all_references = EdgeList::new();
        // Dedup key: (from_instance_id, to_symbol_id, occurrence)
        // Instance-level on from side prevents suppressing edges between different
        // source instances of the same symbol. Symbol-level on to side deduplicates
        // when the target symbol has multiple selected instances (e.g., definition
        // + declaration of f), since a single reference should produce one edge.
        let mut seen_edges: HashSet<(SymbolInstanceId, SymbolId, Option<Occurrence>)> =
            HashSet::new();

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
        let mut instance_to_node: HashMap<i64, (SymbolId, SymbolInstanceId)> = HashMap::new();
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

        // Canonical order: all_nodes is a HashSet with random iteration
        // order, and these ids become rendered binds in the SQL-cache key
        // for find_edges_between.
        let all_ids = canonical_ids(all_nodes.iter().map(|id| Into::<i64>::into(*id)));
        if let Ok(implicit_edges) = index.find_edges_between(&all_ids, &ctx.eph).await {
            for edge in implicit_edges {
                let from_node = instance_to_node.get(&edge.from_instance_id);
                let to_node = instance_to_node.get(&edge.to_instance_id);
                if let (Some((from_sym, from_inst)), Some((_to_sym, to_inst))) =
                    (from_node, to_node)
                {
                    let occurrence = Occurrence {
                        file: FileId::new(edge.from_object),
                        offset_range: range_bounds_to_offsets(&edge.from_offset_range).unwrap(),
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
        let _span = tracing::debug_span!("collect_has_edges").entered();
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
                (
                    &h.parent_instance,
                    &h.child_instance,
                    h.parent_symbol.id,
                    h.child_symbol.id,
                )
            });
            let from_parents = current.has_parents.iter().map(|h| {
                (
                    &h.parent_instance,
                    &h.child_instance,
                    h.parent_symbol.id,
                    h.child_symbol.id,
                )
            });

            for (parent_inst, child_inst, parent_sym, child_sym) in
                from_children.chain(from_parents)
            {
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

        let mut warnings = self.gather_warnings(&statements);
        enrich_no_match_suggestions(&mut warnings, ctx, cfg).await;

        let _collect_results = tracing::debug_span!("collect_results").entered();
        let mut node_map: HashMap<i64, index::db_diesel::SelectionNode> = HashMap::new();
        for statement in &statements {
            if let Some(selection) = statement.get_selection(ctx) {
                let span = statement.command.query_statement_span();
                let stmt_info = index::db_diesel::QueryStatementRange {
                    start: span.start(),
                    end: span.end(),
                    text: span.as_pest_span().as_str().to_string(),
                };
                for mut node in selection.nodes {
                    node.query_statements.push(stmt_info.clone());
                    node_map
                        .entry(node.symbol_instance.id)
                        .and_modify(|existing| {
                            existing
                                .query_statements
                                .extend(node.query_statements.iter().cloned());
                        })
                        .or_insert(node);
                }
            }
        }

        let all_nodes = HashSet::<SymbolInstanceId>::from_iter(
            node_map.keys().map(|id| SymbolInstanceId::new(*id)),
        );

        let ref_edges = Self::collect_ref_edges(&statements, ctx, &all_nodes, &cfg.index).await;
        let has_edges = Self::collect_has_edges(&statements, ctx, &all_nodes);

        Ok(ExecutionResult::new(
            NodeList(node_map.into_values().collect()),
            ref_edges,
            has_edges,
            warnings,
        ))
    }

    /// Notify the dependent statement's execution state about change in the
    /// state of a dependency.
    ///
    /// When children notify their parent (role=Parent) they **conjoin**: the
    /// parent is constrained once per participating child, against that
    /// child's own selection, so it retains only nodes that have evidence
    /// with EVERY one of them.  `X { A ; B }` means *has evidence with A and
    /// with B*; the disjunction is spelled `X { A B }`, several selectors in
    /// one command.  An empty participant therefore empties the parent, just
    /// as a lone empty child does today.
    ///
    /// No deferral: each conjunct is an independently sound upper bound and
    /// `retain` is monotone and idempotent, so children may constrain in any
    /// order, as they resolve, and re-constraining by an already-applied
    /// sibling is a no-op.  The worklist reschedules the parent on every
    /// child's change, which is what makes the fixpoint the full conjunction.
    pub async fn notify(
        &self,
        ctx: &mut ExecutionContext,
        cfg: &ControlFlowGraph,
        dependent: &StatementDependent,
    ) -> Result<PropagationResult, pest::error::Error<Rule>> {
        let _update_dependency: tracing::span::EnteredSpan =
            tracing::debug_span!("notify").entered();

        // PreSeed edges carry no selection data — they exist only to
        // express "compute me after this dep's selection has applied."
        // Ordering is guaranteed by sequential tree execution (labels
        // reference earlier trees only); by the time `run_worklist` is
        // calling `notify`, the receiver's `compute_selected` has
        // already run with the right `eph` and resolved labels.
        // Nothing to propagate.
        if dependent.dependency_role.is_pre_seed() {
            return Ok(PropagationResult { changed: false });
        }

        if dependent.dependency_role == DependencyRole::Parent {
            // A child takes part iff it has selectors and is not a
            // display-only weak unit — the membership test is unchanged;
            // only what we do with the members is.  Collect the
            // participants' selections up front: they are owned clones, so
            // the immutable `ctx` read is over before the mutable
            // `notify_from_selection` calls begin.
            let selections: Vec<Selection> = dependent
                .statement
                .children()
                .filter(|child| {
                    child.command().has_selectors() && !should_skip_in_parent_merge(child)
                })
                .filter_map(|child| child.get_selection(ctx))
                .collect();

            // Nothing to conjoin yet — an unresolved participant simply has
            // not spoken.  It will notify when it resolves, and its conjunct
            // will narrow the parent then.
            if selections.is_empty() {
                return Ok(PropagationResult { changed: false });
            }

            // Use the notifying child's relationship type (all siblings share it).
            let rel_type = self.get_relationship_type();

            let unnest = dependent.statement.is_unnest();
            let parent_scope = build_parent_scope(&dependent.statement, ctx, &ctx.eph);
            // The children scope stays the UNION of the participants' ids.
            // It bounds the neighbourhood PREFETCH, not the semantics: the
            // gate is the per-child constrain below.  Intersecting it would
            // drop the edges to the siblings the parent is displayed with.
            let mut scope_ids: Vec<i64> = selections
                .iter()
                .flat_map(|sel| sel.get_instance_ids())
                .collect();
            scope_ids.sort_unstable();
            scope_ids.dedup();
            let children_scope = ScopeContext::Scope {
                ids: scope_ids,
                filter: None,
            };

            // One constrain per participant.  The FIRST call may find the
            // parent without a selection and derive it from that child
            // alone; every later call sees a selection and constrains it, so
            // the fixpoint is the intersection — and derivation runs at most
            // once, with no extra SQL for the conjunction.
            let mut changed = false;
            for selection in &selections {
                let res = dependent
                    .statement
                    .command()
                    .notify_from_selection(
                        ctx,
                        &cfg.index,
                        selection,
                        DependencyRole::Parent,
                        rel_type,
                        unnest,
                        parent_scope.clone(),
                        children_scope.clone(),
                    )
                    .await?;

                changed |= res.changed;
                dependent
                    .statement
                    .get_state_mut()
                    .warnings
                    .extend(res.warnings);
            }
            return Ok(PropagationResult { changed });
        }

        // Original flow for Child and User roles.
        // Resolve rel_type at the single call site instead of duplicating
        // the role-based match in each accept_notification implementation.
        let rel_type = match &dependent.dependency_role {
            DependencyRole::Child => dependent.statement.get_relationship_type(),
            DependencyRole::Parent | DependencyRole::User => self.get_relationship_type(),
            // Unreachable: PreSeed* notifications are short-circuited at
            // the top of `notify`.  Use the notifier's rel_type as a
            // defensive default rather than panic.
            DependencyRole::PreSeedSibling | DependencyRole::PreSeedLabel(_) => {
                self.get_relationship_type()
            }
        };
        let notif_ctx = NotificationContext {
            role: dependent.dependency_role.clone(),
            rel_type,
            unnest: dependent.statement.is_unnest(),
        };
        let parent_scope = build_parent_scope(&dependent.statement, ctx, &ctx.eph);
        let children_scope = build_children_scope(&dependent.statement, ctx, &ctx.eph);
        let res = dependent
            .statement
            .command()
            .accept_notification(
                ctx,
                &cfg.index,
                self,
                notif_ctx,
                parent_scope,
                children_scope,
            )
            .await?;

        let changed = res.changed;
        dependent
            .statement
            .get_state_mut()
            .warnings
            .extend(res.warnings);
        Ok(PropagationResult { changed })
    }
}

struct Worklist(Vec<Rc<Statement>>);

impl Worklist {
    fn new() -> Self {
        Worklist(Vec::new())
    }

    /// Add a statement to the worklist if not already present.
    fn schedule(&mut self, stmt: Rc<Statement>) {
        if !self.0.iter().any(|s| Rc::ptr_eq(s, &stmt)) {
            self.0.push(stmt);
        }
    }

    /// Remove and return the statement with the lowest propagation_priority.
    fn pop_next(&mut self, ctx: &ExecutionContext) -> Option<Rc<Statement>> {
        if self.0.is_empty() {
            return None;
        }
        let idx = self
            .0
            .iter()
            .enumerate()
            .min_by_key(|(_, s)| s.propagation_priority(ctx))
            .map(|(i, _)| i)?;
        Some(self.0.swap_remove(idx))
    }
}

/// Canonicalize resolved symbol ids for label resolution: sorted, deduped.
///
/// Selection nodes are per-INSTANCE and arrive in raw, unordered SQL result
/// order (the selection queries have no ORDER BY, so the order is
/// plan-dependent), and a symbol with N instances appears N times.  The
/// resolved ids feed ephemeral-op cache keys verbatim (`hash_params` folds
/// the sequence), so without canonicalization the same logical resolution
/// produces different layer hashes run-to-run — duplicate cache entries and
/// near-zero reuse.  Sorting + deduping also matches what the layer actually
/// stores: duplicate rows are collapsed by `ON CONFLICT DO NOTHING` on
/// insert anyway.
pub(crate) fn canonical_ids(ids: impl Iterator<Item = i64>) -> Vec<i64> {
    let mut ids: Vec<i64> = ids.collect();
    ids.sort_unstable();
    ids.dedup();
    ids
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
