use anyhow::Result;
use diesel::connection::SimpleConnection;
use diesel::pg::{Pg, PgConnection};
use diesel::prelude::*;
use diesel::PgRangeExpressionMethods;
use diesel_async::pooled_connection::bb8;
use diesel_async::pooled_connection::{
    AsyncDieselConnectionManager, ManagerConfig, RecyclingMethod,
};
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use diesel_migrations::MigrationHarness;

use crate::models_diesel::{ContentRow, Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::FileId;
use std::future::Future;
use std::pin::Pin;
use std::time::Duration;

/// Boxed future used by [`Index::with_eph_layer`]'s callback so the closure
/// can borrow from the `EphTransaction` across `await` points.
pub type EphScopedFut<'b, R> = Pin<Box<dyn Future<Output = Result<R>> + 'b>>;

use super::cte::{
    build_find_edges_cte_body, build_has_children_cte_body, build_has_children_query,
    build_has_children_query_against_cte, CteFindEdgesBetween, CteHasChildren,
};
use super::mixins::{
    ChildrenQuery, CompositeFilter, CurrentIdQuery, CurrentQuery, EphVisibility, HasParentsQuery,
    ParentsQuery, CONTAINER_INSTANCE_ALIAS, CONTAINER_SYMBOL_ALIAS, CONTAINER_TYPE_ALIAS,
    PARENT_DECLS_ALIAS, PARENT_SYMBOLS_ALIAS,
};
use super::selection::{
    is_eph_leak, ChildReference, EphContext, HasChildReference, HasParentReference,
    ParentReference, RootLayer, Selection, SelectionNode,
};

// ============================================================================
// Scope context — controls parent/children query scoping in find_symbol
// ============================================================================

/// Controls how parent/children queries are scoped in `find_symbol`.
#[derive(Clone)]
pub enum ScopeContext {
    /// Filter by known IDs and/or a composite filter.
    /// - ids only (selected instances) → fast eq_any filter
    /// - filter only (unselected, fallback) → resolve to IDs via CurrentQuery
    /// - both (partial selection) → resolve filter + union with known IDs
    Scope {
        ids: Vec<i64>,
        filter: Option<CompositeFilter>,
    },
    /// No parent/child statement — skip the query entirely.
    Skip,
    /// Run the query without any scope constraint.
    Unscoped,
}

impl ScopeContext {
    /// True for a composition-driven scope (`A { B }`), where the query's rows
    /// feed a Rust-side intersection and so must NOT be budget-bounded.
    /// `Skip`/`Unscoped` are display-only neighbourhoods, safe to bound.
    pub fn is_scope(&self) -> bool {
        matches!(self, ScopeContext::Scope { .. })
    }
}

/// Constrains scope resolution to instances reachable via references.
enum ScopeRole {
    /// Resolved instances must be children of these parent IDs.
    Children(Vec<i64>),
    /// Resolved instances must be parents of these child IDs.
    Parents(Vec<i64>),
}

/// Build a base CurrentQuery (symbols ⋈ instances ⋈ projects ⋈ objects).
/// Applies ephemeral visibility filter: only rows in the visible layer set
/// and rows belonging to the given ephemeral layers are returned.
fn build_current_query(vis: &EphVisibility) -> CurrentQuery<'static> {
    use crate::schema_diesel::*;
    let mut query = symbols::dsl::symbols
        .inner_join(
            symbol_instances::dsl::symbol_instances
                .on(symbols::dsl::id.eq(symbol_instances::dsl::symbol)),
        )
        .inner_join(projects::dsl::projects.on(symbols::dsl::project_id.eq(projects::dsl::id)))
        .inner_join(objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)))
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            Object::as_select(),
            Project::as_select(),
        ))
        .into_boxed::<Pg>();

    // Ephemeral visibility on the selected-row columns, rendered by the
    // partition token (records the columns for the eph-branch guard).
    query = query.filter(vis.pred("symbols.layer"));
    query = query.filter(vis.pred("symbol_instances.layer"));

    query
}

/// [`build_current_query`]'s join tree projected to `symbol_instances.id`
/// — the probe shape (see [`Index::probe_instance_ids`]).
fn build_current_id_query(vis: &EphVisibility) -> CurrentIdQuery<'static> {
    use crate::schema_diesel::*;
    let mut query = symbols::dsl::symbols
        .inner_join(
            symbol_instances::dsl::symbol_instances
                .on(symbols::dsl::id.eq(symbol_instances::dsl::symbol)),
        )
        .inner_join(projects::dsl::projects.on(symbols::dsl::project_id.eq(projects::dsl::id)))
        .inner_join(objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)))
        .select(symbol_instances::dsl::id)
        .into_boxed::<Pg>();

    query = query.filter(vis.pred("symbols.layer"));
    query = query.filter(vis.pred("symbol_instances.layer"));

    query
}

use super::mixins::EphSqlFragment;

/// REFS semi-join: rows whose instance is a CHILD (callee) of any of the
/// given parent instances.  Shared by scope resolution and probes.
fn refs_children_of_expr(
    parent_ids: Vec<i64>,
    vis: &EphVisibility,
) -> EphSqlFragment<diesel::sql_types::Bool> {
    EphSqlFragment::<diesel::sql_types::Bool>::builder()
        .sql(
            "symbol_instances.id IN (\
                SELECT si.id FROM index.symbol_refs sr \
                JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                  AND pd.offset_range @> sr.from_offset_range \
                WHERE ",
        )
        .eph_visibility("sr.layer", vis)
        .sql(" AND ")
        .eph_visibility("si.layer", vis)
        .sql(" AND ")
        .eph_visibility("pd.layer", vis)
        .sql(" AND pd.id = ANY(")
        .bind(parent_ids)
        .sql("))")
        .build()
}

/// REFS semi-join: rows whose instance is a PARENT (caller) of any of the
/// given child instances.  Shared by scope resolution and probes.
fn refs_parents_of_expr(
    child_ids: Vec<i64>,
    vis: &EphVisibility,
) -> EphSqlFragment<diesel::sql_types::Bool> {
    EphSqlFragment::<diesel::sql_types::Bool>::builder()
        .sql(
            "symbol_instances.id IN (\
                SELECT pd.id FROM index.symbol_refs sr \
                JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                  AND pd.offset_range @> sr.from_offset_range \
                JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                WHERE ",
        )
        .eph_visibility("sr.layer", vis)
        .sql(" AND ")
        .eph_visibility("si.layer", vis)
        .sql(" AND ")
        .eph_visibility("pd.layer", vis)
        .sql(" AND si.id = ANY(")
        .bind(child_ids)
        .sql("))")
        .build()
}

/// SYMBOL-level REFS semi-join for probes: rows whose SYMBOL is called
/// from within any of the given parent instances.  Symbol-level because the
/// worklist's constrain accepts evidence per symbol — every instance of an
/// evidenced symbol survives, so probe roles must not drop co-instances.
fn refs_children_of_symbols_expr(
    parent_ids: Vec<i64>,
    vis: &EphVisibility,
) -> EphSqlFragment<diesel::sql_types::Bool> {
    EphSqlFragment::<diesel::sql_types::Bool>::builder()
        .sql(
            "symbols.id IN (\
                SELECT sr.to_symbol FROM index.symbol_refs sr \
                JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                  AND pd.offset_range @> sr.from_offset_range \
                WHERE ",
        )
        .eph_visibility("sr.layer", vis)
        .sql(" AND ")
        .eph_visibility("pd.layer", vis)
        .sql(" AND pd.id = ANY(")
        .bind(parent_ids)
        .sql("))")
        .build()
}

/// SYMBOL-level REFS semi-join for probes: rows whose SYMBOL encloses a
/// call site of any of the given child instances' symbols.
fn refs_parents_of_symbols_expr(
    child_ids: Vec<i64>,
    vis: &EphVisibility,
) -> EphSqlFragment<diesel::sql_types::Bool> {
    EphSqlFragment::<diesel::sql_types::Bool>::builder()
        .sql(
            "symbols.id IN (\
                SELECT pd.symbol FROM index.symbol_refs sr \
                JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                  AND pd.offset_range @> sr.from_offset_range \
                JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                WHERE ",
        )
        .eph_visibility("sr.layer", vis)
        .sql(" AND ")
        .eph_visibility("si.layer", vis)
        .sql(" AND ")
        .eph_visibility("pd.layer", vis)
        .sql(" AND si.id = ANY(")
        .bind(child_ids)
        .sql("))")
        .build()
}

/// Resolve a CompositeFilter to instance IDs by running a CurrentQuery.
///
/// Runs in Combined (legacy-visibility) mode through the SQL cache: the
/// ScopeRole reference-subqueries below consult chain-visible rows, so the
/// persistent/eph partition union would not equal the legacy result.  The
/// query is still cached — chain binds are part of the key.
impl Index {
    async fn resolve_filter_to_ids(
        &self,
        filter: &CompositeFilter,
        role: Option<&ScopeRole>,
        eph: &EphContext,
    ) -> Result<Vec<i64>> {
        let _span =
            tracing::info_span!("resolve_filter_to_ids", has_chain = eph.has_chain()).entered();
        let t0 = std::time::Instant::now();

        let vis = EphVisibility::combined(eph);
        let mut query = build_current_query(&vis);
        if let Some(expr) = filter.compose_current(&vis) {
            query = query.filter(expr);
        }

        // Add reference-based constraint when resolving scoped filters.
        match role {
            Some(ScopeRole::Children(parent_ids)) if !parent_ids.is_empty() => {
                query = query.filter(refs_children_of_expr(parent_ids.clone(), &vis));
            }
            Some(ScopeRole::Parents(child_ids)) if !child_ids.is_empty() => {
                query = query.filter(refs_parents_of_expr(child_ids.clone(), &vis));
            }
            _ => {}
        }

        let results: std::sync::Arc<Vec<(Symbol, SymbolInstance, Object, Project)>> = self
            .cached_load(query)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to resolve filter to IDs: {}", e))?;
        tracing::info!(
            elapsed_ms = t0.elapsed().as_millis() as u64,
            result_rows = results.len(),
            result_bytes = super::sql_cache::vec_weight(results.as_ref()),
            "resolve_filter_to_ids completed",
        );
        let mut ids: Vec<i64> = results.iter().map(|(_, inst, _, _)| inst.id).collect();
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    /// Resolve a Scope's fields into a set of instance IDs for filtering.
    async fn resolve_scope_ids(
        &self,
        ids: &[i64],
        filter: &Option<CompositeFilter>,
        role: Option<&ScopeRole>,
        eph: &EphContext,
    ) -> Result<Vec<i64>> {
        let mut all_ids = ids.to_vec();
        if let Some(ref f) = filter {
            all_ids.extend(self.resolve_filter_to_ids(f, role, eph).await?);
            all_ids.sort_unstable();
            all_ids.dedup();
        }
        Ok(all_ids)
    }

    /// Capped cardinality probe: fetch up to `cap + 1` instance ids
    /// matching `filter ∧ roles` and classify the result.
    ///
    /// This is the dynamic-cost primitive of probe-driven execution: it is
    /// an id-fetch, NOT a `COUNT(*)` — a probe that comes back at or under
    /// the cap already holds the statement's exact current-instance set
    /// ([`ProbeOutcome::Resolved`]) and never needs re-running, while a
    /// capped probe cost at most cap+1 id rows.
    ///
    /// Runs in Combined visibility through the SQL cache, mirroring
    /// [`Index::resolve_filter_to_ids`]: role subqueries consult
    /// chain-visible rows, so the persistent/eph partition union would not
    /// equal the legacy result.  REFS roles are IN-subquery fragments
    /// (shared with scope resolution); HAS roles are emitted as
    /// MATERIALIZED CTEs — the planner mis-drives them as IN-subqueries
    /// (see `has_role_cte_body` and perf/PROBE_BASELINE.md).
    ///
    /// The Resolved id set is complete, so despite the `LIMIT` the outcome
    /// is deterministic: the LIMIT only ever bites in the Capped case,
    /// whose rows are discarded.
    pub async fn probe_instance_ids(
        &self,
        filter: &CompositeFilter,
        roles: Vec<ProbeRole>,
        cap: usize,
        eph: &EphContext,
    ) -> Result<ProbeOutcome> {
        use super::cte::{ProbeCtes, PROBE_ROLE_CTE_NAMES};

        let _span = tracing::info_span!("probe_instance_ids", cap, roles = roles.len()).entered();
        let t0 = std::time::Instant::now();

        let vis = EphVisibility::combined(eph);
        let mut query = build_current_id_query(&vis);
        if let Some(expr) = filter.compose_current(&vis) {
            query = query.filter(expr);
        }

        let mut cte_bodies: Vec<Box<dyn diesel::query_builder::QueryFragment<Pg> + Send>> =
            Vec::new();
        for role in roles {
            match role {
                ProbeRole::RefsChildrenOf(ids) => {
                    query = query.filter(refs_children_of_symbols_expr(ids, &vis));
                }
                ProbeRole::RefsParentsOf(ids) => {
                    query = query.filter(refs_parents_of_symbols_expr(ids, &vis));
                }
                ProbeRole::HasChildrenOf(ids) => {
                    anyhow::ensure!(
                        cte_bodies.len() < PROBE_ROLE_CTE_NAMES.len(),
                        "probe supports at most {} containment roles",
                        PROBE_ROLE_CTE_NAMES.len(),
                    );
                    let name = PROBE_ROLE_CTE_NAMES[cte_bodies.len()];
                    query = query.filter(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                        "symbols.id IN (SELECT symbol FROM {name})"
                    )));
                    cte_bodies.push(has_role_cte_body(ids, true, &vis));
                }
                ProbeRole::HasParentsOf(ids) => {
                    anyhow::ensure!(
                        cte_bodies.len() < PROBE_ROLE_CTE_NAMES.len(),
                        "probe supports at most {} containment roles",
                        PROBE_ROLE_CTE_NAMES.len(),
                    );
                    let name = PROBE_ROLE_CTE_NAMES[cte_bodies.len()];
                    query = query.filter(diesel::dsl::sql::<diesel::sql_types::Bool>(&format!(
                        "symbols.id IN (SELECT symbol FROM {name})"
                    )));
                    cte_bodies.push(has_role_cte_body(ids, false, &vis));
                }
            }
        }

        let query = query.limit(cap as i64 + 1);
        let rows: std::sync::Arc<Vec<i64>> = if cte_bodies.is_empty() {
            self.cached_load(query).await?
        } else {
            self.cached_load(ProbeCtes {
                bodies: cte_bodies,
                outer: query,
            })
            .await?
        };

        let outcome = if rows.len() > cap {
            ProbeOutcome::Capped
        } else {
            let mut ids = rows.as_ref().clone();
            ids.sort_unstable();
            ids.dedup();
            ProbeOutcome::Resolved(ids)
        };
        tracing::info!(
            elapsed_ms = t0.elapsed().as_millis() as u64,
            rows = rows.len(),
            capped = matches!(outcome, ProbeOutcome::Capped),
            "probe completed",
        );
        Ok(outcome)
    }

    /// Resolve a container scope to the `(object_id, lo, hi)` byte-range of
    /// every instance in it.  Content verbs (`search`/`loc`) fuse an enclosing
    /// container into their scan with these: a match is kept iff it lies inside
    /// one of the ranges (same object, `lo <= start_byte < hi`).
    ///
    /// **Degrade-to-unscoped contract:** fusion is a pre-filter, never a
    /// semantic gate — anything this resolve cannot handle cheaply and
    /// faithfully returns `None` (unscoped scan; the composition still
    /// constrains, so results are identical, just unfused):
    /// - `Skip`/`Unscoped` parent — nothing to fuse;
    /// - a condition that composes to no current-query predicate ("no
    ///   constraint" semantics — resolving it would dump the whole index);
    /// - more than [`SCOPE_FUSION_MAX_RANGES`] instances (a broad container:
    ///   inlining tens of thousands of literals would be slower than the
    ///   unscoped scan it replaces);
    /// - any instance with an unbounded `offset_range` (fusion must mirror the
    ///   engine's containment predicate, not guess).
    ///
    /// `Some(vec![])` means the scope genuinely matched nothing => no matches
    /// survive.  Condition-based: it composes the scope's filter directly, so
    /// it never waits for the scope's statement to compute; the
    /// pre-resolved-ids branch is the fast path for an already-computed
    /// parent.  Runs on the caller's connection (populate closures must not
    /// check out a second pooled connection).  `[lo, hi)` follows the stored
    /// `int4range` `[)` convention.
    pub async fn resolve_scope_object_ranges(
        connection: &mut AsyncPgConnection,
        scope: &ScopeContext,
        eph: &EphContext,
    ) -> Result<Option<Vec<(i32, i32, i32)>>> {
        use crate::schema_diesel::*;
        let (ids, filter) = match scope {
            ScopeContext::Scope { ids, filter } => (ids.as_slice(), filter.as_ref()),
            ScopeContext::Skip | ScopeContext::Unscoped => return Ok(None),
        };
        let vis = EphVisibility::combined(eph);
        // Same join shape as `build_current_query` (compose_current predicates
        // target it) but a narrow projection: 12 bytes per row, not 4 models.
        let mut query = symbols::dsl::symbols
            .inner_join(
                symbol_instances::dsl::symbol_instances
                    .on(symbols::dsl::id.eq(symbol_instances::dsl::symbol)),
            )
            .inner_join(projects::dsl::projects.on(symbols::dsl::project_id.eq(projects::dsl::id)))
            .inner_join(
                objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)),
            )
            .select((
                symbol_instances::dsl::object_id,
                symbol_instances::dsl::offset_range,
            ))
            .into_boxed::<Pg>();
        query = query.filter(vis.pred("symbols.layer"));
        query = query.filter(vis.pred("symbol_instances.layer"));
        // The parent is either a not-yet-computed condition (adopt its "ifs")
        // or an already-computed instance-id set — never both, per
        // `build_parent_scope`.  Neither => an empty scope (matched nothing).
        if let Some(f) = filter {
            match f.compose_current(&vis) {
                Some(expr) => query = query.filter(expr),
                // "No constraint" condition: unscoped, NOT everything-resolved.
                None => return Ok(None),
            }
        } else if !ids.is_empty() {
            query = query.filter(symbol_instances::dsl::id.eq_any(ids.to_vec()));
        } else {
            return Ok(Some(Vec::new()));
        }
        let rows: Vec<(i32, (std::ops::Bound<i32>, std::ops::Bound<i32>))> = query
            .limit(SCOPE_FUSION_MAX_RANGES + 1)
            .load(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to resolve scope object ranges: {}", e))?;
        if rows.len() as i64 > SCOPE_FUSION_MAX_RANGES {
            return Ok(None);
        }
        let mut out: Vec<(i32, i32, i32)> = Vec::with_capacity(rows.len());
        for (object_id, range) in &rows {
            match crate::offset_range::range_bounds_to_offsets(range) {
                Some((lo, hi)) => out.push((*object_id, lo, hi)),
                None => return Ok(None),
            }
        }
        out.sort_unstable();
        out.dedup();
        Ok(Some(out))
    }
}

/// Cap on how many container instances scope-fusion will inline into the
/// content-scan SQL.  Above this, fusion degrades to the unscoped scan (see
/// [`Index::resolve_scope_object_ranges`]) — correct either way, and a broad
/// container's literal list would cost more than it saves.  Tunable.
pub const SCOPE_FUSION_MAX_RANGES: i64 = 1024;

// ============================================================================
// Shared query builders — used by both find_symbol and find_*_instance_ids
// ============================================================================

fn build_parents_query(source_ids: Vec<i64>, vis: &EphVisibility) -> ParentsQuery<'static> {
    use crate::schema_diesel::*;
    use diesel::sql_types::Bool;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

    // Redundant by the joins (sr.to_symbol = symbols.id = si.symbol), but
    // it hands the planner the cheap probe route: without it the 5-way
    // join hash-joins a full symbol_refs scan against the source side
    // (measured 933ms for 0 rows from ~900 sources); with it a nested
    // loop drives symbol_refs_to_symbol_idx from the source symbols
    // (10ms).  Semantically a no-op in every scope mode.
    let to_symbol_probe = EphSqlFragment::<Bool>::builder()
        .sql(
            "symbol_refs.to_symbol IN (\
                SELECT si2.symbol FROM index.symbol_instances si2 WHERE ",
        )
        .eph_visibility("si2.layer", vis)
        .sql(" AND si2.id = ANY(")
        .bind(source_ids.clone())
        .sql("))")
        .build();

    symbol_refs::dsl::symbol_refs
        .inner_join(symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::dsl::id)))
        .inner_join(
            symbol_instances::dsl::symbol_instances
                .on(symbols::dsl::id.eq(symbol_instances::dsl::symbol)),
        )
        .inner_join(
            parent_decls.on(parent_decls
                .field(symbol_instances::dsl::object_id)
                .eq(symbol_refs::dsl::from_object)),
        )
        .inner_join(
            parent_symbols.on(parent_symbols
                .field(symbols::dsl::id)
                .eq(parent_decls.field(symbol_instances::dsl::symbol))),
        )
        .filter(
            parent_decls
                .field(symbol_instances::dsl::offset_range)
                .contains_range(symbol_refs::dsl::from_offset_range),
        )
        .filter(symbol_instances::dsl::id.eq_any(source_ids))
        .filter(to_symbol_probe)
        // Ephemeral visibility — canonical and aliased tables, rendered by
        // the partition token (records columns for the eph-branch guard).
        .filter(vis.pred("symbol_refs.layer"))
        .filter(vis.pred("symbols.layer"))
        .filter(vis.pred("symbol_instances.layer"))
        .filter(vis.pred("parent_decls.layer"))
        .filter(vis.pred("parent_symbols.layer"))
        .select((
            SymbolRef::as_select(),
            Symbol::as_select(),
            SymbolInstance::as_select(),
            parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

fn build_children_query(source_ids: Vec<i64>, vis: &EphVisibility) -> ChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

    symbol_refs::dsl::symbol_refs
        .inner_join(symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::id)))
        .inner_join(
            symbol_instances::dsl::symbol_instances
                .on(symbols::dsl::id.eq(symbol_instances::symbol)),
        )
        .inner_join(
            parent_decls.on(parent_decls
                .field(symbol_instances::dsl::object_id)
                .eq(symbol_refs::dsl::from_object)),
        )
        .filter(
            parent_decls
                .field(symbol_instances::dsl::offset_range)
                .contains_range(symbol_refs::dsl::from_offset_range),
        )
        .filter(
            parent_decls
                .field(symbol_instances::dsl::id)
                .eq_any(source_ids),
        )
        .inner_join(
            parent_symbols.on(parent_symbols
                .field(symbols::dsl::id)
                .eq(parent_decls.field(symbol_instances::dsl::symbol))),
        )
        .inner_join(
            objects::dsl::objects
                .on(objects::dsl::id.eq(parent_decls.field(symbol_instances::dsl::object_id))),
        )
        // Ephemeral visibility — canonical and aliased tables, rendered by
        // the partition token (records columns for the eph-branch guard).
        .filter(vis.pred("symbol_refs.layer"))
        .filter(vis.pred("symbols.layer"))
        .filter(vis.pred("symbol_instances.layer"))
        .filter(vis.pred("parent_decls.layer"))
        .filter(vis.pred("parent_symbols.layer"))
        .select((
            parent_symbols.fields(crate::schema_diesel::symbols::all_columns),
            Symbol::as_select(),
            SymbolInstance::as_select(),
            parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
            SymbolRef::as_select(),
            Object::as_select(),
        ))
        .into_boxed::<Pg>()
}

