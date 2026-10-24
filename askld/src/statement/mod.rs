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
use crate::verb::{name_filter, weak_notifier_blocks, LabelResolutions, NotificationContext};
use anyhow::Result;
use core::fmt::Debug;
use index::db_diesel::{CompositeFilter, EphContext, ScopeContext, Selection, TraversalMode};
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
mod weakness;

pub use self::parse::{build_dependency_graph, build_empty_statement, build_statement};

use self::scope_helpers::{
    build_children_scope, build_family_demands, build_parent_scope, is_echo,
};
use index::db_diesel::FamilyDemands;

pub struct ExecutionResult {
    pub nodes: NodeList,
    pub edges: EdgeList,
    pub has_edges: HasEdgeList,
    pub warnings: Vec<Diagnostic>,
    /// Symbols whose implicit out-edge list hit the per-node `edge_cap`
    /// (assembly capping) — an invitation to fetch the rest per node, never
    /// an incompleteness warning.
    pub edges_truncated: Vec<SymbolId>,
}

impl ExecutionResult {
    pub fn new(
        nodes: NodeList,
        edges: EdgeList,
        has_edges: HasEdgeList,
        warnings: Vec<Diagnostic>,
        edges_truncated: Vec<SymbolId>,
    ) -> ExecutionResult {
        ExecutionResult {
            nodes,
            edges,
            has_edges,
            warnings,
            edges_truncated,
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

/// Whether an edge to `neighbour` means someone will read this statement's
/// ROWS as if they were the whole answer.
///
/// One law decides it, and [`Statement::notify`] fences the same sentence:
/// **truncated rows may only travel where they cannot narrow an independent
/// selection.**  An [`is_echo`] neighbour has none — it derives from us
/// (`derive_from_parent`) or re-displays what we found, so a bound on our rows
/// simply bounds the echo too, and our own truncation warning already says so.
/// Any other neighbour computes a selection of its own, which `constrain_*`
/// then retains against our rows, silently pruning neighbours of the rows we
/// never fetched.
///
/// `PreSeedSibling` carries no data at all.  `PreSeedLabel` always counts,
/// echo or not: `compute_roots` reads the selection for `@label` resolution,
/// and a truncated, order-arbitrary id set would both seed the layer wrongly
/// and destabilise its input hash.
fn edge_consumes_rows(role: &DependencyRole, neighbour: &Rc<Statement>) -> bool {
    match role {
        DependencyRole::PreSeedSibling => false,
        DependencyRole::PreSeedLabel(_) => true,
        DependencyRole::Parent | DependencyRole::Child | DependencyRole::User => {
            !is_echo(neighbour)
        }
    }
}

/// Budget gate + scope construction for a statement's READ phase (Phase R).
///
/// Budget-pushdown safety gate: only a statement whose rows nobody consumes
/// may bound its leaves.  [`edge_consumes_rows`] answers that per edge; if any
/// edge says yes, the budget goes.
///
/// This gate is the coarse one.  `find_symbol` applies a second, finer rule
/// (a `Scope` on either side also drops the bound) — it catches the case where
/// this statement's own neighbourhood rows feed a Rust-side intersection,
/// which is invisible from here.  Both must hold; neither subsumes the other.
fn stage_read(
    ctx: &ExecutionContext,
    statement: &Rc<Statement>,
) -> (EphContext, ScopeContext, ScopeContext, FamilyDemands) {
    let mut eph = ctx.eph.clone();
    {
        let st = statement.get_state();
        let composed = st
            .dependencies
            .iter()
            .any(|d| edge_consumes_rows(&d.dependency_role, &d.dependency))
            || st
                .dependents
                .iter()
                .any(|d| edge_consumes_rows(&d.dependency_role, &d.statement));
        if composed {
            eph.set_result_budget(index::db_diesel::ResultBudget::UNLIMITED);
        }
    }
    let parent_scope = build_parent_scope(statement, ctx, &eph);
    let children_scope = build_children_scope(statement, ctx, &eph);
    let demands = build_family_demands(statement);
    (eph, parent_scope, children_scope, demands)
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
    /// How far this statement's scope reaches into the containment tree.
    /// Unfolded by default: descendants with no fold boundary between.
    /// `nested` narrows it to one structural level, `unnest` widens it to
    /// every level.  The same knob drives upward HAS derivation.
    pub mode: TraversalMode,
}

impl Statement {
    pub fn new(command: Command, scope: Rc<dyn Scope>) -> Rc<Statement> {
        Statement::new_full(
            command,
            scope,
            RelationshipType::REFS,
            TraversalMode::Unfolded,
        )
    }

    pub fn new_with_relationship(
        command: Command,
        scope: Rc<dyn Scope>,
        relationship_type: RelationshipType,
    ) -> Rc<Statement> {
        Statement::new_full(command, scope, relationship_type, TraversalMode::Unfolded)
    }

    pub fn new_full(
        command: Command,
        scope: Rc<dyn Scope>,
        relationship_type: RelationshipType,
        mode: TraversalMode,
    ) -> Rc<Statement> {
        Rc::new(Statement {
            command,
            scope,
            parent: RefCell::new(None),
            execution_state: RefCell::new(ExecutionState::new()),
            relationship_type,
            mode,
        })
    }

    pub fn traversal_mode(&self) -> TraversalMode {
        self.mode
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

    /// This statement's selection, merged across its selectors.
    ///
    /// Returns an `Rc`: a single-selector statement — the common case, and the
    /// one where copying costs most, since a wide statement's neighbourhood
    /// families run to six figures of rows — hands out a refcount bump. Only a
    /// genuine multi-selector merge allocates, which is exactly what the old
    /// unconditional deep copy was buying. The final graph-assembly phase asks
    /// for the selection once per consumer while each consumer reads ONE field,
    /// so the copy was paid five times over per statement.
    ///
    /// Accumulation rule: a selector with no selection RESETS the accumulation
    /// (the statement is not ready) and a later selector with one starts it
    /// again — so [NameSel=Some, UserVerb=None] is None (not ready until
    /// UserVerb resolves), while [TypeSel(filter_only)=None, NameSel=Some] is
    /// Some. See
    /// [`crate::execution_context::SelectorRegistry::contributing_selections`].
    /// The largest `listing` any command in this query requests (via the
    /// `limits()` verb).  Used before execution to size the discovery-side
    /// edge fetch so the SQL never under-fetches for the most generous
    /// command; render then trims each node to its own listing.  `None` when
    /// no command sets one.
    pub fn max_listing_limit(&self) -> Option<usize> {
        let mut max: Option<usize> = None;
        let _ = crate::scope::visit::<_, ()>(self.scope(), &mut |stmt| {
            if let Some(v) = stmt.command().limits().listing {
                max = Some(max.map_or(v, |m| m.max(v)));
            }
            Ok(true)
        });
        max
    }

    pub fn get_selection(&self, ctx: &ExecutionContext) -> Option<Rc<Selection>> {
        let contributing = ctx
            .registry
            .contributing_selections(self.command().selectors())?;
        let (first, rest) = contributing.split_first()?;
        if rest.is_empty() {
            return Some(Rc::clone(first));
        }
        let mut merged = first.as_ref().clone();
        for sel in rest {
            merged.extend(sel.as_ref().clone());
        }
        Some(Rc::new(merged))
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

    /// Whether any selector's selection stopped at the result budget's LIMIT,
    /// without cloning (`get_selection` builds a merged copy).  `Selection::
    /// extend` ORs the flag, so asking per selector matches what the merged
    /// selection would say.  Read by the budget fence in [`Statement::notify`].
    fn selection_is_budget_bounded(&self, ctx: &ExecutionContext) -> bool {
        let mut bounded = false;
        ctx.registry
            .for_each_selector(self.command().selectors(), |selector, state| {
                if let Some(sel) = selector.get_selection(state) {
                    bounded |= sel.budget_bounded;
                }
            });
        bounded
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
            // Per candidate: the binding set its last derivation was built
            // from, as sorted (neighbour, size) pairs.  Memoising the INPUT
            // rather than the trigger is what stops a capped statement
            // re-running an identical query every time any neighbour moves.
            // Per candidate: the composition its last probe actually used, as
            // sorted (neighbour, size) pairs, and how many neighbours that was.
            // Retrying is worth it only when the next attempt would be strictly
            // more restrictive than what is recorded here.
            let mut last_used: HashMap<usize, Vec<(usize, usize)>> = HashMap::new();
            let mut attempts: HashMap<usize, usize> = HashMap::new();
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
                    // Compose ONE query from every resolved neighbour, rather
                    // than picking a method and hoping it narrows.  The
                    // conjunction is contained in each single-neighbour
                    // derivation, so it resolves whenever any of them would --
                    // and where none of them does:
                    //
                    //     "a" { { "b" } }
                    //
                    // `a` has too many children to bound the middle statement
                    // and `b` too many parents; each derivation caps, while
                    // "children of a AND parents of b" is small.  Trying
                    // methods one at a time cannot reach that.
                    //
                    // A capped neighbour is absent by construction: it is not
                    // in `probe_resolved`, and it constrains nothing anyway.
                    // That is the same law that stops us narrowing BY a capped
                    // selection, applied to composing one into a query.
                    //
                    // Cost note: `dc495ef` went the other way, binding only the
                    // smallest, because ANDing a broad neighbour cost 2.7 s and
                    // narrowed nothing.  The trade-off inverted -- failing to
                    // bind used to mean a slow success via the predicate path,
                    // and now means the statement stays capped.
                    use index::db_diesel::ProbeRole;
                    let mut neighbours: Vec<(usize, usize, Vec<ProbeRole>)> = Vec::new();
                    let mut consider = |neighbour: usize,
                                        ids: &Vec<i64>,
                                        rel: RelationshipType,
                                        parent_side: bool| {
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
                        neighbours.push((neighbour, ids.len(), branches));
                    };
                    if let Some(parent) = stmt.parent().and_then(|p| p.upgrade()) {
                        let neighbour = statement_key(&parent);
                        if let Some(ids) = ctx.probe_resolved.get(&neighbour) {
                            consider(neighbour, ids, stmt.relationship_type, true);
                        }
                    }
                    for child in stmt.children() {
                        let neighbour = statement_key(&child);
                        if let Some(ids) = ctx.probe_resolved.get(&neighbour) {
                            consider(neighbour, ids, child.relationship_type, false);
                        }
                    }
                    if neighbours.is_empty() {
                        continue;
                    }

                    // Re-derive only when the INPUTS improved: a neighbour
                    // resolved that had not, or one of them narrowed.  Keyed on
                    // the whole binding set rather than on what notified us,
                    // because with one composed query there is no "which
                    // method" to track -- only whether the composition can now
                    // be tighter than the one already tried.
                    // ONE method, retried only when the new attempt is
                    // strictly MORE RESTRICTIVE than the last one -- either
                    // because it composes a neighbour that was not in it, or
                    // because a neighbour it already had has narrowed.  Both
                    // are the same question, so they are one test rather than
                    // two rules that have to agree.
                    //
                    // Reaching here at all means the statement is still
                    // unresolved, so the previous attempt capped; that is what
                    // licenses widening by one.
                    let neighbour_count = neighbours.len();
                    let level = (attempts.get(&key).copied().unwrap_or(0) + 1).min(neighbour_count);
                    neighbours.sort_by_key(|(_, len, _)| *len);
                    let used: Vec<(usize, usize)> = neighbours
                        .iter()
                        .take(level)
                        .map(|(n, len, _)| (*n, *len))
                        .collect();
                    let more_restrictive = match last_used.get(&key) {
                        None => true,
                        Some(prev) => used.iter().any(|(n, len)| {
                            match prev.iter().find(|(pn, _)| pn == n) {
                                None => true,                  // composed in anew
                                Some((_, plen)) => len < plen, // narrowed
                            }
                        }),
                    };
                    if !more_restrictive {
                        continue;
                    }
                    last_used.insert(key, used);
                    attempts.insert(key, level);

                    // Compose on DEMAND, tightest neighbour first.
                    //
                    // Composition only pays when one neighbour cannot bound the
                    // statement.  When one can -- the common case -- ANDing a
                    // broad neighbour buys no resolving power and costs a great
                    // deal: on the corpus `mod("amdgpu") { func {
                    // "drm_dev_enter" } }` went 400 ms -> 2.7 s when every
                    // neighbour was composed unconditionally, because a probe
                    // that took 3-51 ms with one neighbour took 1240 ms and
                    // 2353 ms with two.  That is `dc495ef`'s 2.7 s, and it does
                    // not go away because the alternative got worse.
                    //
                    // So start with the tightest neighbour alone and widen only
                    // after a level has actually capped.  Cheap queries never
                    // pay for composition; the ones that need it --
                    // `mod("x") { { "spinlock_t" } }`, too many children one way
                    // and too many parents the other -- escalate until they
                    // resolve or the neighbours run out.
                    //
                    // Within a level the probe count is the product of the
                    // per-neighbour branch counts (`{ }` contributes REFS and
                    // HAS), so `MAX_ROLE_BRANCHES` bounds it.
                    let mut branch_sets: Vec<Vec<ProbeRole>> = Vec::new();
                    let mut product = 1usize;
                    for (_, _, branches) in neighbours.into_iter().take(level) {
                        let next = product.saturating_mul(branches.len());
                        if next > MAX_ROLE_BRANCHES {
                            break;
                        }
                        product = next;
                        branch_sets.push(branches);
                    }
                    if branch_sets.is_empty() {
                        continue;
                    }

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
                    for combo in combos {
                        wave.push((stmt.clone(), predicate.clone(), combo));
                    }
                }
                if wave.is_empty() {
                    break;
                }
                let outcomes = run_probe_wave(wave, ctx.probe_cap, ctx.eph.clone()).await?;
                // Deliberately NOT `if !settle(..) { break }`.  A round that
                // resolves nothing is exactly when a wider composition is worth
                // trying -- breaking there is what stopped escalation from ever
                // happening.  Emptiness of the next wave is the real fixpoint:
                // the more-restrictive test refuses to repeat a composition, and
                // levels are bounded by the neighbour count, so the loop ends.
                settle(outcomes, ctx, round);
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
                let (eph, parent_scope, children_scope, demands) = stage_read(ctx, &m.statement);
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
                                demands,
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
    /// One strong component per top-level statement: a constraining
    /// statement is strong, and a non-constraining one is strong iff it
    /// reaches two different constraining statements along two routes that
    /// share no statement but itself (see [`weakness`]).  The graph is the
    /// statement's scope nesting plus its label uses whose provider is in
    /// the same top-level statement; a provider in an earlier statement is
    /// an evaluated input, not a neighbour.  Everything else is weak.
    fn mark_weak_statements(&self, statements: &Vec<Rc<Statement>>) {
        let _span = tracing::debug_span!("mark_weak_statements").entered();
        let index: HashMap<*const Statement, usize> = statements
            .iter()
            .enumerate()
            .map(|(i, s)| (Rc::as_ptr(s), i))
            .collect();
        // The top-level statement a statement belongs to.  The walk stops at
        // the first parent outside `statements`: the query's own root wraps
        // every top-level statement and must not join them into one graph.
        let root_of = |s: &Rc<Statement>| -> *const Statement {
            let mut cur = s.clone();
            while let Some(p) = cur.parent().and_then(|p| p.upgrade()) {
                if !index.contains_key(&Rc::as_ptr(&p)) {
                    break;
                }
                cur = p;
            }
            Rc::as_ptr(&cur)
        };
        let roots: Vec<*const Statement> = statements.iter().map(root_of).collect();

        let terminal: Vec<bool> = statements
            .iter()
            .map(|s| !s.command().is_non_constraining())
            .collect();
        let mut edges = Vec::new();
        for (i, s) in statements.iter().enumerate() {
            if let Some(p) = s.parent().and_then(|p| p.upgrade()) {
                if let Some(&j) = index.get(&Rc::as_ptr(&p)) {
                    edges.push((i, j));
                }
            }
            for dep in s.get_state().dependencies.iter() {
                if dep.dependency_role != DependencyRole::User {
                    continue;
                }
                if let Some(&j) = index.get(&Rc::as_ptr(&dep.dependency)) {
                    if roots[i] == roots[j] {
                        edges.push((i, j));
                    }
                }
            }
        }

        let strong = weakness::strong_vertices(&terminal, &edges);
        for (s, strong) in statements.iter().zip(strong) {
            s.get_state_mut().weak = !strong;
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

    /// Test hook: build the dependency graph and mark weakness, without a
    /// database, and report each statement's command text with its flag.
    #[cfg(test)]
    pub(crate) fn weakness_for_test(&self) -> Vec<(String, bool)> {
        let mut labeled_statements = LabeledStatements::new();
        let mut statements = vec![];
        crate::scope::visit(self.scope(), &mut |statement| {
            statements.push(statement.clone());
            labeled_statements.remember(statement);
            Ok::<bool, pest::error::Error<Rule>>(true)
        })
        .unwrap();
        self.build_dependency_graph(&labeled_statements).unwrap();
        self.mark_weak_statements(&statements);
        statements
            .iter()
            .map(|s| {
                (
                    s.command().span().as_str().trim().to_string(),
                    s.get_state().weak,
                )
            })
            .collect()
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
    /// Returns the reference edges among the answer's nodes and, second, the
    /// SYMBOLS whose implicit out-edge list hit the per-node `edge_cap`
    /// (assembly capping: paginable via fetch-more, never a warning).
    async fn collect_ref_edges(
        statements: &[Rc<Statement>],
        ctx: &ExecutionContext,
        all_nodes: &HashSet<SymbolInstanceId>,
        index: &index::db_diesel::Index,
    ) -> (EdgeList, Vec<SymbolId>, Vec<Diagnostic>) {
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
        // E1/F2: `truncated_instances` names the source instances whose
        // out-edge list hit the per-node cap.  Mapped to SYMBOLS below (the
        // client's fetch-more request is symbol-keyed) and surfaced on the
        // response as `edges_truncated`.
        let mut edges_truncated: Vec<SymbolId> = Vec::new();
        let mut edge_warnings: Vec<Diagnostic> = Vec::new();
        match index.find_edges_between(&all_ids, &ctx.eph).await {
            Err(err) => {
                // An edge-discovery failure used to be swallowed silently,
                // rendering a graph with zero implicit edges and no hint why.
                edge_warnings.push(Diagnostic::truncation(
                    crate::span::Span::synthetic("edges"),
                    format!("edge discovery failed: {err}; reference edges may be missing"),
                ));
            }
            Ok((implicit_edges, truncated_instances)) => {
                for inst_id in truncated_instances {
                    if let Some((sym, _)) = instance_to_node.get(&inst_id) {
                        edges_truncated.push(sym.clone());
                    }
                }
                edges_truncated.sort_unstable_by_key(|s| s.0);
                edges_truncated.dedup();
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
        }

        (all_references, edges_truncated, edge_warnings)
    }

    /// Containment edges among the answer's nodes.
    ///
    /// Asks the index for the containments BETWEEN result nodes rather than
    /// reading each statement's `has_parents`/`has_children` families and
    /// discarding every pair with an endpoint outside the answer. Those
    /// families are sized by the neighbourhood, not the answer: bare
    /// `mod("amdgpu")` walked 178,807 of them to emit **zero** has-edges.
    /// This mirrors what `collect_ref_edges` already does for reference edges
    /// via `find_edges_between`.
    ///
    /// Keeps the TIGHTEST container per child. Ties are real — a module
    /// instance and a file instance both span `[0, EOF)` of the same object —
    /// and the old traversal broke them by statement iteration order, which
    /// was arbitrary but not meaningfully stable. The tie-break is now an
    /// explicit total order (span, then parent instance id) so the same query
    /// gives the same graph.
    async fn collect_has_edges(
        ctx: &ExecutionContext,
        all_nodes: &HashSet<SymbolInstanceId>,
        index: &index::db_diesel::Index,
    ) -> anyhow::Result<HasEdgeList> {
        let _span = tracing::debug_span!("collect_has_edges").entered();

        // Canonical order: `all_nodes` is a HashSet with random iteration
        // order, and these ids become rendered binds in the SQL-cache key.
        let all_ids = canonical_ids(all_nodes.iter().map(|id| Into::<i64>::into(*id)));
        // Propagated, not swallowed: this call is where a layer-isolation
        // breach bails, and returning an empty list there would hand back a
        // plausible answer that merely has no containment in it.
        let pairs = index.find_has_edges_between(&all_ids, &ctx.eph).await?;

        // How many of the answer's nodes each candidate container holds.
        // This is the tie-break: see the fold below.
        let mut children_of: HashMap<i64, usize> = HashMap::new();
        for pair in &pairs {
            let parent_id = SymbolInstanceId::new(pair.parent_instance);
            let child_id = SymbolInstanceId::new(pair.child_instance);
            if all_nodes.contains(&parent_id) && all_nodes.contains(&child_id) {
                *children_of.entry(pair.parent_instance).or_insert(0) += 1;
            }
        }

        // child instance -> (edge, rank) where rank orders containers by how
        // tightly they constrain the child.
        let mut best_per_child: HashMap<SymbolInstanceId, (HasEdge, (i64, usize, i64))> =
            HashMap::new();
        for pair in pairs {
            let parent_id = SymbolInstanceId::new(pair.parent_instance);
            let child_id = SymbolInstanceId::new(pair.child_instance);
            // Binding both ends in SQL is what makes this cheap; this check is
            // what makes it CORRECT. They are deliberately not the same
            // mechanism — a graph must never name a node the answer does not
            // contain, and that must not depend on a query staying right.
            if !all_nodes.contains(&parent_id) || !all_nodes.contains(&child_id) {
                continue;
            }
            let parent_span = range_bounds_to_offsets(&pair.parent_offset_range)
                .map(|(start, end)| (end - start) as i64)
                .unwrap_or(i64::MAX);
            let edge = HasEdge {
                parent: SymbolId::new(pair.parent_symbol),
                child: SymbolId::new(pair.child_symbol),
                parent_instance: parent_id,
                child_instance: child_id,
            };
            // Rank a container by how much it constrains this child:
            //   1. byte extent — a tighter range admits less;
            //   2. how many of the answer's nodes it holds — with equal
            //      extent, the container holding fewer of them pins the child
            //      down more (and is itself held by the roomier ones);
            //   3. instance id, purely so the result is reproducible.
            //
            // Neither the container's TYPE nor its nesting in the query
            // appears here on purpose. At this point there is only a graph;
            // "a file is tighter than a module" is a fact about this answer's
            // containment, not a rule to hardcode — and reading it off the
            // data gets `dir`/`file`/`module` right without knowing what
            // those are.
            let rank = (
                parent_span,
                children_of
                    .get(&pair.parent_instance)
                    .copied()
                    .unwrap_or(usize::MAX),
                pair.parent_instance,
            );
            best_per_child
                .entry(child_id)
                .and_modify(|(best_edge, best_rank)| {
                    if rank < *best_rank {
                        *best_edge = edge.clone();
                        *best_rank = rank;
                    }
                })
                .or_insert((edge, rank));
        }

        let mut result = HasEdgeList::new();
        for (_, (edge, _)) in best_per_child {
            result.add(edge);
        }
        Ok(result)
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
                let limits = statement.command.limits();
                let stmt_info = index::db_diesel::QueryStatementRange {
                    start: span.start(),
                    end: span.end(),
                    text: span.as_pest_span().as_str().to_string(),
                    symbols_limit: limits.symbols,
                    listing_limit: limits.listing,
                };
                for node in &selection.nodes {
                    // Clone only on the vacant path: a node selected by two
                    // `;` statements is the common case, and there only its
                    // provenance is read out of the copy.
                    match node_map.entry(node.symbol_instance.id) {
                        std::collections::hash_map::Entry::Occupied(mut existing) => {
                            existing.get_mut().query_statements.push(stmt_info.clone());
                        }
                        std::collections::hash_map::Entry::Vacant(slot) => {
                            let mut node = node.clone();
                            node.query_statements.push(stmt_info.clone());
                            slot.insert(node);
                        }
                    }
                }
            }
        }

        let all_nodes = HashSet::<SymbolInstanceId>::from_iter(
            node_map.keys().map(|id| SymbolInstanceId::new(*id)),
        );

        let (ref_edges, edges_truncated, edge_warnings) =
            Self::collect_ref_edges(&statements, ctx, &all_nodes, &cfg.index).await;
        warnings.extend(edge_warnings);
        let has_edges = Self::collect_has_edges(ctx, &all_nodes, &cfg.index)
            .await
            .map_err(|e| {
                pest::error::Error::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: e.to_string(),
                    },
                    self.command.query_statement_span().as_pest_span(),
                )
            })?;

        Ok(ExecutionResult::new(
            NodeList(node_map.into_values().collect()),
            ref_edges,
            has_edges,
            warnings,
            edges_truncated,
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

        // The budget law, fenced where both ends of the edge are visible:
        // **truncated rows may only travel where they cannot narrow an
        // independent selection.**  [`edge_consumes_rows`] establishes it by
        // clearing the result budget for every edge that carries rows; this
        // assert catches a change that lets one slip through.
        //
        // Two ways an edge is safe.  The receiver is an echo — it has no
        // selection of its own to lose, it derives from us.  Or the weakness
        // rule already blocks this notification, which is how a truncated
        // flag travels BACK from an echo we just fed: the echo is weak by
        // construction, so it may seed a receiver that has nothing but may
        // never narrow one that has resolved.  That second arm asks
        // [`weak_notifier_blocks`] rather than restating it — a fence that
        // disagreed with the rule it guards would be worse than none.
        //
        // It cannot live in `constrain_selection`, where the rows are actually
        // retained against: that sees a bare `Selection` and has no way to
        // tell an echo from a statement with an independent selection to lose.
        debug_assert!(
            !self.selection_is_budget_bounded(ctx)
                || is_echo(&dependent.statement)
                || weak_notifier_blocks(
                    self.get_state().weak,
                    dependent.statement.has_selection(ctx)
                ),
            "a budget_bounded (truncated) selection reached a data-originating \
             dependent: composition would treat the rows that fit the LIMIT as \
             the whole answer"
        );

        if dependent.dependency_role == DependencyRole::Parent {
            // A child takes part iff it has selectors and is not a
            // display-only weak unit — the membership test is unchanged;
            // only what we do with the members is.  Collect the
            // participants' selections up front so the immutable `ctx` read
            // is over before the mutable `notify_from_selection` calls begin.
            // These are `Rc` HANDLES, not owned copies -- they alias the
            // children's live registry entries.  What keeps the snapshot
            // honest is `Rc::make_mut` in `constrain_by_*`: a constrain that
            // fires while these are held copies first, so no participant
            // observes a mid-loop narrowing.  Mutating a selection through
            // any path that is not copy-on-write would break the conjunction
            // fixpoint here.  Each carries its own weakness, because a weak
            // participant may seed the parent but may not narrow it
            // (`weak_notifier_blocks`).
            let selections: Vec<(bool, Rc<Selection>)> = dependent
                .statement
                .children()
                .filter(|child| child.command().has_selectors() && !is_echo(child))
                .filter_map(|child| {
                    let weak = child.get_state().weak;
                    child.get_selection(ctx).map(|sel| (weak, sel))
                })
                .collect();

            // Nothing to conjoin yet — an unresolved participant simply has
            // not spoken.  It will notify when it resolves, and its conjunct
            // will narrow the parent then.
            if selections.is_empty() {
                return Ok(PropagationResult { changed: false });
            }

            // Use the notifying child's relationship type (all siblings share it).
            let rel_type = self.get_relationship_type();

            let mode = dependent.statement.traversal_mode();
            let parent_scope = build_parent_scope(&dependent.statement, ctx, &ctx.eph);
            let demands = build_family_demands(&dependent.statement);
            // The children scope stays the UNION of the participants' ids.
            // It bounds the neighbourhood PREFETCH, not the semantics: the
            // gate is the per-child constrain below.  Intersecting it would
            // drop the edges to the siblings the parent is displayed with.
            let mut scope_ids: Vec<i64> = selections
                .iter()
                .flat_map(|(_, sel)| sel.get_instance_ids())
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
            for (child_weak, selection) in &selections {
                let res = dependent
                    .statement
                    .command()
                    .notify_from_selection(
                        ctx,
                        &cfg.index,
                        selection,
                        *child_weak,
                        DependencyRole::Parent,
                        rel_type,
                        mode,
                        parent_scope.clone(),
                        children_scope.clone(),
                        demands,
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
            mode: dependent.statement.traversal_mode(),
        };
        let parent_scope = build_parent_scope(&dependent.statement, ctx, &ctx.eph);
        let children_scope = build_children_scope(&dependent.statement, ctx, &ctx.eph);
        let demands = build_family_demands(&dependent.statement);
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
                demands,
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