fn build_has_parents_query(source_ids: Vec<i64>, vis: &EphVisibility) -> HasParentsQuery<'static> {
    use crate::schema_diesel::*;

    let container_instance = CONTAINER_INSTANCE_ALIAS;
    let container_symbol = CONTAINER_SYMBOL_ALIAS;
    let container_type = CONTAINER_TYPE_ALIAS;

    symbol_instances::dsl::symbol_instances
        .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
        .inner_join(
            symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)),
        )
        .filter(symbol_instances::dsl::id.eq_any(source_ids))
        .inner_join(
            container_instance.on(container_instance
                .field(symbol_instances::dsl::object_id)
                .eq(symbol_instances::dsl::object_id)),
        )
        .inner_join(
            container_symbol.on(container_symbol
                .field(symbols::dsl::id)
                .eq(container_instance.field(symbol_instances::dsl::symbol))),
        )
        .inner_join(
            container_type.on(container_type
                .field(symbol_types::dsl::id)
                .eq(container_symbol.field(symbols::dsl::symbol_type))),
        )
        .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
            "container_instances.offset_range @> symbol_instances.offset_range",
        ))
        .filter(
            container_type
                .field(symbol_types::dsl::level)
                .ge(symbol_types::dsl::level),
        )
        .filter(
            container_instance
                .field(symbol_instances::dsl::id)
                .ne(symbol_instances::dsl::id),
        )
        // Ephemeral visibility — source and aliased (container) tables,
        // rendered by the partition token.
        .filter(vis.pred("symbols.layer"))
        .filter(vis.pred("symbol_instances.layer"))
        .filter(vis.pred("container_symbols.layer"))
        .filter(vis.pred("container_instances.layer"))
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            container_symbol.fields(crate::schema_diesel::symbols::all_columns),
            container_instance.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

/// Containment join tree shared by the probe-role CTE bodies: canonical
/// `symbol_instances`/`symbols`/`symbol_types` is the CONTAINED side, the
/// `container_*` aliases the containing side (same object, container's
/// offset range covers the row's, container's type level ≥ the row's).
/// `seed_container` picks which side the bound ids constrain; the returned
/// query selects the OTHER side's instance ids.  Emitted only inside a
/// MATERIALIZED CTE (see `ProbeCtes`) — as a plain IN-subquery the planner
/// mis-drives this shape (perf/PROBE_BASELINE.md, S6/S7).
fn has_role_cte_body(
    ids: Vec<i64>,
    seed_container: bool,
    vis: &EphVisibility,
) -> Box<dyn diesel::query_builder::QueryFragment<Pg> + Send> {
    use crate::schema_diesel::*;

    let container_instance = CONTAINER_INSTANCE_ALIAS;
    let container_symbol = CONTAINER_SYMBOL_ALIAS;
    let container_type = CONTAINER_TYPE_ALIAS;

    let base = symbol_instances::dsl::symbol_instances
        .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
        .inner_join(
            symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)),
        )
        .inner_join(
            container_instance.on(container_instance
                .field(symbol_instances::dsl::object_id)
                .eq(symbol_instances::dsl::object_id)),
        )
        .inner_join(
            container_symbol.on(container_symbol
                .field(symbols::dsl::id)
                .eq(container_instance.field(symbol_instances::dsl::symbol))),
        )
        .inner_join(
            container_type.on(container_type
                .field(symbol_types::dsl::id)
                .eq(container_symbol.field(symbols::dsl::symbol_type))),
        )
        .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
            "container_instances.offset_range @> symbol_instances.offset_range",
        ))
        .filter(
            container_type
                .field(symbol_types::dsl::level)
                .ge(symbol_types::dsl::level),
        )
        .filter(
            container_instance
                .field(symbol_instances::dsl::id)
                .ne(symbol_instances::dsl::id),
        )
        // Subquery position: candidate-set membership, not selected rows.
        .filter(vis.subquery_pred("symbol_instances.layer"))
        .filter(vis.subquery_pred("container_instances.layer"));

    if seed_container {
        // HasChildrenOf: containers are bound, contained SYMBOLS come out
        // (symbol-level, mirroring the worklist's per-symbol evidence).
        Box::new(
            base.filter(
                container_instance
                    .field(symbol_instances::dsl::id)
                    .eq_any(ids),
            )
            .select(symbol_instances::dsl::symbol)
            .into_boxed::<Pg>(),
        )
    } else {
        // HasParentsOf: contained rows are bound, container SYMBOLS come out.
        Box::new(
            base.filter(symbol_instances::dsl::id.eq_any(ids))
                .select(container_instance.field(symbol_instances::dsl::symbol))
                .into_boxed::<Pg>(),
        )
    }
}

/// A semi-join constraint a probe binds a resolved neighbour's instance
/// ids into (see [`Index::probe_instance_ids`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeRole {
    /// Probe rows' SYMBOLS must be REFS children (callees) of these
    /// instances.  All roles match at SYMBOL level, mirroring the
    /// worklist's per-symbol evidence: every instance of a matched symbol
    /// is kept, so a probe-resolved set never drops a co-instance the
    /// constrain step would have retained.
    RefsChildrenOf(Vec<i64>),
    /// Probe rows' symbols must be REFS parents (callers) of these instances.
    RefsParentsOf(Vec<i64>),
    /// Probe rows' symbols must be contained in these instances.
    HasChildrenOf(Vec<i64>),
    /// Probe rows' symbols must contain these instances.
    HasParentsOf(Vec<i64>),
}

/// What a capped cardinality probe learned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProbeOutcome {
    /// At most `cap` rows exist; these are ALL of them (sorted, deduped) —
    /// the probe result IS the statement's exact current-instance set.
    Resolved(Vec<i64>),
    /// More than `cap` rows exist; the fetched sample was discarded.
    Capped,
}

#[derive(Clone)]
pub struct Index {
    pub(super) pool: bb8::Pool<AsyncPgConnection>,
    /// Stored for test helpers that need sync DDL connections (migrations, batch_execute).
    database_url: Option<String>,
    /// In-RAM SQL result cache; see [`super::sql_cache`] module docs.  All
    /// clones of an `Index` share one cache (Arc), mirroring the shared pool.
    sql_cache: std::sync::Arc<super::sql_cache::SqlResultCache>,
}

/// Default budget for the SQL result cache (256 MiB) — used by the server
/// (`--sql-cache-bytes` default) and by the test harness, which enables the
/// cache explicitly.  Plain constructors (`from_pool`, `connect`) create a
/// DISABLED cache: correctness of a warm cache depends on the mutation
/// side clearing the SAME instance (see `from_pool_with_cache`), and a
/// silently-enabled private cache in embedder/tooling code would serve
/// stale results after any out-of-band mutation.
pub const DEFAULT_SQL_CACHE_BYTES: usize = 256 * 1024 * 1024;

// ============================================================================
// Ephemeral layer batch types
// ============================================================================

pub struct EphSymbolRow {
    pub name: String,
    pub path: String,
    pub project_id: i32,
    pub symbol_type: i32,
    pub scope: Option<i32>,
    pub leaf_name: String,
}

pub struct EphInstanceRow {
    pub symbol_id: i64,
    pub object_id: i32,
    pub start: i64,
    pub end: i64,
    pub instance_type: i32,
}

pub struct EphRefRow {
    pub to_symbol: i64,
    pub from_object: i32,
    pub start: i64,
    pub end: i64,
}

/// Narrow each i64 offset to i32 for the `int4range` column, failing loudly
/// on overflow rather than silently truncating.
fn try_offsets<I: Iterator<Item = i64>>(iter: I, kind: &'static str) -> Result<Vec<i32>> {
    iter.map(|off| {
        i32::try_from(off).map_err(|_| anyhow::anyhow!("{} offset {} exceeds i32 range", kind, off))
    })
    .collect()
}

/// Rewrite a Diesel error from a batch insert into a user-facing message when
/// it's a foreign-key violation.  The bare Diesel/Postgres message is
/// adequate for operators but useless to a query author who needs to know
/// *which input field* in their `ephemeral_*(...)` verb pointed at a
/// non-existent row.
///
/// `default_prefix` is used both as the fallback message prefix and (lower-
/// cased without "Failed to batch insert eph " noise) to label the verb that
/// triggered the insert.
fn explain_eph_insert_err(
    default_prefix: &'static str,
    err: diesel::result::Error,
) -> anyhow::Error {
    use diesel::result::{DatabaseErrorKind, Error as E};
    if let E::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, info) = &err {
        let constraint = info.constraint_name().unwrap_or("");
        // Constraint names come in two generations: the historical ones
        // (Postgres RENAME preserves constraint names, so live DBs keep the
        // eph_layer-era names) and the ones a regenerated/squashed schema
        // would auto-derive from the current column names.  Match both so
        // the diagnostics survive a future baseline regeneration.
        let field = match constraint {
            "symbol_instances_symbol_fkey" => Some(("ephemeral_instance", "symbol_id")),
            "symbol_instances_object_id_fkey" => Some(("ephemeral_instance", "object_id")),
            "symbol_instances_eph_layer_fkey" | "symbol_instances_layer_fkey" => {
                Some(("ephemeral_instance", "layer"))
            }
            "symbol_refs_to_symbol_fkey" => Some(("ephemeral_ref", "to_symbol")),
            "symbol_refs_eph_layer_fkey" | "symbol_refs_layer_fkey" => {
                Some(("ephemeral_ref", "layer"))
            }
            "symbols_eph_layer_fkey" | "symbols_layer_fkey" => Some(("ephemeral_symbol", "layer")),
            _ => None,
        };
        if let Some((verb, fname)) = field {
            return anyhow::anyhow!(
                "{}: '{}' refers to a row that does not exist \
                 (or is in a different layer than this insert can see). \
                 Postgres constraint: {}",
                verb,
                fname,
                constraint
            );
        }
    }
    anyhow::anyhow!("{}: {}", default_prefix, err)
}

/// Drop every non-canary ephemeral layer.  Run this on a connection that
/// is already inside the write transaction that mutated the persistent
/// index, so the cache purge and the mutation commit (or roll back)
/// atomically together.
///
/// `ON DELETE CASCADE` on the `layer` FK cleans up dependent
/// `symbols`, `symbol_instances`, and `symbol_refs` rows.
///
/// Call this any time `index.objects` / `index.symbols` /
/// `index.symbol_*` gain or lose persistent rows; otherwise
/// input-only-keyed lookups (`loc(path, line)`, `layer { … }`) will
/// keep returning rows derived from the pre-mutation state of the
/// index.
///
/// ## Why the LOCK TABLE
///
/// Layer cache entries are keyed on inputs only, so a stale entry can
/// never be evicted by a key change — the purge is the only invalidation.
/// Under READ COMMITTED a plain DELETE cannot see an *in-flight* layer
/// transaction's uncommitted `layers` row: the layer would commit
/// AFTER the purge with content computed from pre-mutation data and then
/// be served forever.  `LOCK TABLE … IN EXCLUSIVE MODE` closes the race
/// in both directions: the lock request queues behind every open layer
/// transaction's `ROW EXCLUSIVE` (their upsert), so in-flight populates
/// commit first and their rows become visible to the DELETE; and new
/// layer creations block at `create_eph_layer`'s INSERT until the purging
/// transaction commits, so post-purge populates read only post-mutation
/// data.  Reads (`SELECT`) are unaffected.  No deadlock cycle: layer
/// transactions only ever lock `layers` rows and their own batch
/// tables, never `projects`, so lock ordering is one-directional.
pub async fn purge_eph_cache(conn: &mut AsyncPgConnection) -> Result<usize, diesel::result::Error> {
    use crate::schema_diesel::layers;
    use diesel_async::RunQueryDsl;
    diesel::sql_query("LOCK TABLE index.layers IN EXCLUSIVE MODE")
        .execute(&mut *conn)
        .await?;
    diesel::delete(
        layers::table.filter(layers::kind.ne_all(EphLayerKind::PROTECTED.map(|k| k.as_str()))),
    )
    .execute(conn)
    .await
}

/// Build the doomed-closure DELETE for targeted eph-layer garbage
/// collection (`purge_old_eph_layers`, `delete_eph_layer`).  `seed_where`
/// selects the initially-doomed rows and is the only difference between the
/// two variants ($1 is the caller's bind).
///
/// ## Why the closure, not just FK cascades
///
/// `symbol_instances.symbol` / `symbol_refs.to_symbol` cascade to
/// `symbols.id`, and layers reference symbols ACROSS layer boundaries by
/// design (a selection shard's rows point into upstream layers).  A plain
/// `DELETE FROM layers WHERE <seed>` therefore cascade-deletes rows out
/// of OTHER still-cached layers while leaving their `layers` rows
/// `populated = true` — hollow entries whose keys never change, served as
/// valid cache hits with silently missing results.  The recursive CTE
/// computes, BEFORE anything cascades, every layer transitively reachable
/// from the seed through four dependency edges — instance→symbol references,
/// ref→symbol references, chain parentage (`parent_id`), and shard coupling
/// (`root_shard_id`) — and deletes the whole closure atomically.  A layer is
/// either fully alive or gone; hollow-but-populated is unconstructible
/// through GC.  (Postgres allows one recursive self-reference, hence the
/// edge-list UNION inside a single recursive term.)
///
/// Root layers and the canary are never garbage: both are excluded from the
/// seed, the data-row edges are restricted to the negative (ephemeral) id
/// space so a root can never appear as a dependent, and the final DELETE
/// excludes them again as defense in depth — a root's death would cascade a
/// project's whole persistent index away.  (The `projects.root_layer_id` FK
/// additionally hard-blocks deleting a live project's root.)
fn doomed_closure_delete_sql(seed_where: &str) -> String {
    format!(
        "WITH RECURSIVE doomed AS ( \
             SELECT id FROM index.layers \
             WHERE ({seed}) AND kind NOT IN ({protected}) \
           UNION \
             SELECT e.dependent FROM ( \
                 SELECT si.layer AS dependent, s.layer AS needed \
                   FROM index.symbol_instances si \
                   JOIN index.symbols s ON si.symbol = s.id \
                  WHERE si.layer < 0 AND s.layer < 0 \
                    AND si.layer <> s.layer \
               UNION ALL \
                 SELECT sr.layer, s.layer \
                   FROM index.symbol_refs sr \
                   JOIN index.symbols s ON sr.to_symbol = s.id \
                  WHERE sr.layer < 0 AND s.layer < 0 \
                    AND sr.layer <> s.layer \
               UNION ALL \
                 SELECT id, parent_id FROM index.layers \
                  WHERE parent_id IS NOT NULL \
               UNION ALL \
                 SELECT id, root_shard_id FROM index.layers \
                  WHERE root_shard_id IS NOT NULL \
             ) e JOIN doomed d ON e.needed = d.id \
         ) \
         DELETE FROM index.layers \
         WHERE id IN (SELECT id FROM doomed) \
           AND kind NOT IN ({protected})",
        seed = seed_where,
        protected = EphLayerKind::protected_sql_list(),
    )
}

/// Batch of ephemeral rows to insert into a single layer.
pub struct LayerBatch {
    pub symbols: Vec<EphSymbolRow>,
    pub instances: Vec<EphInstanceRow>,
    pub refs: Vec<EphRefRow>,
}

impl LayerBatch {
    pub fn new() -> Self {
        Self {
            symbols: Vec::new(),
            instances: Vec::new(),
            refs: Vec::new(),
        }
    }
}

/// The kind stored in the `layers.kind` column.
///
/// Coarse by design: a layer's kind records only what drives behaviour —
/// whether GC may touch it — not which verb produced it. The producing verb
/// lives in the layer's hash (each verb folds its own domain tag) and the
/// layer's role lives in `parent_id` + the executor's `ShardRole`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EphLayerKind {
    /// The leak-detection sentinel layer.  Never created at runtime; inserted
    /// once by migration and preserved across `purge_eph_cache` /
    /// `purge_old_eph_layers`.  Loaded by `kind='canary'`.
    Canary,
    /// Any query-time ephemeral cache layer — a root shard, a `layer { … }`
    /// block, a composite, a selection shard, or a layer shard.  All are
    /// GC-able; none of these are distinguished by kind (their identity and
    /// role live in the hash, `parent_id`, and `ShardRole`).
    Ephemeral,
    /// A project's root layer: the persistent index data itself.  One per
    /// project via `projects.root_layer_id`, created with the project — never
    /// through the hash-upsert cache path, never garbage-collected.
    Root,
}

impl EphLayerKind {
    /// Layer kinds that garbage collection must NEVER touch: the canary
    /// sentinel and project root layers (a root's death would cascade a
    /// project's whole persistent index away).  Single source for every
    /// purge/TTL/closure exclusion — a future protected kind is added here
    /// and nowhere else.
    pub const PROTECTED: [EphLayerKind; 2] = [EphLayerKind::Canary, EphLayerKind::Root];

    /// The protected kinds as a SQL list literal for `kind NOT IN (...)`
    /// exclusions in raw queries (values come from [`Self::as_str`], not
    /// user input).
    pub(crate) fn protected_sql_list() -> String {
        Self::PROTECTED
            .iter()
            .map(|k| format!("'{}'", k.as_str()))
            .collect::<Vec<_>>()
            .join(", ")
    }

    /// String form stored in `layers.kind`.  NOT folded into any cache hash:
    /// each verb folds its own explicit domain tag in its `layer_spec`, so the
    /// kind can stay coarse without weakening key disjointness.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Canary => "canary",
            Self::Ephemeral => "ephemeral",
            Self::Root => "root",
        }
    }
}

impl std::fmt::Display for EphLayerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity of a materialized root shard, handed to a selection shard's
/// populate closure by [`Index::with_partitioned_layers`].  Unused today;
/// future mask/tombstone rows will need to reference the root shard they
/// mask.  Note the two fields age differently: `hash` is stable across TTL
/// purge + recreate of the root shard's row, `layer_id` is not.
#[derive(Debug, Clone, Copy)]
pub struct RootShardRef {
    pub layer_id: i64,
    pub hash: [u8; 32],
}

/// Outcome of materializing one `layers` row through the cache gate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct LayerOutcome {
    pub layer_id: i64,
    /// `true` iff this call created (and populated) the row; `false` on a
    /// cache hit.
    pub created: bool,
    /// The row's persisted `truncated` flag (fresh on miss, the original
    /// creator's value on hit).
    pub truncated: bool,
}

/// Which shard of a root's cache entry a [`MaterialisedLayer`] is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ShardRole {
    /// Root shard: the root-parented shard of a partitioned entry — keyed on
    /// (root identity, verb inputs), reused under any upstream eph context.
    Root,
    /// Selection shard: the eph-chained delta shard of a partitioned entry;
    /// ordered last within its root, so it is what downstream verbs chain on.
    Selection,
    /// Layer shard `V(L_j)`: a content verb's contribution restricted to ONE
    /// ephemeral content layer, keyed `layer_shard_hash(L_j, input_hash,
    /// extra)` and parented on `L_j` (its lifetime tracks the layer it
    /// shards).  Additive to the root and selection shards; zero of these
    /// exist until an ephemeral layer carries content.
    Layer,
}

/// One layer materialised by [`Index::with_partitioned_layers`], in the
/// exact order the executor records them (and concatenates them into its
/// tree's materialisation): roots ascending; within a root, the root shard
/// first, then
/// zero or more N-way layer shards (`ShardRole::Layer`, one per visible eph
/// content layer, none until eph content exists), then the selection shard
/// LAST — which exists iff that root's upstream (pre-tree) chain was
/// non-empty (a chain-topology rule, not an overlay-emptiness shortcut:
/// under a non-empty chain the selection shard's row is always materialized,
/// even when its populate inserts zero rows).  The selection shard is last so
/// a command's final layer per root — and with it the tree tip, which is the
/// LAST command's final layer — is always a selection shard (or a root shard
/// under first-materialisation elision), never a nondeterministically-ordered
/// layer
/// shard — keeping downstream `selection_shard_hash` keys stable when layer
/// shards churn.
#[derive(Debug, Clone, Copy)]
pub struct MaterialisedLayer {
    /// The root whose chain this layer joins.
    pub root_id: i64,
    pub role: ShardRole,
    pub outcome: LayerOutcome,
}

/// Cache key of a selection shard: parent chain identity + root shard
/// identity + the verb's own selection-shard inputs (`extra`).
///
/// `extra` is whatever the selection shard's populate reads BEYOND the chain
/// and the root shard — empty for verbs whose delta is fully determined by
/// (parent, root shard) (search, loc), a hash of the eph-referencing ops for
/// `layer { … }` blocks.  Without it, two blocks sharing root-shard content
/// under the same chain but differing in their eph-referencing ops would
/// collide on the selection-shard key.  Length-prefixed to keep the encoding
/// injective.
///
/// Folds the parent layer *id*, not its hash — consistent with how verbs
/// chain today (`hash.update(eph.last())`); ids are stable for the lifetime
/// of the row, which is exactly the lifetime of the cache entry.  The domain
/// tag keeps selection-shard hashes disjoint from every verb-computed hash.
pub fn selection_shard_hash(parent_id: i64, input_hash: &[u8; 32], extra: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"selection-shard-v1");
    h.update(parent_id.to_le_bytes());
    h.update(input_hash);
    h.update((extra.len() as u64).to_le_bytes());
    h.update(extra);
    h.finalize().into()
}

/// Cache key of an N-way layer shard `V(L_j)`: the sharded layer's id folded
/// with the (already root-salted) root-shard identity.  Unlike
/// [`selection_shard_hash`] (which folds the runtime chain tip `tip`,
/// so the fused delta is bound to one chain), this folds only `layer_id` — so
/// for a single content verb the layer-shard key is a function of
/// `(L_j, verb inputs)`, is reused by ANY chain containing `L_j`, and adding a
/// new tip re-materialises only the tip's layer shards.  It deliberately
/// carries **no** extra key material: a content scan reads nothing beyond its
/// layer and its own inputs, so folding a selection shard's `extra` here would
/// only fragment the cache (the source of an earlier over-keying bug).  Caveat
/// for composites: the `input_hash` passed in is the *composite* input hash,
/// which folds every part's inputs — so a `search() layer{}` statement's layer
/// shards key on the whole statement, i.e. reuse across composites is
/// conservative; standalone verbs are keyed tightly.  Its own domain tag keeps
/// layer-shard keys disjoint from selection-shard and verb-computed hashes;
/// old keys age out via TTL (no purge on upgrade).
pub fn layer_shard_hash(layer_id: i64, input_hash: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"layer-shard-v1");
    h.update(layer_id.to_le_bytes());
    h.update(input_hash);
    h.finalize().into()
}

/// Fold ONE root layer's identity hash into a verb's input hash: the cache
/// key of that root's root shard under per-root materialisation.  Identical
/// verb inputs against different roots key different root shards, and — the
/// headline property — a root's shard key is independent of which other
/// roots are co-visible, so cached root shards are reused across visibility
/// sets.  The domain tag makes all keys disjoint from the pre-per-root
/// multi-root fold, so no cache purge is needed on upgrade: stale rows
/// simply never hit again and age out via TTL.
///
/// Invariant preserved: the result is a function of (root identity, verb
/// inputs) only — never ephemeral chain state.
pub fn root_shard_hash(root: &super::selection::RootLayer, input_hash: &[u8; 32]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"root-shard-v1");
    h.update((root.hash.len() as u64).to_le_bytes());
    h.update(&root.hash);
    h.update(input_hash);
    h.finalize().into()
}

/// Metadata of one `layers` row, as read by [`Index::eph_layer_meta`].
/// Exposes the cache-state flags (`populated`, `truncated`) so tests can
/// assert them directly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EphLayerMeta {
    pub id: i64,
    pub parent_id: Option<i64>,
    pub kind: String,
    pub populated: bool,
    pub truncated: bool,
}

/// SQL the bb8 pool must run on every checkout.  `EphTransaction::Drop` is
/// synchronous (cannot issue an async `ROLLBACK` itself); cleanup of a
/// transaction left open by a cancelled / `?`-returned future depends
/// entirely on this query running before the connection is handed to the
/// next caller.  Bb8's `RecyclingMethod::Fast` (the default) does *not*
/// run cleanup; `CustomQuery(EPH_POOL_RECYCLING_QUERY.into())` does.
///
/// All three pool-builder sites — `Index::build_async_manager` (this
/// crate) and the two pools in `askld/src/bin/askld/server.rs` — must
/// use [`eph_pool_manager_config`] (which applies this constant).
/// Changing the value or the manager config without updating *all* sites
/// silently disarms ephemeral-layer cancellation safety; see the rustdoc
/// on [`Index::with_eph_layer`] for the load-bearing contract.
pub const EPH_POOL_RECYCLING_QUERY: &str = "ROLLBACK";

/// Server-side backstop for abandoned layer transactions.  The recycling
/// ROLLBACK above only runs when the abandoned connection is next checked
/// out — until then the connection sits idle in bb8 with its transaction
/// open and the `layers` row lock held, blocking every concurrent
/// identical query.  Deferred populates made this window as long as the
/// whole content scan, so cap it: Postgres kills a session idle inside a
/// transaction for longer than this, releasing the lock.  Legitimate
/// populates are *active* (running SQL) during their long stretches; their
/// idle-in-transaction gaps between statements are milliseconds, so 60s is
/// generous.  Convoying of concurrent identical cold queries behind the
/// row lock is intentional (it dedups the expensive work) — size the pool
/// with that in mind.
pub const EPH_POOL_IDLE_IN_TXN_TIMEOUT: &str = "60s";

/// The one true `ManagerConfig` for pools that touch ephemeral layers:
/// checkout-recycling ROLLBACK plus the idle-in-transaction session
/// timeout.  Every pool-builder site must use this instead of hand-rolling
/// a config, so the sites cannot drift apart.
pub fn eph_pool_manager_config() -> ManagerConfig<AsyncPgConnection> {
    use diesel_async::AsyncConnection;
    use futures::FutureExt;
    let mut config = ManagerConfig::default();
    config.recycling_method = RecyclingMethod::CustomQuery(EPH_POOL_RECYCLING_QUERY.into());
    config.custom_setup = Box::new(|url| {
        async move {
            let mut conn = AsyncPgConnection::establish(url).await?;
            diesel::sql_query(format!(
                "SET idle_in_transaction_session_timeout = '{}'",
                EPH_POOL_IDLE_IN_TXN_TIMEOUT
            ))
            .execute(&mut conn)
            .await
            .map_err(diesel::ConnectionError::CouldntSetupConfiguration)?;
            Ok(conn)
        }
        .boxed()
    });
    config
}

impl Index {
    /// Plain constructor: SQL result cache DISABLED (see
    /// [`DEFAULT_SQL_CACHE_BYTES`] docs for why).  Enable via
    /// [`Index::with_sql_cache`] or construct through
    /// [`Index::from_pool_with_cache`] with an instance shared with the
    /// mutation side.
    pub fn from_pool(pool: bb8::Pool<AsyncPgConnection>) -> Self {
        Self::from_pool_with_cache(pool, super::sql_cache::SqlResultCache::new(0))
    }

    /// Builder-style override of the SQL result cache (used by the test
    /// harness to run the suite cache-ON with per-fixture instances).
    pub fn with_sql_cache(
        mut self,
        sql_cache: std::sync::Arc<super::sql_cache::SqlResultCache>,
    ) -> Self {
        self.sql_cache = sql_cache;
        self
    }

    /// Construct with an explicit (possibly shared) SQL result cache — the
    /// server hands the same instance to `IndexStore` so the mutation side
    /// clears the cache the query side reads.
    pub fn from_pool_with_cache(
        pool: bb8::Pool<AsyncPgConnection>,
        sql_cache: std::sync::Arc<super::sql_cache::SqlResultCache>,
    ) -> Self {
        Self {
            pool,
            database_url: None,
            sql_cache,
        }
    }

    pub fn sql_cache(&self) -> &std::sync::Arc<super::sql_cache::SqlResultCache> {
        &self.sql_cache
    }

    /// Execute a read query through the SQL result cache — the single entry
    /// point ("proxy") for cacheable reads.  The key is derived from the
    /// query's rendered SQL + binds, so any query routed through here is
    /// keyed on exactly what Postgres would see; a future query cannot be
    /// under-keyed by forgetting a parameter.
    ///
    /// Order matters: a cache hit never touches the connection pool; the
    /// cache epoch is snapshotted BEFORE the DB read so a concurrent
    /// [`SqlResultCache::clear`] (index mutation committing) rejects this
    /// load's insert.  A query that fails to render degrades to a plain
    /// uncached load through the same path.
    ///
    /// NOT for populate closures: those run inside an [`EphTransaction`] on
    /// `txn.connection()` and must see uncommitted state — keeping them off
    /// `Index` methods is the structural opt-out.
    pub async fn cached_load<Q, T>(&self, query: Q) -> Result<std::sync::Arc<Vec<T>>>
    where
        Q: diesel_async::methods::LoadQuery<'static, AsyncPgConnection, T>
            + diesel::query_builder::QueryFragment<Pg>
            + 'static,
        T: Clone + Send + Sync + super::sql_cache::CacheWeight + 'static,
    {
        use super::sql_cache::{vec_weight, CacheKey};

        let key = CacheKey::for_query::<T, _>(&query);
        if let Some(k) = &key {
            if let Some(hit) = self.sql_cache.get::<T>(k) {
                return Ok(hit);
            }
        }

        let epoch = self.sql_cache.epoch();
        let mut connection = self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        let rows: Vec<T> = query
            .load(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load cached query: {}", e))?;
        drop(connection);

        let arc = std::sync::Arc::new(rows);
        if let Some(k) = key {
            let bytes = vec_weight(&arc);
            self.sql_cache.put_if_epoch(k, arc.clone(), bytes, epoch);
        }
        Ok(arc)
    }

    /// The ONLY way to run an eph-visible read on the query hot path.
    ///
    /// Instantiates the query template `build` per branch via the
    /// [`EphVisibility`] capability token:
    ///
    /// - `chain_dependent = false`: a PERSISTENT instantiation (root-only:
    ///   every eph column `= ANY($roots)` — its cache key carries the
    ///   root-id binds, deliberately root-scoped, but stays chain-free, so
    ///   the expensive branch is shared across all upstream ephemeral
    ///   contexts under the same visible root set) plus an EPH
    ///   instantiation (roots ∪ chain visibility + the sign-form
    ///   disjointness guard, chain-keyed).  Results merge with mandatory
    ///   row-identity dedup: node consumers are duplicate-sensitive, so
    ///   dedup is a correctness requirement, which also makes a missing
    ///   guard benign (wasted cache bytes, never wrong results).
    /// - `chain_dependent = true` (the filter tree contains subquery-level
    ///   chain dependence, [`CompositeFilter::is_chain_dependent`]): the
    ///   partition union would not equal the legacy result, so ONE Combined
    ///   legacy-visibility query runs instead — still cached, chain-keyed.
    ///
    /// The builder cannot skip the split or leak the chain: tokens are only
    /// minted here, and the chain ids live inside the token.
    ///
    /// ## Accepted limitations (documented, by design decision)
    ///
    /// - **Torn reads under concurrent mutation**: the two branch loads
    ///   are separate statements (and independent cache states); a
    ///   mutation committing between them can merge pre- and
    ///   post-mutation rows in one result.  Legacy already had this
    ///   window BETWEEN query families (READ COMMITTED, one statement
    ///   each); the intra-family window is new and accepted for a
    ///   single-user tool — the epoch check prevents such reads from
    ///   being cached, not from being returned once.
    /// - **Sharing scope**: the persistent branch's key contains ALL
    ///   binds.  Statements whose inputs embed chain-derived ids
    ///   (negative instance ids from an upstream eph selection) therefore
    ///   do not share across chains — cross-chain sharing applies to
    ///   selections whose inputs are chain-free (typically the first
    ///   statement of a pipeline).
    /// - **Combined fallback**: direct/innermost/outer-parent filters are
    ///   chain-dependent and demote to one Combined query.  With an empty
    ///   chain those keys are still stable and shared; only cross-chain
    ///   sharing for eph workloads is lost.  Partitioning those
    ///   subqueries is a possible follow-up.
    pub async fn cached_load_partitioned<Q, T, F>(
        &self,
        eph: &EphContext,
        chain_dependent: bool,
        build: F,
    ) -> Result<Vec<T>>
    where
        F: Fn(&EphVisibility) -> Q,
        Q: diesel_async::methods::LoadQuery<'static, AsyncPgConnection, T>
            + diesel::query_builder::QueryFragment<Pg>
            + 'static,
        T: Clone + Send + Sync + super::sql_cache::CacheWeight + super::sql_cache::RowKey + 'static,
    {
        if chain_dependent {
            let vis = EphVisibility::combined(eph);
            let rows = self.cached_load(build(&vis)).await?;
            return Ok(rows.as_ref().clone());
        }

        // Loader-level algebraic short-circuit: with an EMPTY chain the
        // eph branch is `visibility AND guard` = `col = ANY($roots) AND
        // NOT(all cols = ANY($roots))` — a contradiction, so it provably
        // returns zero rows.  This is a uniform rule of the cache layer
        // (branch algebra), not per-verb overlay peeking; it saves one
        // guaranteed-empty roundtrip per family on every plain query plus
        // the dead cache entry it would store.
        if !eph.has_chain() {
            let persistent = self
                .cached_load(build(&EphVisibility::root_only(eph)))
                .await?;
            return Ok(persistent.as_ref().clone());
        }

        let evis = EphVisibility::eph_touching(eph);
        let eph_query = build(&evis);
        debug_assert!(
            evis.guard_was_taken(),
            "eph-branch query built without applying vis.guard()"
        );
        // The branches are independent (separate pool connections on
        // misses); run them concurrently.
        let (persistent, ephemeral) = tokio::try_join!(
            self.cached_load(build(&EphVisibility::root_only(eph))),
            self.cached_load(eph_query),
        )?;

        let mut seen = std::collections::HashSet::with_capacity(persistent.len() + ephemeral.len());
        let mut merged = Vec::with_capacity(persistent.len() + ephemeral.len());
        for row in persistent.iter().chain(ephemeral.iter()) {
            if seen.insert(row.row_key()) {
                merged.push(row.clone());
            }
        }
        Ok(merged)
    }

    /// Load the root layers of all (non-canary) projects — id plus identity
    /// hash — through the SQL result cache, so per-request resolution is a
    /// RAM hit and the same epoch clear that index mutations already perform
    /// invalidates it.  Ids feed visibility binds via [`EphContext::rooted`];
    /// hashes feed the root-shard salt (see [`root_shard_hash`]).
    pub async fn load_root_layers(&self) -> Result<Vec<RootLayer>> {
        use crate::schema_diesel::{layers, projects};

        let rows: std::sync::Arc<Vec<(i64, i32, Vec<u8>)>> = self
            .cached_load(
                projects::table
                    .inner_join(layers::table.on(layers::id.eq(projects::root_layer_id)))
                    .filter(projects::id.gt(0))
                    .order(projects::root_layer_id.asc())
                    .select((projects::root_layer_id, projects::id, layers::hash)),
            )
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load root layers: {}", e))?;

        Ok(rows
            .iter()
            .map(|(id, project_id, hash)| RootLayer {
                id: *id,
                project_id: *project_id,
                hash: hash.clone(),
            })
            .collect())
    }

    /// Map object ids to their owning projects.  Used by `layer { … }`
    /// validation to reject references the scoped inserts would otherwise
    /// silently drop (the per-root insert SQL filters rows by project, so a
    /// typo'd object id would commit hollow, input-keyed — i.e. sticky —
    /// cache layers with no diagnostic).
    pub async fn load_object_projects(
        &self,
        object_ids: &[i32],
    ) -> Result<std::collections::HashMap<i32, i32>> {
        use crate::schema_diesel::objects;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let rows: Vec<(i32, i32)> = objects::table
            .filter(objects::id.eq_any(object_ids))
            .select((objects::id, objects::project_id))
            .load(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to load object projects: {}", e))?;

        Ok(rows.into_iter().collect())
    }

    fn build_async_manager(database_url: &str) -> AsyncDieselConnectionManager<AsyncPgConnection> {
        AsyncDieselConnectionManager::new_with_config(database_url, eph_pool_manager_config())
    }

    pub async fn connect(database_url: &str) -> Result<Self> {
        // Run migrations with a sync connection
        {
            let connection = &mut <PgConnection as diesel::Connection>::establish(database_url)
                .map_err(|e| anyhow::anyhow!("Failed to establish connection: {}", e))?;
            connection
                .run_pending_migrations(super::MIGRATIONS)
                .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;
        }

        let manager = Self::build_async_manager(database_url);
        let pool = bb8::Pool::builder()
            .test_on_check_out(false)
            .build(manager)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create connection pool: {}", e))?;
        let index = Self {
            pool,
            database_url: Some(database_url.to_string()),
            sql_cache: super::sql_cache::SqlResultCache::new(0),
        };
        index.validate_canary().await?;
        Ok(index)
    }

    /// Confirm the canary row exists.  The canary is load-bearing for the
    /// leak-detection defence; running without it silently disables
    /// `Selection::has_eph_leak` / `Checked::new` as detectors (they keep
    /// running, just have nothing to catch).  Call this after migrations
    /// have run and the pool is ready.
    pub async fn validate_canary(&self) -> Result<()> {
        use crate::schema_diesel::layers;
        use diesel::dsl::count_star;
        use diesel_async::RunQueryDsl;
        let mut connection = self.pool.get().await.map_err(|e| {
            anyhow::anyhow!("Failed to get connection for canary validation: {}", e)
        })?;
        let c: i64 = layers::table
            .filter(layers::id.eq(super::selection::CANARY_LAYER_ID))
            .filter(layers::kind.eq(EphLayerKind::Canary.as_str()))
            .select(count_star())
            .get_result(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to validate canary row: {}", e))?;
        if c != 1 {
            anyhow::bail!(
                "canary row missing from index.layers (kind='{}', id={}); \
                 leak detection is not armed. Re-apply the layers migration.",
                EphLayerKind::Canary.as_str(),
                super::selection::CANARY_LAYER_ID,
            );
        }
        Ok(())
    }

    /// Connect and load test data using a direct connection for DDL work,
    /// then create a fresh pool. This avoids stale prepared statement caches
    /// that occur when DDL (revert+rerun migrations) happens on pooled connections.
    pub async fn connect_with_test_input(database_url: &str, input_path: &str) -> Result<Self> {
        // Use a direct connection for all DDL + data loading so the pool
        // never sees stale prepared statements.
        {
            let connection = &mut <PgConnection as diesel::Connection>::establish(database_url)
                .map_err(|e| anyhow::anyhow!("Failed to establish connection: {}", e))?;

            connection
                .revert_all_migrations(super::MIGRATIONS)
                .map_err(|e| anyhow::anyhow!("Failed to revert migrations: {}", e))?;
            connection
                .run_pending_migrations(super::MIGRATIONS)
                .map_err(|e| anyhow::anyhow!("Failed to run migrations: {}", e))?;

            Self::load_sql(connection, input_path);
        }

        let manager = Self::build_async_manager(database_url);
        let pool = bb8::Pool::builder()
            .test_on_check_out(false)
            .build(manager)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create connection pool: {}", e))?;
        Ok(Self {
            pool,
            database_url: Some(database_url.to_string()),
            sql_cache: super::sql_cache::SqlResultCache::new(0),
        })
    }

    pub const TEST_INPUT_A: &'static str = "test_input_a.sql";
    pub const TEST_INPUT_B: &'static str = "test_input_b.sql";
    pub const TEST_INPUT_MODULES: &'static str = "test_input_modules.sql";
    pub const TEST_INPUT_SYMBOL_TOKENS: &'static str = "test_input_symbol_tokens.sql";
    pub const TEST_INPUT_CONTAINMENT: &'static str = "test_input_containment.sql";
    pub const TEST_INPUT_TREE_BROWSER: &'static str = "test_input_tree_browser.sql";
    pub const TEST_INPUT_NESTED_FUNC: &'static str = "test_input_nested_func.sql";
    pub const TEST_INPUT_TYPE_FILTER: &'static str = "test_input_type_filter.sql";
    pub const VERB_TEST: &'static str = "verb_test.sql";
    pub const TEST_INPUT_SEARCH: &'static str = "test_input_search.sql";

    /// Lookup table of test-fixture file name → embedded SQL.  Kept here so
    /// each new fixture only needs to land its file under `askl/sql/` and add
    /// one line to this list (vs. an `include_str!` arm per fixture).
    const TEST_FIXTURES: &'static [(&'static str, &'static str)] = &[
        (
            "test_input_a.sql",
            include_str!("../../../sql/test_input_a.sql"),
        ),
        (
            "test_input_b.sql",
            include_str!("../../../sql/test_input_b.sql"),
        ),
        (
            "test_input_modules.sql",
            include_str!("../../../sql/test_input_modules.sql"),
        ),
        (
            "test_input_symbol_tokens.sql",
            include_str!("../../../sql/test_input_symbol_tokens.sql"),
        ),
        ("verb_test.sql", include_str!("../../../sql/verb_test.sql")),
        (
            "test_input_containment.sql",
            include_str!("../../../sql/test_input_containment.sql"),
        ),
        (
            "test_input_tree_browser.sql",
            include_str!("../../../sql/test_input_tree_browser.sql"),
        ),
        (
            "test_input_nested_func.sql",
            include_str!("../../../sql/test_input_nested_func.sql"),
        ),
        (
            "test_input_type_filter.sql",
            include_str!("../../../sql/test_input_type_filter.sql"),
        ),
        (
            "test_input_search.sql",
            include_str!("../../../sql/test_input_search.sql"),
        ),
    ];

    fn load_sql(connection: &mut PgConnection, input_path: &str) {
        let sql = Self::TEST_FIXTURES
            .iter()
            .find_map(|(name, body)| (*name == input_path).then_some(*body))
            .unwrap_or_else(|| panic!("Unknown test fixture: {}", input_path));
        connection
            .batch_execute(sql)
            .map_err(|e| anyhow::anyhow!("Failed to execute SQL file '{}': {}", input_path, e))
            .unwrap();
    }

    pub async fn load_test_input(&mut self, input_path: &str) -> Result<()> {
        let database_url = self
            .database_url
            .as_ref()
            .expect("load_test_input requires Index created via connect/connect_with_test_input");
        let connection = &mut <PgConnection as diesel::Connection>::establish(database_url)
            .map_err(|e| anyhow::anyhow!("Failed to establish connection: {}", e))?;

        connection.revert_all_migrations(super::MIGRATIONS).unwrap();
        connection
            .run_pending_migrations(super::MIGRATIONS)
            .unwrap();

        Self::load_sql(connection, input_path);

        // Rebuild the pool so all connections are fresh — DDL above
        // invalidated any prepared statements cached by existing connections.
        let manager = Self::build_async_manager(database_url);
        self.pool = bb8::Pool::builder()
            .test_on_check_out(false)
            .build(manager)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to rebuild pool: {}", e))?;

        Ok(())
    }

    pub async fn get_file_contents(&self, object_id: FileId) -> Result<String> {
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        Self::get_file_contents_on(&mut *connection, object_id).await
    }

    /// Suggest indexed symbol names close to `query`, for "did you mean?" when a
    /// name selector matched nothing. Uses pg_trgm `word_similarity` (GIN-indexed
    /// via `<%`) against both the full `name` and the normalized `leaf_name`, so a
    /// typed leaf token (`allocateNode`) can still surface a compound symbol
    /// (`(*cache).allocateNode`). Scoped to the visible projects; empty scope or a
    /// blank query returns nothing.
    ///
    /// Routed through [`Index::cached_load`] so repeated bad-name probes hit the
    /// RAM result cache; the cache is keyed on the rendered SQL+binds (query text,
    /// project ids, limit) and cleared on index mutation, so suggestions stay
    /// consistent after a reindex. Binds are owned so the query is `'static`.
    /// `visible_ids` = the visible layer set (`eph.visible_ids()`); suggestions
    /// are restricted to it via `s.layer = ANY(...)`, so they agree with the
    /// `has_symbol_matching` existence probe (which applies the same visibility)
    /// rather than returning names from hidden/superseded layers.
    ///
    /// Uses pg_trgm `word_similarity` via `<%` (GIN-accelerated). The match floor
    /// is the session `pg_trgm.word_similarity_threshold` default (0.6); we never
    /// `SET` it, so it is deterministic. Lowering it for more short-typo recall
    /// would need a txn-scoped `SET LOCAL` (the GIN `<%` operator is threshold-
    /// bound) and is deferred.
    pub async fn suggest_symbol_names(
        &self,
        query: &str,
        project_ids: &[i32],
        visible_ids: &[i64],
        limit: usize,
    ) -> Result<Vec<String>> {
        use diesel::sql_types::{Array, BigInt, Integer, Text};

        if query.trim().is_empty() || project_ids.is_empty() {
            return Ok(Vec::new());
        }

        let sql = r#"
            SELECT name, MAX(score)::float8 AS score
            FROM (
                SELECT s.name AS name,
                       GREATEST(
                           word_similarity($1, s.name),
                           word_similarity($1, s.leaf_name)
                       ) AS score
                FROM index.symbols s
                WHERE s.project_id = ANY($2)
                  AND s.layer = ANY($3)
                  AND ($1 <% s.name OR $1 <% s.leaf_name)
            ) t
            GROUP BY name
            ORDER BY score DESC, char_length(name) ASC, name ASC
            LIMIT $4
        "#;

        let sql_query = diesel::sql_query(sql)
            .bind::<Text, _>(query.to_string())
            .bind::<Array<Integer>, _>(project_ids.to_vec())
            .bind::<Array<BigInt>, _>(visible_ids.to_vec())
            .bind::<Integer, _>(limit as i32);

        let rows: std::sync::Arc<Vec<NameSuggestionRow>> = self.cached_load(sql_query).await?;

        Ok(rows.iter().map(|r| r.name.clone()).collect())
    }

    /// Raw file bytes, exactly as indexed — no lossy UTF-8 conversion, so byte
    /// offsets stored in the index line up with the returned content. Preferred
    /// when offsets must stay exact (e.g. rendering `file:line` locations).
    pub async fn get_file_contents_bytes(&self, object_id: FileId) -> Result<Vec<u8>> {
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        Self::get_file_contents_bytes_on(&mut *connection, object_id).await
    }

    /// Same as [`Index::get_file_contents`], but on a caller-supplied
    /// connection — for deferred populate closures running inside an
    /// [`EphTransaction`] (see [`Index::search_content_matches_on`]).
    pub async fn get_file_contents_on(
        connection: &mut AsyncPgConnection,
        object_id: FileId,
    ) -> Result<String> {
        let content = Self::get_file_contents_bytes_on(connection, object_id).await?;
        Ok(String::from_utf8_lossy(&content).to_string())
    }

    /// Raw file bytes on a caller-supplied connection. The `String` variants are
    /// lossy on non-UTF-8 content and must not be used where byte offsets matter.
    pub async fn get_file_contents_bytes_on(
        connection: &mut AsyncPgConnection,
        object_id: FileId,
    ) -> Result<Vec<u8>> {
        let object_id: i32 = object_id.into();
        let result: Option<Vec<u8>> = diesel::sql_query(
            r#"
            SELECT COALESCE(oc.content, cs.content) AS content
            FROM index.objects o
            LEFT JOIN index.object_contents oc ON oc.object_id = o.id
            LEFT JOIN index.content_store cs ON cs.content_hash = o.content_hash
            WHERE o.id = $1
              AND o.id > 0
            "#,
        )
        .bind::<diesel::sql_types::Integer, _>(object_id)
        .get_result::<ContentRow>(&mut *connection)
        .await
        .optional()
        .map_err(|e| anyhow::anyhow!("Failed to query file contents: {}", e))?
        .map(|row| row.content);

        match result {
            Some(content) => Ok(content),
            None => Err(anyhow::anyhow!(
                "File contents not found for object_id {}",
                object_id
            )),
        }
    }

    /// Does any symbol match `filter` under the current visibility? A lean
    /// existence probe: the *current* query only (no parents/children joins,
    /// unlike `find_symbol`) with `LIMIT 1`, so it never materialises the full
    /// match set. Reuses the same visibility + composite-filter machinery, so a
    /// name filter here matches exactly what a selector would. Cache-integrated.
    pub async fn has_symbol_matching(
        &self,
        filter: &CompositeFilter,
        eph: &EphContext,
    ) -> Result<bool> {
        let chain_dependent = filter.is_chain_dependent();
        let rows: Vec<(Symbol, SymbolInstance, Object, Project)> = self
            .cached_load_partitioned(eph, chain_dependent, |vis| {
                let mut q = build_current_query(vis);
                if let Some(expr) = filter.compose_current(vis) {
                    q = q.filter(expr);
                }
                q.filter(vis.guard()).limit(1)
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to probe symbol existence: {}", e))?;
        Ok(!rows.is_empty())
    }

    pub async fn find_symbol(
        &self,
        filter: &CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
    ) -> Result<super::selection::Checked<Selection>> {
        let visible_ids = eph.visible_ids();
        let eph_ids: &[i64] = &visible_ids;
        use crate::schema_diesel::*;

        // One chain-dependence decision for the whole statement's filter
        // tree (conservative: any chain-dependent leaf demotes every
        // sub-query to the Combined fallback — correctness-neutral, minor
        // sharing loss, simple plumbing).
        let chain_dependent = filter.is_chain_dependent();

        // Budget pushdown — the "how many" facet of fusion.  A *bare* selector's
        // neighbourhood queries are display-only, so bound how many rows they
        // produce.  A *composed* selector (parent/children scope is a `Scope`)
        // must NOT be bounded here: its unscoped rows feed the Rust-side
        // intersection, and truncating them would drop valid results — that is a
        // job for scope-fusion, not budget.
        let leaf_limit: Option<i64> = if parent_scope.is_scope() || children_scope.is_scope() {
            None
        } else {
            eph.result_budget().leaf_limit()
        };

        let current = {
            let _select_current: tracing::span::EnteredSpan =
                tracing::info_span!("select_current", eph_count = eph_ids.len()).entered();
            let t0 = std::time::Instant::now();

            let rows: Vec<(Symbol, SymbolInstance, Object, Project)> = self
                .cached_load_partitioned(eph, chain_dependent, |vis| {
                    let mut q = build_current_query(vis);
                    if let Some(expr) = filter.compose_current(vis) {
                        q = q.filter(expr);
                    }
                    let mut q = q.filter(vis.guard());
                    if let Some(l) = leaf_limit {
                        q = q.limit(l);
                    }
                    q
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = rows.len(),
                result_bytes = super::sql_cache::vec_weight(&rows),
                "select_current completed",
            );
            rows
        };

        // Use the IDs that survived current's filters (type, name, etc.)
        // so that parents/children queries don't process instances that
        // current excluded.  Sorted: SQL result order is plan-dependent and
        // these ids become rendered binds in downstream cache keys.
        let mut current_instance_ids: Vec<i64> =
            current.iter().map(|(_, inst, _, _)| inst.id).collect();
        current_instance_ids.sort_unstable();
        current_instance_ids.dedup();

        let parents = match parent_scope {
            ScopeContext::Skip => vec![],
            ScopeContext::Unscoped => {
                let _parents_span: tracing::span::EnteredSpan = tracing::info_span!(
                    "select_parents",
                    scope = "unscoped",
                    source_count = current_instance_ids.len(),
                    eph_count = eph_ids.len(),
                )
                .entered();
                let t0 = std::time::Instant::now();
                let rows: Vec<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)> = self
                    .cached_load_partitioned(eph, chain_dependent, |vis| {
                        let mut q = build_parents_query(current_instance_ids.clone(), vis);
                        if let Some(expr) = filter.compose_parents(vis) {
                            q = q.filter(expr);
                        }
                        let mut q = q.filter(vis.guard());
                        if let Some(l) = leaf_limit {
                            q = q.limit(l);
                        }
                        q
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    result_bytes = super::sql_cache::vec_weight(&rows),
                    "select_parents completed",
                );
                rows
            }
            ScopeContext::Scope {
                ref ids,
                filter: ref scope_filter,
            } => {
                let _parents_span: tracing::span::EnteredSpan = tracing::info_span!(
                    "select_parents",
                    scope = "scoped",
                    source_count = current_instance_ids.len(),
                    eph_count = eph_ids.len(),
                )
                .entered();
                let t0 = std::time::Instant::now();

                let role = ScopeRole::Parents(current_instance_ids.clone());
                let scope_ids = self
                    .resolve_scope_ids(ids, scope_filter, Some(&role), eph)
                    .await?;

                let rows: Vec<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)> = self
                    .cached_load_partitioned(eph, chain_dependent, |vis| {
                        let mut q = build_parents_query(current_instance_ids.clone(), vis);
                        if let Some(expr) = filter.compose_parents(vis) {
                            q = q.filter(expr);
                        }
                        // Always apply scope filter. When scope_ids is empty,
                        // eq_any([]) correctly returns zero rows (scope
                        // specified but matched nothing).
                        let parent_decls = PARENT_DECLS_ALIAS;
                        q = q.filter(
                            parent_decls
                                .field(symbol_instances::dsl::id)
                                .eq_any(scope_ids.clone()),
                        );
                        q.filter(vis.guard())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    result_bytes = super::sql_cache::vec_weight(&rows),
                    "select_parents completed",
                );
                rows
            }
        };

        let children = match children_scope {
            ScopeContext::Skip => vec![],
            ScopeContext::Unscoped => {
                let _select_children: tracing::span::EnteredSpan = tracing::info_span!(
                    "select_children",
                    scope = "unscoped",
                    source_count = current_instance_ids.len(),
                    eph_count = eph_ids.len(),
                )
                .entered();
                let t0 = std::time::Instant::now();
                let rows: Vec<(
                    Symbol,
                    Symbol,
                    SymbolInstance,
                    SymbolInstance,
                    SymbolRef,
                    Object,
                )> = self
                    .cached_load_partitioned(eph, chain_dependent, |vis| {
                        let mut q = build_children_query(current_instance_ids.clone(), vis);
                        if let Some(expr) = filter.compose_children(vis) {
                            q = q.filter(expr);
                        }
                        let mut q = q.filter(vis.guard());
                        if let Some(l) = leaf_limit {
                            q = q.limit(l);
                        }
                        q
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    result_bytes = super::sql_cache::vec_weight(&rows),
                    "select_children completed",
                );
                rows
            }
            ScopeContext::Scope {
                ref ids,
                filter: ref scope_filter,
            } => {
                let _select_children: tracing::span::EnteredSpan = tracing::info_span!(
                    "select_children",
                    scope = "scoped",
                    source_count = current_instance_ids.len(),
                    eph_count = eph_ids.len(),
                )
                .entered();
                let t0 = std::time::Instant::now();

                let role = ScopeRole::Children(current_instance_ids.clone());
                let scope_ids = self
                    .resolve_scope_ids(ids, scope_filter, Some(&role), eph)
                    .await?;

                let rows: Vec<(
                    Symbol,
                    Symbol,
                    SymbolInstance,
                    SymbolInstance,
                    SymbolRef,
                    Object,
                )> = self
                    .cached_load_partitioned(eph, chain_dependent, |vis| {
                        let mut q = build_children_query(current_instance_ids.clone(), vis);
                        if let Some(expr) = filter.compose_children(vis) {
                            q = q.filter(expr);
                        }
                        // Always apply scope filter.
                        q = q.filter(symbol_instances::dsl::id.eq_any(scope_ids.clone()));
                        q.filter(vis.guard())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    result_bytes = super::sql_cache::vec_weight(&rows),
                    "select_children completed",
                );
                rows
            }
        };

        // Scope the containment families by an ids-only neighbour scope —
        // the probe-resolved / computed-selection case.  This is what keeps
        // a broad container's has-family from materialising its entire
        // containment neighbourhood (measured 214k rows / 142MB) when the
        // neighbour is already known to a small id set.  Predicate scopes
        // and Skip/Unscoped keep the unconstrained display behaviour:
        // Skip's refs-deferral contract does not extend to containment.
        let has_parent_scope_ids: Option<Vec<i64>> = match &parent_scope {
            ScopeContext::Scope { ids, filter: None } => Some(ids.clone()),
            _ => None,
        };
        let has_children_scope_ids: Option<Vec<i64>> = match &children_scope {
            ScopeContext::Scope { ids, filter: None } => Some(ids.clone()),
            _ => None,
        };

        let has_parents = {
            let _has_parents_span: tracing::span::EnteredSpan = tracing::info_span!(
                "select_has_parents",
                source_count = current_instance_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();

            let rows: Vec<(Symbol, SymbolInstance, Symbol, SymbolInstance)> = self
                .cached_load_partitioned(eph, chain_dependent, |vis| {
                    let mut q = build_has_parents_query(current_instance_ids.clone(), vis);
                    if let Some(expr) = filter.compose_has_parents(vis) {
                        q = q.filter(expr);
                    }
                    if let Some(scope_ids) = &has_parent_scope_ids {
                        let container_instance = CONTAINER_INSTANCE_ALIAS;
                        q = q.filter(
                            container_instance
                                .field(symbol_instances::dsl::id)
                                .eq_any(scope_ids.clone()),
                        );
                    }
                    q.filter(vis.guard())
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load containment parents: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = rows.len(),
                result_bytes = super::sql_cache::vec_weight(&rows),
                "select_has_parents completed",
            );
            rows
        };

        let has_children = {
            let source_count = current_instance_ids.len();
            let _has_children_span: tracing::span::EnteredSpan = tracing::info_span!(
                "select_has_children",
                source_count = source_count,
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();

            let has_children_filtered = filter.constrains_has_children();
            let contained_scope = |q: super::mixins::HasChildrenQuery<'static>| {
                let contained_instance = super::mixins::CONTAINED_INSTANCE_ALIAS;
                match &has_children_scope_ids {
                    Some(scope_ids) => q.filter(
                        contained_instance
                            .field(symbol_instances::dsl::id)
                            .eq_any(scope_ids.clone()),
                    ),
                    None => q,
                }
            };
            let rows: Vec<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)> =
                if has_children_filtered {
                    self.cached_load_partitioned(eph, chain_dependent, |vis| {
                        let mut q = build_has_children_query(current_instance_ids.clone(), vis);
                        if let Some(expr) = filter.compose_has_children(vis) {
                            q = q.filter(expr);
                        }
                        contained_scope(q).filter(vis.guard())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
                } else {
                    // Fast path: CTE-materialised source set (planner-hint fix).
                    self.cached_load_partitioned(eph, chain_dependent, |vis| CteHasChildren {
                        cte_body: build_has_children_cte_body(current_instance_ids.clone(), vis),
                        outer: contained_scope(build_has_children_query_against_cte(vis))
                            .filter(vis.guard()),
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
                };
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = rows.len(),
                result_bytes = super::sql_cache::vec_weight(&rows),
                "select_has_children completed",
            );
            rows
        };

        // Any leaf that filled its budget LIMIT may have dropped rows — flag
        // the selection so the owning command surfaces a truncation warning.
        // Budget truncation is acceptable, silent budget truncation is not.
        // (Branch-level limits mean a full branch is only *probably* truncated;
        // over-warning is the safe direction.)
        let budget_bounded = leaf_limit.is_some_and(|l| {
            let l = l as usize;
            current.len() >= l || parents.len() >= l || children.len() >= l
        });

        let selection: Result<Selection> = {
            let _collect_span: tracing::span::EnteredSpan =
                tracing::debug_span!("collect").entered();

            let nodes: Vec<_> = current
                .into_iter()
                .map(|(sym, instance, object, project)| SelectionNode {
                    symbol: sym,
                    symbol_instance: instance,
                    object,
                    project,
                    query_statements: vec![],
                })
                .collect();

            let parents: Vec<_> = parents
                .into_iter()
                .map(
                    |(symbol_ref, to_symbol, to_instance, from_instance)| ParentReference {
                        symbol_ref,
                        to_symbol,
                        to_instance,
                        from_instance,
                    },
                )
                .collect();

            let mut children: Vec<_> = children
                .into_iter()
                .map(
                    |(parent_symbol, sym, instance, from_instance, sym_ref, from_object)| {
                        ChildReference {
                            parent_symbol,
                            symbol: sym,
                            symbol_instance: instance,
                            from_instance,
                            symbol_ref: sym_ref,
                            from_object,
                        }
                    },
                )
                .collect();

            children.sort_by_key(|child| (child.from_instance.id, child.symbol_instance.id));

            let has_parents: Vec<_> = has_parents
                .into_iter()
                .map(
                    |(child_symbol, child_instance, parent_symbol, parent_instance)| {
                        HasParentReference {
                            child_symbol,
                            child_instance,
                            parent_symbol,
                            parent_instance,
                        }
                    },
                )
                .collect();

            let mut has_children: Vec<_> = has_children
                .into_iter()
                .map(
                    |(
                        parent_symbol,
                        parent_instance,
                        child_symbol,
                        child_instance,
                        parent_object,
                    )| {
                        HasChildReference {
                            parent_symbol,
                            parent_instance,
                            child_symbol,
                            child_instance,
                            parent_object,
                        }
                    },
                )
                .collect();

            has_children.sort_by_key(|child| (child.parent_instance.id, child.child_instance.id));

            Ok(Selection {
                nodes,
                parents,
                children,
                has_parents,
                has_children,
                budget_bounded,
            })
        };

        // Checked::new runs has_eph_leak; bails with a uniform error on leak.
        super::selection::Checked::new(selection?, eph)
    }

    /// Query child instance IDs directly from DB given parent instance IDs.
    pub async fn find_child_instance_ids(
        &self,
        parent_ids: &[i64],
        include_refs: bool,
        include_has: bool,
        filter: &CompositeFilter,
        eph: &EphContext,
    ) -> Result<Vec<crate::symbols::SymbolInstanceId>> {
        // Canonical binds: callers pass selection-order ids; the SQL cache
        // keys on rendered binds, so sort+dedup for run-stable keys.
        let mut canonical: Vec<i64> = parent_ids.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        let parent_ids: &[i64] = &canonical;

        let visible_ids = eph.visible_ids();
        let eph_ids: &[i64] = &visible_ids;
        let chain_dependent = filter.is_chain_dependent();

        // Canonical bind order for cache-key stability (see find_edges_between).
        let mut source_ids = parent_ids.to_vec();
        source_ids.sort_unstable();
        source_ids.dedup();

        let mut all_ids: Vec<i64> = Vec::new();

        if include_has {
            let _span = tracing::info_span!(
                "find_child_instance_ids_has",
                source_count = parent_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();
            let has_children_filtered = filter.constrains_has_children();
            let results: Vec<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)> =
                if has_children_filtered {
                    self.cached_load_partitioned(eph, chain_dependent, |vis| {
                        let mut q = build_has_children_query(source_ids.clone(), vis);
                        if let Some(expr) = filter.compose_has_children(vis) {
                            q = q.filter(expr);
                        }
                        q.filter(vis.guard())
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to find has-child instance IDs: {}", e))?
                } else {
                    // Fast path: CTE-materialised source set (planner-hint fix).
                    self.cached_load_partitioned(eph, chain_dependent, |vis| CteHasChildren {
                        cte_body: build_has_children_cte_body(source_ids.clone(), vis),
                        outer: build_has_children_query_against_cte(vis).filter(vis.guard()),
                    })
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to find has-child instance IDs: {}", e))?
                };
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                result_bytes = super::sql_cache::vec_weight(&results),
                "find_child_instance_ids (has) completed",
            );
            for (ps, pi, cs, ci, _) in &results {
                if is_eph_leak(ps.layer, eph_ids)
                    || is_eph_leak(pi.layer, eph_ids)
                    || is_eph_leak(cs.layer, eph_ids)
                    || is_eph_leak(ci.layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "layer leak in find_child_instance_ids (has)");
                    anyhow::bail!("internal error: ephemeral layer isolation violation");
                }
            }
            all_ids.extend(results.iter().map(|(_, _, _, child_inst, _)| child_inst.id));
        }

        if include_refs {
            let _span = tracing::info_span!(
                "find_child_instance_ids_refs",
                source_count = parent_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();
            let results: Vec<(
                Symbol,
                Symbol,
                SymbolInstance,
                SymbolInstance,
                SymbolRef,
                Object,
            )> = self
                .cached_load_partitioned(eph, chain_dependent, |vis| {
                    let mut q = build_children_query(source_ids.clone(), vis);
                    if let Some(expr) = filter.compose_children(vis) {
                        q = q.filter(expr);
                    }
                    q.filter(vis.guard())
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-child instance IDs: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                result_bytes = super::sql_cache::vec_weight(&results),
                "find_child_instance_ids (refs) completed",
            );
            for (ps, cs, ci, fi, sr, _) in &results {
                if is_eph_leak(ps.layer, eph_ids)
                    || is_eph_leak(cs.layer, eph_ids)
                    || is_eph_leak(ci.layer, eph_ids)
                    || is_eph_leak(fi.layer, eph_ids)
                    || is_eph_leak(sr.layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "layer leak in find_child_instance_ids (refs)");
                    anyhow::bail!("internal error: ephemeral layer isolation violation");
                }
            }
            all_ids.extend(
                results
                    .iter()
                    .map(|(_, _, callee_inst, _, _, _)| callee_inst.id),
            );
        }

        all_ids.sort_unstable();
        all_ids.dedup();
        Ok(all_ids
            .into_iter()
            .map(crate::symbols::SymbolInstanceId::new)
            .collect())
    }

    /// Query parent instance IDs directly from DB given child instance IDs.
    pub async fn find_parent_instance_ids(
        &self,
        child_ids: &[i64],
        include_refs: bool,
        include_has: bool,
        filter: &CompositeFilter,
        eph: &EphContext,
    ) -> Result<Vec<crate::symbols::SymbolInstanceId>> {
        // Canonical binds: callers pass selection-order ids; the SQL cache
        // keys on rendered binds, so sort+dedup for run-stable keys.
        let mut canonical: Vec<i64> = child_ids.to_vec();
        canonical.sort_unstable();
        canonical.dedup();
        let child_ids: &[i64] = &canonical;

        let visible_ids = eph.visible_ids();
        let eph_ids: &[i64] = &visible_ids;
        let chain_dependent = filter.is_chain_dependent();

        // Canonical bind order for cache-key stability (see find_edges_between).
        let mut source_ids = child_ids.to_vec();
        source_ids.sort_unstable();
        source_ids.dedup();

        let mut all_ids: Vec<i64> = Vec::new();

        if include_refs {
            let _span = tracing::info_span!(
                "find_parent_instance_ids_refs",
                source_count = child_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();
            let results: Vec<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)> = self
                .cached_load_partitioned(eph, chain_dependent, |vis| {
                    let mut q = build_parents_query(source_ids.clone(), vis);
                    if let Some(expr) = filter.compose_parents(vis) {
                        q = q.filter(expr);
                    }
                    q.filter(vis.guard())
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-parent instance IDs: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                result_bytes = super::sql_cache::vec_weight(&results),
                "find_parent_instance_ids (refs) completed",
            );
            for (sr, s, ci, pi) in &results {
                if is_eph_leak(sr.layer, eph_ids)
                    || is_eph_leak(s.layer, eph_ids)
                    || is_eph_leak(ci.layer, eph_ids)
                    || is_eph_leak(pi.layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "layer leak in find_parent_instance_ids (refs)");
                    anyhow::bail!("internal error: ephemeral layer isolation violation");
                }
            }
            all_ids.extend(results.iter().map(|(_, _, _, parent_inst)| parent_inst.id));
        }

        if include_has {
            let _span = tracing::info_span!(
                "find_parent_instance_ids_has",
                source_count = child_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();
            let results: Vec<(Symbol, SymbolInstance, Symbol, SymbolInstance)> = self
                .cached_load_partitioned(eph, chain_dependent, |vis| {
                    let mut q = build_has_parents_query(source_ids.clone(), vis);
                    if let Some(expr) = filter.compose_has_parents(vis) {
                        q = q.filter(expr);
                    }
                    q.filter(vis.guard())
                })
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-parent instance IDs: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                result_bytes = super::sql_cache::vec_weight(&results),
                "find_parent_instance_ids (has) completed",
            );
            for (cs, ci, ps, pi) in &results {
                if is_eph_leak(cs.layer, eph_ids)
                    || is_eph_leak(ci.layer, eph_ids)
                    || is_eph_leak(ps.layer, eph_ids)
                    || is_eph_leak(pi.layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "layer leak in find_parent_instance_ids (has)");
                    anyhow::bail!("internal error: ephemeral layer isolation violation");
                }
            }
            all_ids.extend(
                results
                    .iter()
                    .map(|(_, _, _, container_inst)| container_inst.id),
            );
        }

        all_ids.sort_unstable();
        all_ids.dedup();
        Ok(all_ids
            .into_iter()
            .map(crate::symbols::SymbolInstanceId::new)
            .collect())
    }

    /// Discover all reference edges between a set of selected instances.
    pub async fn find_edges_between(
        &self,
        instance_ids: &[i64],
        eph: &EphContext,
    ) -> Result<Vec<ImplicitEdge>> {
        let visible_ids = eph.visible_ids();
        let eph_ids: &[i64] = &visible_ids;
        if instance_ids.is_empty() {
            return Ok(vec![]);
        }
        // Canonicalize the candidate ids: eq_any membership is
        // order-insensitive, but the rendered binds are part of the cache
        // key — a caller iterating a HashSet would otherwise re-key this
        // (expensive) query on every request.
        let mut instance_ids = instance_ids.to_vec();
        instance_ids.sort_unstable();
        instance_ids.dedup();
        let instance_ids = &instance_ids[..];

        let _span = tracing::info_span!(
            "find_edges_between",
            count = instance_ids.len(),
            eph_count = eph_ids.len(),
        )
        .entered();
        let t0 = std::time::Instant::now();

        // CTE-materialised candidate set: PG's planner picks a much
        // better nested-loop join order when it knows the from/to
        // candidate set is exactly 52-ish rows.  Without the CTE, the
        // planner with empty `$2` (no ephemeral layers visible) picks
        // a to_inst → sr (by to_symbol, ~2.7K rows per to_inst) →
        // from_inst (GIST) plan and scans ~141K rows; with the CTE
        // materialised, it scans ~2.7K and runs ~1000× faster on the
        // empty-eph case while staying ~equivalent on the non-empty
        // case.  See the EXPLAIN-ANALYZE comparison documented in
        // the commit that added this.
        //
        // The CTE body is built via typed Diesel DSL so it tracks
        // `symbol_instances` schema changes automatically.  The outer
        // SELECT's projection is bespoke (DISTINCT ON + custom column
        // aliases) and stays as raw SQL inside `CteFindEdgesBetween`;
        // its single bind (eph_ids for `sr.layer`) goes through
        // `push_bind_param` so the final query has only typed binds.
        // No composite filter reaches this query, so it always partitions.
        // The eph branch's guard is rendered inside CteFindEdgesBetween's
        // walk_ast (raw outer SQL) from the token snapshot.
        let mut results: Vec<ImplicitEdge> = self
            .cached_load_partitioned(eph, false, |vis| CteFindEdgesBetween {
                cte_body: build_find_edges_cte_body(instance_ids.to_vec(), vis),
                vis: vis.snapshot(),
            })
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find edges between instances: {}", e))?;

        // Restore the legacy single-query `DISTINCT ON (from_inst.id, sr.id)
        // ... ORDER BY from_inst.id, sr.id, to_inst.id` invariant ACROSS the
        // merged branches: each branch ran its own DISTINCT ON, so a
        // (from, ref) pair whose target symbol has both a persistent and an
        // eph instance yields one row per branch.  Downstream dedups edges
        // by (from_instance, to_symbol, occurrence) with first-row-wins, so
        // without this collapse the winner would be picked by merge order
        // (persistent first) instead of min to_instance_id — silently
        // retargeting implicit edges away from eph instances (negative ids
        // sort first, so legacy picked the eph one).
        results.sort_unstable_by_key(|e| (e.from_instance_id, e.ref_id, e.to_instance_id));
        results.dedup_by_key(|e| (e.from_instance_id, e.ref_id));

        tracing::info!(
            elapsed_ms = t0.elapsed().as_millis() as u64,
            result_rows = results.len(),
            result_bytes = super::sql_cache::vec_weight(&results),
            "find_edges_between completed",
        );

        for edge in &results {
            if is_eph_leak(edge.sr_layer, eph_ids)
                || is_eph_leak(edge.from_layer, eph_ids)
                || is_eph_leak(edge.to_layer, eph_ids)
            {
                tracing::error!(?eph_ids, "layer leak in find_edges_between");
                anyhow::bail!("internal error: ephemeral layer isolation violation");
            }
        }

        Ok(results)
    }

    // ========================================================================
    // Ephemeral layer CRUD
    // ========================================================================

    /// Begin a transactional ephemeral layer create-or-find.
    ///
    /// Opens a database transaction and performs an atomic upsert on `layers`.
    /// Returns an `EphTransaction` that holds the connection with an open transaction.
    /// Most callers should use [`Index::with_eph_layer`] instead, which owns
    /// commit/rollback and the 2-phase `populated` flip through a scoped closure.
    ///
    /// The `created` flag uses PostgreSQL's `xmax = 0` trick: a freshly inserted row
    /// has xmax=0, while an ON CONFLICT UPDATE sets xmax to the updating transaction.
    ///
    /// ## Implicit serialization between concurrent creators
    ///
    /// Two requests with the same `hash` that race for the layer will both
    /// reach the `INSERT … ON CONFLICT` statement; the loser blocks on a
    /// row-level lock until the winner's transaction commits or rolls back.
    /// The winner inserts with `populated = FALSE`, runs the populate batch,
    /// flips `populated = TRUE`, and commits — all in one transaction.  The
    /// loser unblocks after the commit and sees `created = false, populated
    /// = true`.  A `created = false, populated = false` cache hit is an
    /// anomaly (someone committed the layer row without running the populate
    /// batch — e.g. a future writer that bypasses [`Index::with_eph_layer`]);
    /// it surfaces as an error rather than a silently-half-built layer.
    ///
    /// ## `parent_id` semantics under cache collision
    ///
    /// `parent_id` is recorded only when the row is *first* inserted; the
    /// `ON CONFLICT DO UPDATE` clause only touches `last_used`.  Under
    /// per-root materialisation this is vacuous rather than a caveat: a
    /// layer's parent is a pure function of its hash (a root shard's hash
    /// folds its root's identity and its parent IS that root; a selection
    /// shard's hash folds its parent id), so a cache hit always carries the
    /// same `parent_id` the caller would have written.
    pub async fn create_eph_layer(
        &self,
        parent_id: Option<i64>,
        root_shard_id: Option<i64>,
        hash: &[u8],
        kind: EphLayerKind,
    ) -> Result<EphTransaction<'_>> {
        let mut conn = self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::sql_query("BEGIN")
            .execute(&mut *conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to BEGIN transaction: {}", e))?;

        // Diesel-DSL upsert: VALUES + ON CONFLICT (hash) DO UPDATE SET
        // last_used = now() RETURNING (id, xmax = 0 AS created, populated, truncated).
        // The `xmax = 0` projection is PG-specific (works because a
        // freshly-inserted row has xmax = 0 while an ON CONFLICT DO
        // UPDATE bumps xmax to the updating txid) — express it via a
        // `sql::<Bool>` fragment.
        //
        // `truncated` is reset to FALSE on insert (a fresh layer has not
        // truncated anything yet); on cache hit we read the value previously
        // written by the layer's original creator so the caller can re-emit
        // the same warning.
        use crate::schema_diesel::layers;
        use diesel::sql_types::Bool;
        let upsert_result = diesel::insert_into(layers::table)
            .values((
                layers::parent_id.eq(parent_id),
                layers::root_shard_id.eq(root_shard_id),
                layers::hash.eq(hash),
                layers::kind.eq(kind.as_str()),
                layers::populated.eq(false),
                layers::truncated.eq(false),
            ))
            .on_conflict(layers::hash)
            .do_update()
            .set(layers::last_used.eq(diesel::dsl::now))
            .returning((
                layers::id,
                diesel::dsl::sql::<Bool>("xmax = 0"),
                layers::populated,
                layers::truncated,
            ))
            .get_result::<(i64, bool, bool, bool)>(&mut *conn)
            .await;
        let (id, created, populated, truncated_on_open) = match upsert_result {
            Ok(row) => row,
            Err(e) => {
                let _ = diesel::sql_query("ROLLBACK").execute(&mut *conn).await;
                // A parent/root-shard FK violation here means the referenced
                // layer was purged between this statement's earlier
                // materialization steps and this insert (e.g. `purge_eph_cache`
                // won the layers lock in the gap between a root shard's commit
                // and its selection shard).  The cache is coherent — this
                // request just
                // straddled the purge — so tell the user to retry rather
                // than surfacing a raw constraint error.
                use diesel::result::{DatabaseErrorKind, Error as E};
                if let E::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, info) = &e {
                    let constraint = info.constraint_name().unwrap_or("");
                    // Both constraint-name generations: historical
                    // (eph_layers_*, preserved by RENAME on live DBs) and
                    // regenerated-baseline (layers_*).
                    if constraint.starts_with("eph_layers_") || constraint.starts_with("layers_") {
                        anyhow::bail!(
                            "ephemeral cache was purged while this statement was \
                             executing (index update or project deletion); \
                             please retry the query"
                        );
                    }
                }
                return Err(anyhow::anyhow!("Failed to upsert eph layer: {}", e));
            }
        };

        // 2-PC sanity check: cache hits must be populated.  A
        // `created=false, populated=false` row means a previous writer
        // committed the layer without running the populate batch — an
        // invariant violation we'd rather see than silently feed callers
        // half-built state.
        if !created && !populated {
            let _ = diesel::sql_query("ROLLBACK").execute(&mut *conn).await;
            anyhow::bail!(
                "layers row id={} found with populated=false on cache hit; \
                 a previous writer committed without running the populate batch \
                 (this would silently return wrong results — see Index::with_eph_layer)",
                id,
            );
        }

        Ok(EphTransaction {
            conn,
            layer_id: id,
            created,
            truncated_on_open,
            finished: false,
        })
    }

    /// Run `body` inside an ephemeral-layer transaction, committing on success
    /// and rolling back on error.  This is the safe entry point: the
    /// `EphTransaction` cannot escape the closure, so a future cancellation
    /// or `?` early-return cannot leave a dangling open transaction.
    ///
    /// Returns `(layer_id, created, body_result)`.
    ///
    /// ## Implicit serialization between concurrent creators
    ///
    /// The contract documented on [`Index::create_eph_layer`] applies: the
    /// loser of a hash race blocks on the row lock until the winner
    /// commits, so populating-and-committing inside `body` is what keeps
    /// concurrent readers from seeing an empty layer.
    ///
    /// ## Cancellation
    ///
    /// If the outer future returned by `with_eph_layer` is dropped before
    /// either branch of the match resolves, the open transaction goes back
    /// into the bb8 pool with `EphTransaction.finished = false`.
    /// [`EphTransaction::Drop`] is synchronous and only logs at error level
    /// — it does **not** issue `ROLLBACK`. Cleanup happens at the *next*
    /// checkout of that connection via
    /// `RecyclingMethod::CustomQuery("ROLLBACK")` (configured in
    /// [`Index::connect`]).  In other words, the cancellation safety of
    /// this API depends on the pool recycling configuration; do not change
    /// `RecyclingMethod` without revisiting this contract.
    pub async fn with_eph_layer<'s, F>(
        &'s self,
        parent_id: Option<i64>,
        root_shard_id: Option<i64>,
        hash: &[u8],
        kind: EphLayerKind,
        body: F,
    ) -> Result<LayerOutcome>
    where
        F: for<'b> FnOnce(&'b mut EphTransaction<'s>) -> EphScopedFut<'b, bool>,
    {
        let mut txn = self
            .create_eph_layer(parent_id, root_shard_id, hash, kind)
            .await?;
        let layer_id = txn.layer_id();
        let created = txn.created();

        // Cache hit: skip the populate callback entirely and propagate the
        // `truncated` flag that the original creator already wrote.  Doing
        // this here (rather than expecting every caller to check `created`)
        // keeps the body closure single-shape and ensures cached truncation
        // warnings always surface to the caller.
        if !created {
            let truncated = txn.truncated_on_open();
            txn.commit().await?;
            return Ok(LayerOutcome {
                layer_id,
                created: false,
                truncated,
            });
        }

        match body(&mut txn).await {
            Ok(truncated) => {
                // 2-PC: record `truncated` and `populated` in the same
                // transaction as the populate batch so the flag and the rows
                // commit atomically.
                txn.mark_truncated(truncated).await?;
                txn.mark_populated().await?;
                txn.commit().await?;
                Ok(LayerOutcome {
                    layer_id,
                    created: true,
                    truncated,
                })
            }
            Err(e) => {
                let _ = txn.rollback().await;
                Err(e)
            }
        }
    }

    /// The ephemeral *content* layers visible in this request that carry
    /// `objects` rows, grouped by project.  This is the N-way shard set: the
    /// logical content layers (root is the always-present root shard, handled
    /// separately), NOT the derived layer shards — layer shards hold
    /// instances, never `objects`, so they are visible-for-read but never
    /// re-sharded, which is what keeps the shard set from growing
    /// multiplicatively.  Filters `layer < 0` (ephemeral) and groups by
    /// project so each root shards only over its own content.  Returns `{}`
    /// today (no eph layer carries content) ⇒ zero layer shards ⇒ production
    /// byte-identical.
    async fn eph_content_layers_grouped(
        &self,
        visible: &[i64],
    ) -> Result<std::collections::HashMap<i32, Vec<i64>>> {
        use crate::schema_diesel::objects;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        // ORDER BY makes the per-project layer order deterministic (SELECT
        // DISTINCT alone does not), so the resulting layer-shard
        // materialisation and activation-trace order are stable across runs.
        let rows = objects::table
            .filter(objects::layer.eq_any(visible))
            .filter(objects::layer.lt(0))
            .select((objects::project_id, objects::layer))
            .distinct()
            .order((objects::project_id.asc(), objects::layer.asc()))
            .load::<(i32, i64)>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to enumerate eph content layers: {}", e))?;
        let mut grouped: std::collections::HashMap<i32, Vec<i64>> =
            std::collections::HashMap::new();
        for (project_id, layer) in rows {
            grouped.entry(project_id).or_default().push(layer);
        }
        Ok(grouped)
    }

    /// Materialize a verb's partitioned cache entries, ONE PER VISIBLE ROOT:
    /// for each root, a *root shard* parented on the root, keyed on
    /// `root_shard_hash(root, input_hash)` and holding only that project's
    /// rows (unmasked — masks are composed at read time, never applied here)
    /// plus, iff that root's chain is non-empty, a *selection shard* chained
    /// on the root's `tip` holding the eph-derived delta.  `eph` is the
    /// executor's PRE-TREE snapshot, so selection shards parent on the
    /// previous top-level statement's tip — commands of one tree never chain
    /// on each other.  The selection shard is always materialized under a
    /// non-empty chain — even with zero rows — so downstream chaining keys
    /// stay deterministic.  Results are sorted by root id, so activation
    /// traces are deterministic.
    ///
    /// Root-parenting the root shard means a project's cached chains die with
    /// its root (`parent_id ON DELETE CASCADE` + the doomed-closure parent
    /// edge); a root shard's parent is a pure function of its hash, since the
    /// hash folds the root's identity.
    ///
    /// Each layer runs the full 2-PC populated protocol via
    /// [`Index::with_eph_layer`].  A failure after some layers commit leaves
    /// valid, reusable entries — benign.
    ///
    /// Accepted limitation (single-user tool, mirrors the torn-read note on
    /// [`Index::cached_load_partitioned`]): per-root layers commit in
    /// SEPARATE transactions at different instants, so a purge racing this
    /// statement can delete an earlier root's just-committed root shard
    /// before a later root's is created — the statement then reads a silently
    /// PARTIAL, mixed-epoch union (the dead layer contributes zero rows).
    /// The epoch check prevents such reads from being cached, not from
    /// being returned once.  Because populate closures run the
    /// expensive queries themselves, the work happens while each layer's row
    /// lock is held: concurrent identical requests serialize per (root,
    /// hash) and the losers get cache hits instead of duplicating the
    /// computation.
    pub async fn with_partitioned_layers<'s, FR, FS, FL>(
        &'s self,
        eph: &'s EphContext,
        input_hash: &[u8; 32],
        selection_extra: &[u8],
        root_shard_populate: FR,
        selection_shard_populate: FS,
        layer_shard_populate: Option<FL>,
    ) -> Result<Vec<MaterialisedLayer>>
    where
        FR: for<'b> Fn(&'b mut EphTransaction<'s>, &'b RootLayer) -> EphScopedFut<'b, bool>,
        FS: for<'b> Fn(
            &'b mut EphTransaction<'s>,
            &'b RootLayer,
            RootShardRef,
        ) -> EphScopedFut<'b, bool>,
        FL: for<'b> Fn(
            &'b mut EphTransaction<'s>,
            &'b RootLayer,
            i64,
            RootShardRef,
        ) -> EphScopedFut<'b, bool>,
    {
        use futures::stream::StreamExt;

        // Roots materialise CONCURRENTLY: their chains, cache rows, and row
        // locks are disjoint, and each per-root task holds at most one pool
        // connection at a time (the root shard commits and returns its
        // connection before the selection shard's checkout), so a bounded
        // fan-out cannot
        // deadlock against the pool — it just queues.  Keep the bound
        // comfortably under the bb8 pool size (default 10).  Works on a
        // LocalSet with !Send futures: nothing is spawned, the stream polls
        // in place.
        const ROOT_FANOUT: usize = 4;

        // Shared by reference across the per-root futures (`Fn` closures).
        let root_shard_populate = &root_shard_populate;
        let selection_shard_populate = &selection_shard_populate;
        let layer_shard_populate = layer_shard_populate.as_ref();

        // N-way shard set, computed ONCE (a function of the visible set, not
        // the root): the eph content layers grouped by project.  Skipped
        // entirely when the verb exposes no layer-shard scan (`layer { … }`) or
        // when there is no eph chain at all — with only root layers visible
        // the set is necessarily empty (roots are positive), so a bare
        // `search()` does zero extra work and stays byte-identical.
        let content_by_project = if layer_shard_populate.is_some() && eph.has_chain() {
            self.eph_content_layers_grouped(&eph.visible_ids()).await?
        } else {
            std::collections::HashMap::new()
        };
        let content_by_project = &content_by_project;

        let results = futures::stream::iter(eph.roots())
            .map(|root| async move {
                let root_shard_key = root_shard_hash(root, input_hash);

                let root_shard = self
                    .with_eph_layer(
                        Some(root.id),
                        None,
                        &root_shard_key,
                        EphLayerKind::Ephemeral,
                        |txn| root_shard_populate(txn, root),
                    )
                    .await?;

                // `root_shard_id` couples a delta layer's lifetime to its
                // root shard
                // (ON DELETE CASCADE): the root shard is always the older
                // half, so when it ages out the delta — whose key folds the
                // stable root-shard *hash*, not the id — can no longer hit
                // against a recreated root shard of a different incarnation.
                // Shared by the selection shard and every layer shard.
                let root_shard_ref = RootShardRef {
                    layer_id: root_shard.layer_id,
                    hash: root_shard_key,
                };

                let selection_shard = match eph.tip(root.id) {
                    None => None,
                    Some(parent) => {
                        let sel_hash =
                            selection_shard_hash(parent, &root_shard_key, selection_extra);
                        Some(
                            self.with_eph_layer(
                                Some(parent),
                                Some(root_shard.layer_id),
                                &sel_hash,
                                EphLayerKind::Ephemeral,
                                |txn| selection_shard_populate(txn, root, root_shard_ref),
                            )
                            .await?,
                        )
                    }
                };

                // N-way layer shards (additive to the root and selection
                // shards).  One layer shard per visible eph content layer for
                // THIS root's project, keyed `layer_shard_hash(L_j, …)` and
                // parented on the layer it shards (so it dies with `L_j`).
                // The layer is already committed (it is an earlier
                // materialisation's content), so no intra-statement ordering
                // constraint applies.  Empty content set ⇒ no layer shards.
                let mut layer_shards = Vec::new();
                if let Some(layer_populate) = layer_shard_populate {
                    if let Some(layer_ids) = content_by_project.get(&root.project_id) {
                        for &lj in layer_ids {
                            let shard_hash = layer_shard_hash(lj, &root_shard_key);
                            let layer_shard = self
                                .with_eph_layer(
                                    Some(lj),
                                    Some(root_shard.layer_id),
                                    &shard_hash,
                                    EphLayerKind::Ephemeral,
                                    |txn| layer_populate(txn, root, lj, root_shard_ref),
                                )
                                .await?;
                            layer_shards.push(layer_shard);
                        }
                    }
                }

                // Order within a root: root shard → layer shards → selection
                // shard.  The (empty) selection shard goes LAST so IT — not a
                // nondeterministically-ordered layer shard — is the root's
                // `tip`, keeping downstream `selection_shard_hash` keys
                // stable and layer-shard-order-independent.  Layer shards stay
                // visible in the chain (their content is read), just never the
                // tip.
                let mut layers = Vec::with_capacity(2 + layer_shards.len());
                layers.push(MaterialisedLayer {
                    root_id: root.id,
                    role: ShardRole::Root,
                    outcome: root_shard,
                });
                for outcome in layer_shards {
                    layers.push(MaterialisedLayer {
                        root_id: root.id,
                        role: ShardRole::Layer,
                        outcome,
                    });
                }
                if let Some(outcome) = selection_shard {
                    layers.push(MaterialisedLayer {
                        root_id: root.id,
                        role: ShardRole::Selection,
                        outcome,
                    });
                }
                Ok::<_, anyhow::Error>(layers)
            })
            .buffer_unordered(ROOT_FANOUT)
            // Run EVERY per-root future to completion before propagating an
            // error — never cancel siblings.  A cancelled future would drop
            // its EphTransaction mid-populate, and Drop cannot ROLLBACK: the
            // abandoned connection would sit idle-in-transaction holding its
            // layer's row lock (blocking identical retries on the hash
            // upsert and any purge's LOCK TABLE) until pool recycling.
            // Letting each root finish means every transaction reaches its
            // own commit or explicit rollback; roots that committed before a
            // sibling's error remain valid, reusable cache entries.
            .collect::<Vec<Result<Vec<MaterialisedLayer>>>>()
            .await;

        let mut groups = results.into_iter().collect::<Result<Vec<_>>>()?;

        // Deterministic result (and therefore activation-trace) order
        // regardless of completion order: roots ascending, and within a root
        // root shard → layer shards → selection shard (by construction of each
        // group;
        // the stable sort preserves that intra-group order).
        groups.sort_by_key(|g| g.get(0).map(|l| l.root_id));
        Ok(groups.into_iter().flatten().collect())
    }

    /// Get all instance IDs belonging to any of the given ephemeral layers.
    /// Union read across a root shard + selection shard pair — the point
    /// where the halves of a partitioned entry compose (and where future mask
    /// rows will subtract).
    pub async fn get_eph_instance_ids_for_layers(&self, layer_ids: &[i64]) -> Result<Vec<i64>> {
        use crate::schema_diesel::symbol_instances;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let ids = symbol_instances::table
            .filter(symbol_instances::layer.eq_any(layer_ids))
            .select(symbol_instances::id)
            .load::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get eph instance IDs: {}", e))?;

        Ok(ids)
    }

    /// Get all instance IDs belonging to a given ephemeral layer.
    /// Delegates to the plural variant so there is exactly one read path
    /// (and one future mask-composition point).
    pub async fn get_eph_instance_ids_for_layer(&self, layer_id: i64) -> Result<Vec<i64>> {
        self.get_eph_instance_ids_for_layers(&[layer_id]).await
    }

    /// Get the ephemeral layer ID(s) for a given instance ID.
    pub async fn get_eph_layer_for_instance(&self, instance_id: i64) -> Result<Vec<i64>> {
        use crate::schema_diesel::symbol_instances;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let layers = symbol_instances::table
            .filter(symbol_instances::id.eq(instance_id))
            // Ephemeral layers only (negative id space) — a root-layer row
            // has no "eph layer" in the sense this accessor serves.
            .filter(symbol_instances::layer.lt(0))
            .select(symbol_instances::layer)
            .load::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get eph layer for instance: {}", e))?;

        Ok(layers)
    }

    /// Count the non-canary ephemeral layers currently in the table.
    /// Used by tests that verify cache reuse (a query that hits the
    /// cache should leave this count unchanged).  The canary is
    /// excluded so the count reflects only request-driven layers.
    pub async fn eph_layer_count(&self) -> Result<i64> {
        use crate::schema_diesel::layers;
        use diesel::dsl::count_star;
        use diesel_async::RunQueryDsl;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        layers::table
            .filter(layers::kind.ne_all(EphLayerKind::PROTECTED.map(|k| k.as_str())))
            .select(count_star())
            .get_result::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to count eph layers: {}", e))
    }

    /// Read the metadata row of a single ephemeral layer.  Used by tests to
    /// assert cache state directly (populated / truncated flags, kind,
    /// parent chaining) instead of inferring it from query results.
    pub async fn eph_layer_meta(&self, layer_id: i64) -> Result<EphLayerMeta> {
        use crate::schema_diesel::layers;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let (id, parent_id, kind, populated, truncated) = layers::table
            .filter(layers::id.eq(layer_id))
            .select((
                layers::id,
                layers::parent_id,
                layers::kind,
                layers::populated,
                layers::truncated,
            ))
            .get_result::<(i64, Option<i64>, String, bool, bool)>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to read eph layer meta: {}", e))?;

        Ok(EphLayerMeta {
            id,
            parent_id,
            kind,
            populated,
            truncated,
        })
    }

    /// Count the symbol and instance rows belonging to a given ephemeral
    /// layer.  Used by tests to prove a cache hit did not re-populate the
    /// layer (counts must be identical before and after the repeat call).
    pub async fn count_eph_rows_for_layer(&self, layer_id: i64) -> Result<(i64, i64)> {
        use crate::schema_diesel::{symbol_instances, symbols};
        use diesel::dsl::count_star;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let symbol_count = symbols::table
            .filter(symbols::layer.eq(layer_id))
            .select(count_star())
            .get_result::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to count eph symbols for layer: {}", e))?;

        let instance_count = symbol_instances::table
            .filter(symbol_instances::layer.eq(layer_id))
            .select(count_star())
            .get_result::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to count eph instances for layer: {}", e))?;

        Ok((symbol_count, instance_count))
    }

    /// Delete ephemeral layers older than the given duration, together with
    /// their full dependent closure.  See [`doomed_closure_delete_sql`] for
    /// why deleting the closure (rather than relying on FK cascades alone)
    /// is a cache-correctness requirement, not an optimization.
    pub async fn purge_old_eph_layers(&self, older_than: Duration) -> Result<u64> {
        use diesel::sql_types::BigInt;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let interval_secs = older_than.as_secs() as i64;
        let result = diesel::sql_query(doomed_closure_delete_sql(
            "last_used < now() - make_interval(secs => $1)",
        ))
        .bind::<BigInt, _>(interval_secs)
        .execute(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to purge old eph layers: {}", e))?;

        Ok(result as u64)
    }

    /// Delete a single ephemeral layer by ID, together with its full
    /// dependent closure (layers whose rows reference this layer's symbols,
    /// transitively).  See [`doomed_closure_delete_sql`].
    pub async fn delete_eph_layer(&self, layer_id: i64) -> Result<()> {
        use diesel::sql_types::BigInt;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::sql_query(doomed_closure_delete_sql("id = $1"))
            .bind::<BigInt, _>(layer_id)
            .execute(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete eph layer: {}", e))?;

        Ok(())
    }

    /// Touch the last_used timestamps of ephemeral layers in one round trip
    /// (throttled: rows fresher than an hour are skipped by the WHERE
    /// clause, so repeated hits cost a no-op statement, not a write).
    pub async fn touch_eph_layers(&self, layer_ids: &[i64]) -> Result<()> {
        if layer_ids.is_empty() {
            return Ok(());
        }
        use crate::schema_diesel::layers;
        use diesel::sql_types::Bool;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::update(layers::table.filter(layers::id.eq_any(layer_ids)).filter(
            diesel::dsl::sql::<Bool>("last_used < now() - interval '1 hour'"),
        ))
        .set(layers::last_used.eq(diesel::dsl::now))
        .execute(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to touch eph layers: {}", e))?;

        Ok(())
    }

    /// Check if a symbol exists in the visible layer set (roots ∪ chain).
    pub async fn symbol_exists(&self, symbol_id: i64, eph: &EphContext) -> Result<bool> {
        use crate::schema_diesel::symbols;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let exists = symbols::table
            .filter(symbols::id.eq(symbol_id))
            .filter(symbols::layer.eq_any(eph.visible_ids()))
            .select(symbols::id)
            .first::<i64>(&mut *connection)
            .await
            .optional()
            .map_err(|e| anyhow::anyhow!("Failed to check symbol existence: {}", e))?;

        Ok(exists.is_some())
    }

    /// Find file objects whose `filesystem_path` equals `path` or ends with it on
    /// a **segment boundary** (`fs/read_write.c` matches `/p/fs/read_write.c`, not
    /// `/p/ecryptfs/read_write.c`), optionally filtered by project name.  Takes a
    /// caller-supplied connection: the only caller is loc's deferred populate
    /// closure, which runs inside an [`EphTransaction`] and must not check out a
    /// second pool connection while the layer's row lock is held (see
    /// [`Index::search_content_matches_on`]).
    ///
    /// Layer-sharded like [`Index::search_content_matches_on`]: `visible_layers`
    /// scopes the resolution to a layer set (the root shard binds the root
    /// ids, an eph shard binds its own layer), and `eph_branch` adds the
    /// eph-disjointness guard (`objects.layer < 0` — negative layers are
    /// ephemeral) so an eph shard only ever sees eph-layer objects.
    pub async fn find_objects_by_path_on(
        connection: &mut AsyncPgConnection,
        path: &str,
        project_name: Option<&str>,
        visible_layers: &[i64],
        eph_branch: bool,
    ) -> Result<Vec<(FileId, crate::symbols::ProjectId)>> {
        use crate::schema_diesel::*;

        let escaped = super::mixins::escape_like(path);
        let mut query = objects::table
            .inner_join(projects::table.on(projects::id.eq(objects::project_id)))
            // Exact path, or a suffix on a segment boundary ("…/<path>"); the
            // leading-slash guard is what keeps `fs/x` off `.../ecryptfs/x`.
            .filter(
                objects::filesystem_path
                    .eq(path)
                    .or(objects::filesystem_path.like(format!("%/{}", escaped))),
            )
            // Scope to the shard's visible layer set (root ids for the root
            // shard, its own layer for an eph shard).
            .filter(objects::layer.eq_any(visible_layers))
            .select((objects::id, projects::id))
            .into_boxed::<Pg>();

        if let Some(name) = project_name {
            query = query.filter(projects::project_name.eq(name));
        }

        // Eph-disjointness guard: an eph shard scans ephemeral objects
        // only (`objects.layer < 0`), so a root-shard object never
        // double-counts as an eph-shard object.
        if eph_branch {
            query = query.filter(objects::layer.lt(0));
        }

        let results = query
            .load::<(i32, i32)>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find objects by path: {}", e))?;

        Ok(results
            .into_iter()
            .map(|(obj_id, proj_id)| (FileId::new(obj_id), crate::symbols::ProjectId::new(proj_id)))
            .collect())
    }

    /// Resolve a source file by exact path or by a path *suffix on a segment
    /// boundary*, optionally within one project — `fs/read_write.c` resolves
    /// `/proj/fs/read_write.c` but not `/proj/fs/ecryptfs/read_write.c` (where the
    /// suffix would fall mid-segment). Built with the query DSL (no raw SQL) and
    /// routed through [`Index::cached_load`], so repeated `askl_read`s hit the RAM
    /// cache; index mutations purge it. Results are exact-first then shortest-path,
    /// so the best candidate leads. The `filesystem_path LIKE '%/…'` predicate is
    /// served by the `objects_filesystem_path_trgm_idx` GIN index.
    pub async fn resolve_source_file(
        &self,
        path: &str,
        project_name: Option<&str>,
    ) -> Result<Vec<ResolvedSourceFile>> {
        use crate::schema_diesel::*;

        // Segment-boundary suffix: exact path, or "…/<path>". LIKE wildcards in
        // the path are escaped so `_`/`%` stay literal.
        let exact = path.to_string();
        let boundary = format!("%/{}", super::mixins::escape_like(path));

        let mut query = objects::table
            .inner_join(projects::table.on(projects::id.eq(objects::project_id)))
            .filter(projects::id.gt(0))
            .filter(
                objects::filesystem_path
                    .eq(exact.clone())
                    .or(objects::filesystem_path.like(boundary)),
            )
            .select((
                objects::id,
                projects::project_name,
                objects::filesystem_path,
            ))
            // Exact matches first so the LIMIT never drops them; the full
            // exact-then-shortest ordering is finalized in Rust below.
            .order(objects::filesystem_path.eq(exact.clone()).desc())
            .limit(RESOLVE_SOURCE_FILE_LIMIT)
            .into_boxed::<Pg>();

        if let Some(name) = project_name {
            query = query.filter(projects::project_name.eq(name.to_string()));
        }

        let rows: std::sync::Arc<Vec<(i32, String, String)>> = self.cached_load(query).await?;

        let mut resolved: Vec<ResolvedSourceFile> = rows
            .iter()
            .map(
                |(object_id, project_name, filesystem_path)| ResolvedSourceFile {
                    object_id: FileId::new(*object_id),
                    project_name: project_name.clone(),
                    filesystem_path: filesystem_path.clone(),
                },
            )
            .collect();
        // Best candidate first: exact match, then shortest path, then name.
        resolved.sort_by(|a, b| {
            let a_exact = a.filesystem_path == exact;
            let b_exact = b.filesystem_path == exact;
            b_exact
                .cmp(&a_exact)
                .then_with(|| a.filesystem_path.len().cmp(&b.filesystem_path.len()))
                .then_with(|| a.filesystem_path.cmp(&b.filesystem_path))
        });
        Ok(resolved)
    }
}

/// One source file resolved by [`Index::resolve_source_file`] from a (possibly
/// partial) path — carries the canonical `filesystem_path` so `askl_read` can
/// show it and disambiguate multiple matches.
#[derive(Debug, Clone)]
pub struct ResolvedSourceFile {
    pub object_id: FileId,
    pub project_name: String,
    pub filesystem_path: String,
}

/// Cap on candidates returned by [`Index::resolve_source_file`]; exact matches
/// are ordered first so the cap can only drop surplus boundary matches.
const RESOLVE_SOURCE_FILE_LIMIT: i64 = 50;

/// Helper for `RETURNING id` on BigInt columns.
#[derive(diesel::QueryableByName)]
struct IdRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    id: i64,
}

/// One byte-range match produced by [`Index::search_content_matches`].
#[derive(diesel::QueryableByName, Debug, Clone, PartialEq, Eq)]
pub struct SearchMatchRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub object_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub project_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub start_byte: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub end_byte: i32,
}

/// One "did you mean?" suggestion from [`Index::suggest_symbol_names`].
/// `QueryableByName` maps only the columns it declares, so the query's
/// `score` (used for `ORDER BY` only) is intentionally not a field here.
#[derive(diesel::QueryableByName, Debug, Clone, PartialEq)]
pub struct NameSuggestionRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    pub name: String,
}

/// LIKE-escape a raw user query and wrap with the surrounding wildcards.  The
/// three LIKE metacharacters (`\`, `%`, `_`) are escaped with a backslash so
/// they match literally in the pre-filter.  Without this, a query like
/// `"mana_ib_reg_user_"` would have each `_` treated as a single-character
/// wildcard, ballooning the pg_trgm recheck candidate set by an order of
/// magnitude — see the EXPLAIN ANALYZE numbers in the plan.
fn make_like_pattern(query: &str) -> String {
    format!("%{}%", super::mixins::escape_like(query))
}

/// Build one of the four search SQL variants.  Each variant uses the same
/// `content_store ⋈ objects ⋈ LATERAL find_substr_byte_ranges(...)` skeleton
/// and differs only in three places — what gets passed to the helper
/// (lowercased haystack/needle vs raw), the pre-filter predicate the GIN
/// indexes can hit, and whether a whole-word boundary check is appended.
///
/// Bind slots:
///   $1 = needle (raw query, used by `find_substr_byte_ranges`'s `position()`)
///   $2 = limit + 1
///   $3 = either `make_like_pattern(query)` (variants 1, 2, 4) OR `visible_project_ids`
///        (variant 3 with project filter) — depends on `uses_like_pattern`
///   $4 = `visible_project_ids` (only when variant uses LIKE AND project filter is present)
///
/// When a project filter is present, the search query adds
/// `AND o.project_id = ANY($N)` on the objects join.  Content shared
/// across projects (same content_hash, different project_id on the
/// referring objects) is naturally scoped: each project's object comes
/// through the same objects JOIN and the ANY() filter keeps only the
/// caller's project's entries.
///
/// PostgreSQL 16 rejects prepared statements with unreferenced parameters
/// ("wrong number of parameters"), so the bind chain — and therefore the
/// project-filter slot — must match exactly which params the SQL uses.
fn build_search_sql(
    whole_word: bool,
    case_sensitive: bool,
    eph_branch: bool,
    scope: Option<&[(i32, i32, i32)]>,
) -> String {
    let uses_like_pattern = !(whole_word && !case_sensitive);

    // Haystack/needle passed to find_substr_byte_ranges.
    let (haystack_expr, needle_expr) = if case_sensitive {
        ("cs.content_text", "$1")
    } else {
        ("lower(cs.content_text)", "lower($1)")
    };

    // Index-hitting pre-filter.  Variants 1/2/4 reference the pre-escaped
    // LIKE pattern bound as $3 (see make_like_pattern above).
    let pre_filter = match (whole_word, case_sensitive) {
        (false, false) => "cs.content_text ILIKE $3",
        (false, true) => "cs.content_text LIKE $3",
        (true, false) => "cs.content_tsv @@ phraseto_tsquery('simple', lower($1))",
        (true, true) => "cs.content_text LIKE $3",
    };

    // Whole-word boundary check appended to WHERE (variants 3 and 4 only).
    // Uses p.start_char against the *original* content_text — `is_word_char`
    // only inspects ASCII ranges so original case is fine.
    let boundary_clauses = if whole_word {
        " AND (p.start_char = 1 \
                OR NOT index.is_word_char(substring(cs.content_text from p.start_char - 1 for 1))) \
           AND (p.start_char + char_length($1) > char_length(cs.content_text) \
                OR NOT index.is_word_char(substring(cs.content_text from p.start_char + char_length($1) for 1)))"
    } else {
        ""
    };

    // Project-level scoping: ALWAYS present.  Per-root populate scopes
    // every search to one project; the caller intersects the composite
    // filter's resolved project set with the root's project and binds the
    // result (possibly empty — zero rows, the uniform empty shard).  Content
    // shared across projects is naturally scoped: for each cs row the JOIN
    // produces one object per project that references it, and
    // `o.project_id = ANY($N)` keeps only the caller's.  Slot shifts based
    // on whether $3 holds the LIKE pattern.
    let (proj_slot, layer_slot) = if uses_like_pattern {
        ("$4", "$5")
    } else {
        ("$3", "$4")
    };
    let project_filter = format!(" AND o.project_id = ANY({proj_slot})");
    let layer_filter = format!(
        " AND o.layer = ANY({layer_slot}){}",
        if eph_branch { " AND o.layer < 0" } else { "" }
    );
    // Container scope-fusion.  The resolved (object, [lo,hi)) ranges are inlined
    // as literals (trusted DB integers, never user text) so the object-level
    // `o.id IN (...)` is a plain top-level predicate the planner pushes BEFORE
    // the LATERAL — pruning `find_substr_byte_ranges` to the container's objects
    // (the actual speedup).  The `EXISTS (VALUES …)` then refines to the byte
    // range for a finer container (a `func` body); for a whole-object container
    // (`file`/`dir`) it is trivially satisfied.  `None` => unscoped (no clause);
    // an empty scope => the container matched nothing => `AND FALSE`.
    let scope_filter = match scope {
        None => String::new(),
        Some(ranges) if ranges.is_empty() => " AND FALSE".to_string(),
        Some(ranges) => {
            let mut oids: Vec<i32> = ranges.iter().map(|r| r.0).collect();
            oids.sort_unstable();
            oids.dedup();
            let oid_list = oids
                .iter()
                .map(|o| o.to_string())
                .collect::<Vec<_>>()
                .join(",");
            let values = ranges
                .iter()
                .map(|(o, lo, hi)| format!("({o},{lo},{hi})"))
                .collect::<Vec<_>>()
                .join(",");
            format!(
                " AND o.id IN ({oid_list}) \
                 AND EXISTS (SELECT 1 FROM (VALUES {values}) AS c(oid, lo, hi) \
                     WHERE c.oid = o.id AND p.start_byte >= c.lo AND p.start_byte < c.hi)"
            )
        }
    };

    // No ORDER BY: with a bare `LIMIT $2` Postgres can stop the LATERAL recheck
    // as soon as limit+1 matches exist, instead of ranking every GIN candidate
    // before truncating.  Truncation is therefore non-deterministic in *which*
    // matches drop (the count and the `truncated` flag are unaffected) — an
    // accepted trade-off for the dense-common-term case.  Re-sort the ≤limit set
    // in Rust if a stable display order is ever needed.
    format!(
        "SELECT \
            o.id AS object_id, \
            o.project_id, \
            p.start_byte, \
            p.end_byte \
         FROM index.content_store cs \
         JOIN index.objects o ON o.content_hash = cs.content_hash \
         CROSS JOIN LATERAL index.find_substr_byte_ranges({haystack}, {needle}, $2) AS p \
         WHERE {pre_filter}{project_filter}{layer_filter}{scope_filter}{boundary} \
         LIMIT $2",
        haystack = haystack_expr,
        needle = needle_expr,
        pre_filter = pre_filter,
        project_filter = project_filter,
        layer_filter = layer_filter,
        scope_filter = scope_filter,
        boundary = boundary_clauses,
    )
}

impl Index {
    /// Find every byte-range occurrence of `query` in the content of indexed
    /// objects, subject to the surrounding command's `composite_filter`.
    ///
    /// The candidate query joins `content_store` to `objects` directly so
    /// files without indexed symbols (docs, configs, headers) are reachable.
    /// `composite_filter.compose_objects()` is materialised as a `Vec<i32>`
    /// of visible object_ids and bound into the SQL; selectors that don't
    /// constrain objects (most filters) yield no extra clause.
    ///
    /// Returns at most `limit` matches.  The fence is enforced via SQL
    /// `LIMIT N+1`: if more than `limit` rows came back, the (limit+1)th is
    /// dropped and `truncated = true` is returned so the caller can persist
    /// the flag on the layer and surface the warning.
    ///
    /// Takes a caller-supplied connection: the only caller is search's
    /// deferred populate closure, which runs inside an [`EphTransaction`]
    /// and must not check out a second pool connection while the layer's
    /// row lock is held (a pool-exhaustion deadlock under concurrent
    /// searches).
    pub async fn search_content_matches_on(
        connection: &mut AsyncPgConnection,
        query: &str,
        case_sensitive: bool,
        whole_word: bool,
        composite_filter: &CompositeFilter,
        limit: usize,
        project_id: i32,
        visible_layers: &[i64],
        eph_branch: bool,
        scope: Option<&[(i32, i32, i32)]>,
    ) -> Result<(Vec<SearchMatchRow>, bool)> {
        use crate::schema_diesel::objects;
        use diesel::sql_types::{Array, BigInt, Integer, Text};

        let limit_plus_one = (limit as i32).saturating_add(1);

        // Per-root scoping is computed as bind PREPARATION (not row
        // filtering): when the composite filter constrains objects (today:
        // ProjectFilterMixin via objects_expr), a single indexed EXISTS
        // probe answers the only question asked — "does this root's project
        // pass the filter" — instead of loading every matching project's
        // id.  With no filter, the root's project passes through.  The
        // resulting bind is `[project_id]` or `[]` — an empty array yields
        // zero rows, the uniform empty shard for a filtered-out root.  No
        // special-casing per filter type — any future FilterLeaf that
        // implements objects_expr is picked up automatically (the typed
        // expression is applied verbatim inside the probe).
        let passes = match composite_filter.compose_objects() {
            Some(expr) => diesel::select(diesel::dsl::exists(
                objects::table
                    .filter(expr)
                    .filter(objects::project_id.eq(project_id)),
            ))
            .get_result::<bool>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to probe project visibility for search: {}", e))?,
            None => true,
        };
        let project_ids: Vec<i32> = if passes { vec![project_id] } else { Vec::new() };

        // Variant 3 (whole_word=true, case_sensitive=false) uses the tsvector
        // pre-filter and does not reference $3; binding the LIKE pattern
        // anyway makes PostgreSQL 16 reject the prepared statement
        // ("wrong number of parameters").  Bind $3 only when the SQL
        // actually uses it.
        let uses_like_pattern = !(whole_word && !case_sensitive);
        let like_pattern = if uses_like_pattern {
            Some(make_like_pattern(query))
        } else {
            None
        };

        let sql = build_search_sql(whole_word, case_sensitive, eph_branch, scope);

        let rows: Vec<SearchMatchRow> = match like_pattern.as_ref() {
            Some(pat) => {
                diesel::sql_query(&sql)
                    .bind::<Text, _>(query)
                    .bind::<Integer, _>(limit_plus_one)
                    .bind::<Text, _>(pat)
                    .bind::<Array<Integer>, _>(&project_ids)
                    .bind::<Array<BigInt>, _>(visible_layers)
                    .load::<SearchMatchRow>(&mut *connection)
                    .await
            }
            None => {
                diesel::sql_query(&sql)
                    .bind::<Text, _>(query)
                    .bind::<Integer, _>(limit_plus_one)
                    .bind::<Array<Integer>, _>(&project_ids)
                    .bind::<Array<BigInt>, _>(visible_layers)
                    .load::<SearchMatchRow>(&mut *connection)
                    .await
            }
        }
        .map_err(|e| anyhow::anyhow!("Failed to run search content query: {}", e))?;

        let truncated = rows.len() > limit;
        let matches: Vec<SearchMatchRow> = rows.into_iter().take(limit).collect();
        Ok((matches, truncated))
    }
}

/// Edge discovered between two selected instances via DB query.
///
/// Derives both `QueryableByName` (for legacy raw `sql_query` callers)
/// and `Queryable` (for the typed `CteFindEdgesBetween` wrapper that
/// loads positionally from a tuple `SqlType`).
#[derive(diesel::QueryableByName, diesel::Queryable, Debug, Clone)]
pub struct ImplicitEdge {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub ref_id: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub to_symbol: i64,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub from_object: i32,
    #[diesel(sql_type = diesel::sql_types::Int4range)]
    pub from_offset_range: (std::ops::Bound<i32>, std::ops::Bound<i32>),
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub to_instance_id: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub from_instance_id: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub sr_layer: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub from_layer: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    pub to_layer: i64,
}

/// Holds a pooled connection with an open transaction for atomic ephemeral layer
/// create+populate. Created by `Index::create_eph_layer`.
///
/// The caller must call `commit()` or `rollback()`. If dropped without either
/// (e.g. early `?` return), the pool's `RecyclingMethod::CustomQuery("ROLLBACK")`
/// cleans up the stale transaction on next checkout.
pub struct EphTransaction<'a> {
    conn: bb8::PooledConnection<'a, AsyncPgConnection>,
    layer_id: i64,
    created: bool,
    /// Value of `layers.truncated` as observed when the layer was opened.
    /// On a cache hit this is what the layer's original creator wrote.  On a
    /// cache miss it is the freshly-inserted default (false); the populate
    /// callback may flip it via [`mark_truncated`].
    truncated_on_open: bool,
    finished: bool,
}

impl<'a> EphTransaction<'a> {
    pub fn layer_id(&self) -> i64 {
        self.layer_id
    }

    /// The transaction's own connection, for populate closures that defer
    /// their (read-only) queries into the cache-miss path — e.g.
    /// [`Index::search_content_matches_on`].  The connection is inside the
    /// open layer transaction; callers must not issue transaction-control
    /// statements on it.
    pub fn connection(&mut self) -> &mut AsyncPgConnection {
        &mut *self.conn
    }

    pub fn created(&self) -> bool {
        self.created
    }

    /// Whether this layer was already marked truncated when opened.  Used by
    /// [`Index::with_eph_layer`] to return the right `truncated` value on a
    /// cache hit (when the populate callback does not run).
    pub fn truncated_on_open(&self) -> bool {
        self.truncated_on_open
    }

    /// Insert a batch of rows into this layer, scoped to one project: rows
    /// whose object (or explicit `project_id`, for symbols) does not belong
    /// to `project_id` are filtered out by the SQL itself — a per-root layer
    /// contains exactly one project's rows, and the partitioning lives in
    /// the insert statements, never in Rust.  Note the resulting
    /// micro-semantic: an instance/ref naming an object outside the project
    /// is silently dropped by the JOIN rather than raising an FK error
    /// (only reachable via hand-written `layer {}` ops; these inserts are
    /// already non-exact via ON CONFLICT DO NOTHING).
    /// Returns the IDs of inserted symbols (in insertion order).
    pub async fn insert_batch(&mut self, batch: &LayerBatch, project_id: i32) -> Result<Vec<i64>> {
        let symbol_ids = self
            .insert_symbols_scoped(&batch.symbols, project_id)
            .await?;
        self.insert_instances_scoped(&batch.instances, project_id)
            .await?;
        self.insert_refs_scoped(&batch.refs, project_id).await?;
        Ok(symbol_ids)
    }

    async fn insert_symbols_scoped(
        &mut self,
        rows: &[EphSymbolRow],
        project_id: i32,
    ) -> Result<Vec<i64>> {
        if rows.is_empty() {
            return Ok(Vec::new());
        }
        use diesel::sql_types::{Array, BigInt, Integer, Nullable, Text};

        let layer_id = self.layer_id;
        let conn = &mut *self.conn;

        let names: Vec<&str> = rows.iter().map(|r| r.name.as_str()).collect();
        let paths: Vec<&str> = rows.iter().map(|r| r.path.as_str()).collect();
        let project_ids: Vec<i32> = rows.iter().map(|r| r.project_id).collect();
        let symbol_types: Vec<i32> = rows.iter().map(|r| r.symbol_type).collect();
        let scopes: Vec<Option<i32>> = rows.iter().map(|r| r.scope).collect();
        let leaf_names: Vec<&str> = rows.iter().map(|r| r.leaf_name.as_str()).collect();

        let inserted: Vec<IdRow> = diesel::sql_query(
            "INSERT INTO index.symbols (id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name, layer) \
             SELECT nextval('index.eph_symbol_id_seq'), \
                    t.name, t.path::ltree, t.project_id, t.symbol_type, t.scope, t.leaf_name, $7 \
             FROM UNNEST($1::text[], $2::text[], $3::int4[], $4::int4[], $5::int4[], $6::text[]) \
             AS t(name, path, project_id, symbol_type, scope, leaf_name) \
             WHERE t.project_id = $8 \
             RETURNING id"
        )
            .bind::<Array<Text>, _>(&names)
            .bind::<Array<Text>, _>(&paths)
            .bind::<Array<Integer>, _>(&project_ids)
            .bind::<Array<Integer>, _>(&symbol_types)
            .bind::<Array<Nullable<Integer>>, _>(&scopes)
            .bind::<Array<Text>, _>(&leaf_names)
            .bind::<BigInt, _>(layer_id)
            .bind::<Integer, _>(project_id)
            .get_results(conn)
            .await
            .map_err(|e| explain_eph_insert_err("Failed to batch insert eph symbols", e))?;

        Ok(inserted.into_iter().map(|r| r.id).collect())
    }

    async fn insert_instances_scoped(
        &mut self,
        rows: &[EphInstanceRow],
        project_id: i32,
    ) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        use diesel::sql_types::{Array, BigInt, Integer};

        let layer_id = self.layer_id;
        let conn = &mut *self.conn;

        let sym_ids: Vec<i64> = rows.iter().map(|r| r.symbol_id).collect();
        let object_ids: Vec<i32> = rows.iter().map(|r| r.object_id).collect();
        let starts: Vec<i32> = try_offsets(rows.iter().map(|r| r.start), "start")?;
        let ends: Vec<i32> = try_offsets(rows.iter().map(|r| r.end), "end")?;
        let instance_types: Vec<i32> = rows.iter().map(|r| r.instance_type).collect();

        diesel::sql_query(
            "INSERT INTO index.symbol_instances (id, symbol, object_id, offset_range, instance_type, layer) \
             SELECT nextval('index.eph_instance_id_seq'), \
                    t.symbol_id, t.object_id, int4range(t.start_off, t.end_off), t.instance_type, $6 \
             FROM UNNEST($1::int8[], $2::int4[], $3::int4[], $4::int4[], $5::int4[]) \
             AS t(symbol_id, object_id, start_off, end_off, instance_type) \
             JOIN index.objects o ON o.id = t.object_id AND o.project_id = $7 \
             ON CONFLICT DO NOTHING"
        )
            .bind::<Array<BigInt>,  _>(&sym_ids)
            .bind::<Array<Integer>, _>(&object_ids)
            .bind::<Array<Integer>, _>(&starts)
            .bind::<Array<Integer>, _>(&ends)
            .bind::<Array<Integer>, _>(&instance_types)
            .bind::<BigInt, _>(layer_id)
            .bind::<Integer, _>(project_id)
            .execute(conn)
            .await
            .map_err(|e| explain_eph_insert_err("Failed to batch insert eph instances", e))?;
        Ok(())
    }

    async fn insert_refs_scoped(&mut self, rows: &[EphRefRow], project_id: i32) -> Result<()> {
        if rows.is_empty() {
            return Ok(());
        }
        use diesel::sql_types::{Array, BigInt, Integer};

        let layer_id = self.layer_id;
        let conn = &mut *self.conn;

        let to_symbols: Vec<i64> = rows.iter().map(|r| r.to_symbol).collect();
        let from_objects: Vec<i32> = rows.iter().map(|r| r.from_object).collect();
        let starts: Vec<i32> = try_offsets(rows.iter().map(|r| r.start), "start")?;
        let ends: Vec<i32> = try_offsets(rows.iter().map(|r| r.end), "end")?;

        diesel::sql_query(
            "INSERT INTO index.symbol_refs (id, to_symbol, from_object, from_offset_range, layer) \
             SELECT nextval('index.eph_ref_id_seq'), \
                    t.to_symbol, t.from_object, int4range(t.start_off, t.end_off), $5 \
             FROM UNNEST($1::int8[], $2::int4[], $3::int4[], $4::int4[]) \
             AS t(to_symbol, from_object, start_off, end_off) \
             JOIN index.objects o ON o.id = t.from_object AND o.project_id = $6 \
             ON CONFLICT DO NOTHING",
        )
        .bind::<Array<BigInt>, _>(&to_symbols)
        .bind::<Array<Integer>, _>(&from_objects)
        .bind::<Array<Integer>, _>(&starts)
        .bind::<Array<Integer>, _>(&ends)
        .bind::<BigInt, _>(layer_id)
        .bind::<Integer, _>(project_id)
        .execute(conn)
        .await
        .map_err(|e| explain_eph_insert_err("Failed to batch insert eph refs", e))?;
        Ok(())
    }

    /// Flip the 2-PC `populated` flag for this layer to `TRUE`.  Called by
    /// [`Index::with_eph_layer`] just before COMMIT once the populate batch
    /// has succeeded.  Must run inside the same transaction as the populate
    /// inserts so the flag and the rows commit atomically.
    pub async fn mark_populated(&mut self) -> Result<()> {
        use crate::schema_diesel::layers;
        diesel::update(layers::table.filter(layers::id.eq(self.layer_id)))
            .set(layers::populated.eq(true))
            .execute(&mut *self.conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to mark eph layer populated: {}", e))?;
        Ok(())
    }

    /// Record whether the populate batch was truncated by a result cap.  Called
    /// by [`Index::with_eph_layer`] just before COMMIT on a freshly-inserted
    /// layer (cache miss).  Cache hits do NOT call this — they inherit the
    /// value already on the row (see `truncated_on_open`).
    pub async fn mark_truncated(&mut self, truncated: bool) -> Result<()> {
        use crate::schema_diesel::layers;
        diesel::update(layers::table.filter(layers::id.eq(self.layer_id)))
            .set(layers::truncated.eq(truncated))
            .execute(&mut *self.conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to mark eph layer truncated: {}", e))?;
        Ok(())
    }

    /// COMMIT the transaction. Consumes self, returns the connection to the pool.
    pub async fn commit(mut self) -> Result<()> {
        diesel::sql_query("COMMIT")
            .execute(&mut *self.conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to COMMIT eph transaction: {}", e))?;
        self.finished = true;
        Ok(())
    }

    /// ROLLBACK the transaction. Consumes self.
    pub async fn rollback(mut self) -> Result<()> {
        let _ = diesel::sql_query("ROLLBACK").execute(&mut *self.conn).await;
        self.finished = true;
        Ok(())
    }
}

impl Drop for EphTransaction<'_> {
    fn drop(&mut self) {
        if !self.finished {
            // Cannot issue async ROLLBACK from Drop. The pool's
            // RecyclingMethod::CustomQuery("ROLLBACK") will clean up the stale
            // transaction when this connection is next checked out.
            tracing::error!(
                layer_id = self.layer_id,
                "EphTransaction dropped without commit/rollback — \
                 pool will ROLLBACK on next checkout"
            );
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Per-root root-shard identity: identical verb inputs against different
    /// roots key different root shards, the fold is stable per (root, hash),
    /// and — the reuse property — the domain tag is disjoint from the older
    /// multi-root fold so upgrades strand (rather than alias) old cache rows.
    #[test]
    fn root_shard_hash_scopes_cache_per_root() {
        let verb_hash = [7u8; 32];
        let r1 = RootLayer {
            project_id: 1,
            id: 1,
            hash: vec![0x11; 32],
        };
        let r2 = RootLayer {
            project_id: 2,
            id: 2,
            hash: vec![0x22; 32],
        };

        let k1 = root_shard_hash(&r1, &verb_hash);
        let k2 = root_shard_hash(&r2, &verb_hash);
        assert_ne!(k1, k2, "different roots must key different root shards");
        assert_ne!(k1, verb_hash, "salting must change the verb hash");
        assert_eq!(
            k1,
            root_shard_hash(&r1, &verb_hash),
            "same (root, verb hash) must fold deterministically"
        );
    }

    /// Layer-shard keys: distinct per (layer, root shard), deterministic, and
    /// disjoint from the selection-shard key family and the raw root-shard
    /// hash — the property the `layer-shard-v1` domain tag exists to
    /// guarantee.
    #[test]
    fn layer_shard_hash_is_layer_scoped_and_disjoint() {
        let root_shard = [9u8; 32];
        let k1 = layer_shard_hash(-1001, &root_shard);
        let k2 = layer_shard_hash(-1002, &root_shard);
        assert_ne!(k1, k2, "different layers must key different layer shards");
        assert_eq!(
            k1,
            layer_shard_hash(-1001, &root_shard),
            "same (layer, root shard) must fold deterministically"
        );
        // A different root shard (different verb inputs) ⇒ a different
        // layer-shard key.
        assert_ne!(
            k1,
            layer_shard_hash(-1001, &[1u8; 32]),
            "a different root-shard hash must key a different layer shard"
        );
        // Disjoint from the selection-shard key over the SAME ids — the domain
        // tags (`layer-shard-v1` vs `selection-shard-v1`) keep them apart.
        assert_ne!(
            k1,
            selection_shard_hash(-1001, &root_shard, &[]),
            "layer-shard and selection-shard keys must never collide"
        );
        assert_ne!(
            k1, root_shard,
            "a layer-shard key must differ from the root-shard hash it folds"
        );
    }

    /// Pins the disjointness property of the partition at the SQL level,
    /// with no database: the eph-branch instantiation must render the
    /// `NOT (...all recorded columns = ANY($roots)...)` guard over every
    /// selected-row eph column, and the persistent instantiation must not
    /// embed the chain ids anywhere (its rendered SQL + binds are the cache
    /// key — chain-freedom IS the sharing guarantee; root ids in the binds
    /// are by design, they scope the key to the visible projects).
    #[test]
    fn branch_sql_renders_guard_and_stays_chain_free() {
        let mut eph = EphContext::rooted(vec![RootLayer {
            project_id: 1,
            id: 424242,
            hash: vec![0x42; 32],
        }]);
        eph.push_materialisation(&[(424242, -7)]);

        let evis = EphVisibility::eph_touching(&eph);
        let eq = build_parents_query(vec![1, 2], &evis).filter(evis.guard());
        let esql = format!("{:?}", diesel::debug_query::<Pg, _>(&eq));
        for col in [
            "symbol_refs.layer",
            "symbols.layer",
            "symbol_instances.layer",
            "parent_decls.layer",
            "parent_symbols.layer",
        ] {
            assert!(
                esql.contains(&format!("{col} = ANY(")),
                "eph branch must render visibility for {col}, got: {esql}"
            );
        }
        assert!(
            esql.contains(
                "NOT (symbol_refs.layer > 0 AND symbols.layer > 0 AND \
                 symbol_instances.layer > 0 AND parent_decls.layer > 0 AND \
                 parent_symbols.layer > 0)"
            ),
            "eph branch must render the bind-free sign-form disjointness guard \
             over all five recorded columns, got: {esql}"
        );

        let pvis = EphVisibility::root_only(&eph);
        let pq = build_parents_query(vec![1, 2], &pvis).filter(pvis.guard());
        let psql = format!("{:?}", diesel::debug_query::<Pg, _>(&pq));
        assert!(
            psql.contains("symbols.layer = ANY("),
            "persistent branch pins columns to the root set, got: {psql}"
        );
        assert!(
            psql.contains("424242"),
            "persistent branch binds the root ids — they scope the cache key \
             to the visible projects, got: {psql}"
        );
        assert!(
            !psql.contains("-7"),
            "persistent branch must not embed chain ids — its rendered SQL is \
             the chain-free cache key, got: {psql}"
        );
    }

    // ===================================================================
    // N-way layer sharding.
    //
    // Nothing in production writes ephemeral CONTENT (objects/content_store
    // on a negative layer), so N-way is byte-identical to 2-way until such a
    // feature exists.  These tests hand-seed synthetic eph content — the only
    // way to make `V(L_k)` non-empty — to prove: (a) the shard-set
    // enumeration, (b) the content scan is a union-homomorphism over layers
    // `V([L1,L2]) = V(L1) ∪ V(L2)`, and (c) the headline caching property —
    // extending the chain re-materialises ONLY the new layer's shard while
    // every prior layer shard (and the root shard) is a cache hit.
    // ===================================================================

    /// Seed a positive root layer plus two ephemeral CONTENT layers (-1001,
    /// -1002) chained under it, each carrying one object whose content holds
    /// the literal "NWAYNEEDLE".  The sign check `(id>0)=(layer>0)` is honoured:
    /// negative object ids live on negative layers.
    async fn seed_nway_content(index: &Index) -> Result<()> {
        use diesel_async::RunQueryDsl;
        let conn = &mut index
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        diesel::sql_query(
            "INSERT INTO index.layers (id, parent_id, hash, kind, populated) \
             OVERRIDING SYSTEM VALUE VALUES \
             (1000001, NULL, decode(md5('nway-root'),'hex'), 'root', TRUE), \
             (-1001, 1000001, decode(md5('nway-eph-1'),'hex'), 'layer', TRUE), \
             (-1002, -1001, decode(md5('nway-eph-2'),'hex'), 'layer', TRUE)",
        )
        .execute(&mut *conn)
        .await?;
        diesel::sql_query(
            "INSERT INTO index.projects (id, project_name, root_path, root_layer_id) \
             VALUES (1, 'nway_proj', '/p1', 1000001)",
        )
        .execute(&mut *conn)
        .await?;
        diesel::sql_query(
            "INSERT INTO index.content_store (content_hash, content) VALUES \
             ('nway_cs_1', E'NWAYNEEDLE alpha'), \
             ('nway_cs_2', E'NWAYNEEDLE beta')",
        )
        .execute(&mut *conn)
        .await?;
        diesel::sql_query(
            "INSERT INTO index.objects \
             (id, project_id, module_path, filesystem_path, filetype, content_hash, layer) \
             VALUES \
             (-2001, 1, 'nway1.c', '/p1/nway1.c', 'cc', 'nway_cs_1', -1001), \
             (-2002, 1, 'nway2.c', '/p1/nway2.c', 'cc', 'nway_cs_2', -1002)",
        )
        .execute(&mut *conn)
        .await?;
        Ok(())
    }

    fn nway_root() -> RootLayer {
        RootLayer {
            project_id: 1,
            id: 1000001,
            hash: vec![0x11; 32],
        }
    }

    // No-op populates: the cache-reuse property is a function of the layer
    // shard's KEY and layer row, not its contents, so these tests need no real
    // scan.
    fn nway_noop_root_shard<'b>(
        _txn: &'b mut EphTransaction<'_>,
        _root: &'b RootLayer,
    ) -> EphScopedFut<'b, bool> {
        Box::pin(async { Ok(false) })
    }
    fn nway_noop_selection_shard<'b>(
        _txn: &'b mut EphTransaction<'_>,
        _root: &'b RootLayer,
        _root_shard_ref: RootShardRef,
    ) -> EphScopedFut<'b, bool> {
        Box::pin(async { Ok(false) })
    }
    fn nway_noop_layer_shard<'b>(
        _txn: &'b mut EphTransaction<'_>,
        _root: &'b RootLayer,
        _layer_id: i64,
        _root_shard_ref: RootShardRef,
    ) -> EphScopedFut<'b, bool> {
        Box::pin(async { Ok(false) })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nway_shard_scan_unions_per_layer_and_enumerates_content() -> Result<()> {
        let docker = testcontainers::clients::Cli::default();
        let (_node, url) = crate::symbols_test::start_postgres(&docker);
        crate::symbols_test::wait_for_postgres(&url).await?;
        let index = Index::connect(&url).await?;
        seed_nway_content(&index).await?;

        // (a) Shard set: content-bearing eph layers, grouped by project,
        //     filtered to layer<0.  Root-only visibility yields none (roots
        //     are positive), which is exactly why production stays 2-way.
        let grouped = index
            .eph_content_layers_grouped(&[1000001, -1001, -1002])
            .await?;
        let mut got = grouped.get(&1).cloned().unwrap_or_default();
        got.sort();
        assert_eq!(
            got,
            vec![-1002, -1001],
            "both eph content layers of project 1 must be enumerated"
        );
        assert!(
            index
                .eph_content_layers_grouped(&[1000001])
                .await?
                .is_empty(),
            "root-only visibility ⇒ empty shard set (no eph content) ⇒ prod byte-identical"
        );

        // (b) The content scan is a union-homomorphism over layers: each
        //     single-layer shard is a disjoint singleton, and the two-layer
        //     shard is exactly their union.
        let conn = &mut index.pool.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        let filter = CompositeFilter::and(vec![]);
        let (m1, _) = Index::search_content_matches_on(
            &mut *conn,
            "NWAYNEEDLE",
            false,
            false,
            &filter,
            500,
            1,
            &[-1001],
            true,
            None,
        )
        .await?;
        assert_eq!(
            m1.iter().map(|m| m.object_id).collect::<Vec<_>>(),
            vec![-2001],
            "V(L=-1001) = the object on that layer"
        );
        let (m2, _) = Index::search_content_matches_on(
            &mut *conn,
            "NWAYNEEDLE",
            false,
            false,
            &filter,
            500,
            1,
            &[-1002],
            true,
            None,
        )
        .await?;
        assert_eq!(
            m2.iter().map(|m| m.object_id).collect::<Vec<_>>(),
            vec![-2002],
            "V(L=-1002) = the object on that layer"
        );
        let (m12, _) = Index::search_content_matches_on(
            &mut *conn,
            "NWAYNEEDLE",
            false,
            false,
            &filter,
            500,
            1,
            &[-1001, -1002],
            true,
            None,
        )
        .await?;
        let mut ids: Vec<i32> = m12.iter().map(|m| m.object_id).collect();
        ids.sort();
        assert_eq!(
            ids,
            vec![-2002, -2001],
            "V([L1,L2]) must equal V(L1) ∪ V(L2)"
        );

        // Scope-fusion: constrain matches to object -2001's whole byte-range —
        // only its match survives, -2002's is dropped.
        let (m_scoped, _) = Index::search_content_matches_on(
            &mut *conn,
            "NWAYNEEDLE",
            false,
            false,
            &filter,
            500,
            1,
            &[-1001, -1002],
            true,
            Some(&[(-2001, 0, i32::MAX)]),
        )
        .await?;
        assert_eq!(
            m_scoped.iter().map(|m| m.object_id).collect::<Vec<_>>(),
            vec![-2001],
            "scope=(-2001, whole range) keeps only -2001's match"
        );

        // An empty scope means the container resolved to nothing => no matches.
        let (m_empty_scope, _) = Index::search_content_matches_on(
            &mut *conn,
            "NWAYNEEDLE",
            false,
            false,
            &filter,
            500,
            1,
            &[-1001, -1002],
            true,
            Some(&[]),
        )
        .await?;
        assert!(
            m_empty_scope.is_empty(),
            "an empty scope (container matched nothing) drops all matches"
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nway_layer_shards_reuse_across_chain_extension() -> Result<()> {
        let docker = testcontainers::clients::Cli::default();
        let (_node, url) = crate::symbols_test::start_postgres(&docker);
        crate::symbols_test::wait_for_postgres(&url).await?;
        let index = Index::connect(&url).await?;
        seed_nway_content(&index).await?;

        let input_hash = [42u8; 32];

        // Run A: chain = [L=-1001].
        let mut eph_a = EphContext::rooted(vec![nway_root()]);
        eph_a.push_materialisation(&[(1000001, -1001)]);
        let a = index
            .with_partitioned_layers(
                &eph_a,
                &input_hash,
                &[],
                nway_noop_root_shard,
                nway_noop_selection_shard,
                Some(nway_noop_layer_shard),
            )
            .await?;

        let root_shard_a = a
            .iter()
            .find(|l| l.role == ShardRole::Root)
            .expect("a root shard");
        assert!(
            root_shard_a.outcome.created,
            "the root shard is cold on first run"
        );
        let layer_shards_a: Vec<_> = a.iter().filter(|l| l.role == ShardRole::Layer).collect();
        assert_eq!(
            layer_shards_a.len(),
            1,
            "one visible content layer ⇒ one layer shard, got {:?}",
            a
        );
        assert!(
            layer_shards_a[0].outcome.created,
            "the layer shard is cold on first run"
        );
        assert_eq!(
            index
                .eph_layer_meta(layer_shards_a[0].outcome.layer_id)
                .await?
                .parent_id,
            Some(-1001),
            "the layer shard parents on the layer it shards"
        );
        let layer_shard_a_id = layer_shards_a[0].outcome.layer_id;
        let root_shard_a_id = root_shard_a.outcome.layer_id;

        // Ordering even for a single-layer-shard run: root shard → layer shard
        // → selection shard, so the selection shard is the deterministic tip.
        assert_eq!(
            a.get(0).map(|l| l.role),
            Some(ShardRole::Root),
            "the root shard must be first, got {:?}",
            a
        );
        assert_eq!(
            a.last().map(|l| l.role),
            Some(ShardRole::Selection),
            "the selection shard must be last (tip) on run A, got {:?}",
            a
        );

        // Run B: chain = [L=-1001, L=-1002] — a new tip appended.
        let mut eph_b = EphContext::rooted(vec![nway_root()]);
        eph_b.push_materialisation(&[(1000001, -1001)]);
        eph_b.push_materialisation(&[(1000001, -1002)]);
        let b = index
            .with_partitioned_layers(
                &eph_b,
                &input_hash,
                &[],
                nway_noop_root_shard,
                nway_noop_selection_shard,
                Some(nway_noop_layer_shard),
            )
            .await?;

        // The root shard is chain-independent, so it is reused unchanged.
        let root_shard_b = b
            .iter()
            .find(|l| l.role == ShardRole::Root)
            .expect("a root shard");
        assert!(
            !root_shard_b.outcome.created,
            "the root shard must be reused across the chain extension"
        );
        assert_eq!(
            root_shard_b.outcome.layer_id, root_shard_a_id,
            "same root-shard row"
        );

        // The headline: the -1001 layer shard reappears by the SAME layer_id as
        // a cache hit; only the newly-appended -1002 layer's shard is cold.
        // Under 2-way the whole selection shard went cold here.
        let layer_shards_b: Vec<_> = b.iter().filter(|l| l.role == ShardRole::Layer).collect();
        assert_eq!(
            layer_shards_b.len(),
            2,
            "two visible content layers ⇒ two layer shards, got {:?}",
            b
        );
        let reused = layer_shards_b
            .iter()
            .find(|l| l.outcome.layer_id == layer_shard_a_id)
            .expect("the L=-1001 layer shard must reappear by the same layer_id");
        assert!(
            !reused.outcome.created,
            "the non-tip layer shard must be a cache HIT — the core N-way property"
        );
        assert_eq!(
            index
                .eph_layer_meta(reused.outcome.layer_id)
                .await?
                .parent_id,
            Some(-1001),
        );
        let fresh: Vec<_> = layer_shards_b
            .iter()
            .filter(|l| l.outcome.layer_id != layer_shard_a_id)
            .collect();
        assert_eq!(fresh.len(), 1, "exactly one new layer shard");
        assert!(
            fresh[0].outcome.created,
            "only the new tip layer's shard is materialised cold"
        );
        assert_eq!(
            index
                .eph_layer_meta(fresh[0].outcome.layer_id)
                .await?
                .parent_id,
            Some(-1002),
            "the fresh layer shard covers the newly-appended tip L=-1002"
        );

        // Ordering / tip: the (empty) selection shard is emitted LAST, so it
        // — not a nondeterministically-ordered layer shard — is the root's
        // tip, keeping downstream selection-shard keys stable.
        assert_eq!(
            b.last().map(|l| l.role),
            Some(ShardRole::Selection),
            "the selection shard must be the last (tip) layer, got {:?}",
            b
        );
        assert!(
            b.iter().rposition(|l| l.role == ShardRole::Layer).unwrap()
                < b.iter()
                    .rposition(|l| l.role == ShardRole::Selection)
                    .unwrap(),
            "every layer shard must precede the selection shard, got {:?}",
            b
        );
        Ok(())
    }

    // A layer-shard populate that materialises the shard's real content: it
    // finds the object seeded on `L_j` and inserts one instance for it,
    // mirroring what `sharded_scan`'s scan does.  Used to prove that with the
    // selection shard a no-op, reading across root + layer + selection shards
    // returns each match exactly ONCE (no double-count).
    fn nway_content_layer_shard<'b>(
        txn: &'b mut EphTransaction<'_>,
        root: &'b RootLayer,
        layer_id: i64,
        _root_shard_ref: RootShardRef,
    ) -> EphScopedFut<'b, bool> {
        Box::pin(async move {
            let filter = CompositeFilter::and(vec![]);
            let (matches, _) = Index::search_content_matches_on(
                txn.connection(),
                "NWAYNEEDLE",
                false,
                false,
                &filter,
                500,
                root.project_id,
                &[layer_id],
                true,
                None,
            )
            .await?;
            if matches.is_empty() {
                return Ok(false);
            }
            // `path` is an ltree, so it must be a valid label (no `:`/`-`).
            let label = format!("nway_{}", layer_id.unsigned_abs());
            let mut sym_batch = LayerBatch::new();
            sym_batch.symbols.push(EphSymbolRow {
                name: format!("nway:{layer_id}"),
                path: label.clone(),
                project_id: root.project_id,
                symbol_type: crate::db_diesel::SYMBOL_TYPE_CONTENT,
                scope: None,
                leaf_name: label,
            });
            let symbol_ids = txn.insert_batch(&sym_batch, root.project_id).await?;
            let mut inst_batch = LayerBatch::new();
            for m in &matches {
                inst_batch.instances.push(EphInstanceRow {
                    symbol_id: symbol_ids[0],
                    object_id: m.object_id,
                    start: m.start_byte as i64,
                    end: m.end_byte as i64,
                    instance_type: crate::db_diesel::INSTANCE_TYPE_DEFINITION,
                });
            }
            txn.insert_batch(&inst_batch, root.project_id).await?;
            Ok(false)
        })
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nway_selection_shard_is_noop_so_no_double_count() -> Result<()> {
        let docker = testcontainers::clients::Cli::default();
        let (_node, url) = crate::symbols_test::start_postgres(&docker);
        crate::symbols_test::wait_for_postgres(&url).await?;
        let index = Index::connect(&url).await?;
        seed_nway_content(&index).await?;

        // Chain over both content layers; the layer shards carry the content,
        // the selection shard is the no-op marker.
        let mut eph = EphContext::rooted(vec![nway_root()]);
        eph.push_materialisation(&[(1000001, -1001)]);
        eph.push_materialisation(&[(1000001, -1002)]);
        let layers = index
            .with_partitioned_layers(
                &eph,
                &[7u8; 32],
                &[],
                nway_noop_root_shard,
                nway_noop_selection_shard,
                Some(nway_content_layer_shard),
            )
            .await?;

        // The selection shard holds ZERO rows (no-op) while each layer shard
        // holds its layer's single match.
        let selection_shard = layers
            .iter()
            .find(|l| l.role == ShardRole::Selection)
            .expect("a selection-shard marker");
        assert_eq!(
            index
                .count_eph_rows_for_layer(selection_shard.outcome.layer_id)
                .await?,
            (0, 0),
            "the selection shard must stay empty — content lives in the layer shards"
        );
        let layer_shards: Vec<_> = layers
            .iter()
            .filter(|l| l.role == ShardRole::Layer)
            .collect();
        assert_eq!(layer_shards.len(), 2, "one layer shard per content layer");
        for layer_shard in &layer_shards {
            let (_syms, insts) = index
                .count_eph_rows_for_layer(layer_shard.outcome.layer_id)
                .await?;
            assert_eq!(insts, 1, "each layer shard holds its layer's single match");
        }

        // The union read across ALL of this statement's layers returns each
        // match exactly once — no selection/layer shard double-count.
        let all_ids: Vec<i64> = layers.iter().map(|l| l.outcome.layer_id).collect();
        let instance_ids = index.get_eph_instance_ids_for_layers(&all_ids).await?;
        assert_eq!(
            instance_ids.len(),
            2,
            "exactly two instances (one per content layer), no duplicates; got {:?}",
            instance_ids
        );
        Ok(())
    }

    #[tokio::test(flavor = "current_thread")]
    async fn nway_loc_shard_scan_resolves_eph_content_per_layer() -> Result<()> {
        let docker = testcontainers::clients::Cli::default();
        let (_node, url) = crate::symbols_test::start_postgres(&docker);
        crate::symbols_test::wait_for_postgres(&url).await?;
        let index = Index::connect(&url).await?;
        seed_nway_content(&index).await?;

        let conn = &mut index.pool.get().await.map_err(|e| anyhow::anyhow!("{e}"))?;
        // loc's path lookup (`find_objects_by_path_on`) shards the same way as
        // search: bound to `[L_j]` with `eph_branch`, each eph content layer
        // resolves only its OWN object, and a layer never resolves a path that
        // lives on a different layer.
        let m1 =
            Index::find_objects_by_path_on(&mut *conn, "/p1/nway1.c", None, &[-1001], true).await?;
        let ids1: Vec<i32> = m1.iter().map(|(fid, _)| (*fid).into()).collect();
        assert_eq!(ids1, vec![-2001], "loc on L=-1001 resolves its object");

        let m2 =
            Index::find_objects_by_path_on(&mut *conn, "/p1/nway2.c", None, &[-1002], true).await?;
        let ids2: Vec<i32> = m2.iter().map(|(fid, _)| (*fid).into()).collect();
        assert_eq!(ids2, vec![-2002], "loc on L=-1002 resolves its object");

        // Cross-shard isolation: nway1.c lives on -1001, not on -1002.
        let none =
            Index::find_objects_by_path_on(&mut *conn, "/p1/nway1.c", None, &[-1002], true).await?;
        assert!(
            none.is_empty(),
            "a path must not resolve on a layer it doesn't live on, got {:?}",
            none
        );
        Ok(())
    }
}
