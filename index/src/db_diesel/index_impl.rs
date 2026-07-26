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
    ChildrenQuery, CompositeFilter, CurrentQuery, HasParentsQuery, ParentsQuery,
    CONTAINER_INSTANCE_ALIAS, CONTAINER_SYMBOL_ALIAS, CONTAINER_TYPE_ALIAS, PARENT_DECLS_ALIAS,
    PARENT_SYMBOLS_ALIAS,
};
use super::selection::{
    is_eph_leak, ChildReference, EphContext, HasChildReference, HasParentReference,
    ParentReference, Selection, SelectionNode,
};
use super::Connection;

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

/// Constrains scope resolution to instances reachable via references.
enum ScopeRole {
    /// Resolved instances must be children of these parent IDs.
    Children(Vec<i64>),
    /// Resolved instances must be parents of these child IDs.
    Parents(Vec<i64>),
}

/// Build a base CurrentQuery (symbols ⋈ instances ⋈ projects ⋈ objects).
/// Applies ephemeral visibility filter: only persistent rows (eph_layer IS NULL)
/// and rows belonging to the given ephemeral layers are returned.
fn build_current_query(eph_ids: &[i64]) -> CurrentQuery<'static> {
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

    // Ephemeral visibility: persistent rows + rows from active layers
    let eph_ids_owned = eph_ids.to_vec();
    query = query.filter(
        symbols::eph_layer
            .is_null()
            .or(symbols::eph_layer.eq_any(eph_ids_owned.clone())),
    );
    query = query.filter(
        symbol_instances::eph_layer
            .is_null()
            .or(symbol_instances::eph_layer.eq_any(eph_ids_owned)),
    );

    query
}

use super::mixins::EphSqlFragment;

/// Resolve a CompositeFilter to instance IDs by running a CurrentQuery.
async fn resolve_filter_to_ids(
    filter: &CompositeFilter,
    role: Option<&ScopeRole>,
    eph_ids: &[i64],
    conn: &mut Connection,
) -> Result<Vec<i64>> {
    use diesel::sql_types::Bool;

    let _span = tracing::info_span!("resolve_filter_to_ids", eph_count = eph_ids.len()).entered();
    let t0 = std::time::Instant::now();

    let eph = EphContext::from_slice(eph_ids);
    let mut query = build_current_query(eph_ids);
    if let Some(expr) = filter.compose_current() {
        query = query.filter(expr);
    }

    // Add reference-based constraint when resolving scoped filters.
    match role {
        Some(ScopeRole::Children(parent_ids)) if !parent_ids.is_empty() => {
            query = query.filter(
                EphSqlFragment::<Bool>::builder()
                    .sql(
                        "symbol_instances.id IN (\
                            SELECT si.id FROM index.symbol_refs sr \
                            JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                            JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                              AND pd.offset_range @> sr.from_offset_range \
                            WHERE ",
                    )
                    .eph_visibility("sr.eph_layer", &eph)
                    .sql(" AND ")
                    .eph_visibility("si.eph_layer", &eph)
                    .sql(" AND ")
                    .eph_visibility("pd.eph_layer", &eph)
                    .sql(" AND pd.id = ANY(")
                    .bind(parent_ids.clone())
                    .sql("))")
                    .build(),
            );
        }
        Some(ScopeRole::Parents(child_ids)) if !child_ids.is_empty() => {
            query = query.filter(
                EphSqlFragment::<Bool>::builder()
                    .sql(
                        "symbol_instances.id IN (\
                            SELECT pd.id FROM index.symbol_refs sr \
                            JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                              AND pd.offset_range @> sr.from_offset_range \
                            JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                            WHERE ",
                    )
                    .eph_visibility("sr.eph_layer", &eph)
                    .sql(" AND ")
                    .eph_visibility("si.eph_layer", &eph)
                    .sql(" AND ")
                    .eph_visibility("pd.eph_layer", &eph)
                    .sql(" AND si.id = ANY(")
                    .bind(child_ids.clone())
                    .sql("))")
                    .build(),
            );
        }
        _ => {}
    }

    let results = query
        .load::<(Symbol, SymbolInstance, Object, Project)>(conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve filter to IDs: {}", e))?;
    tracing::info!(
        elapsed_ms = t0.elapsed().as_millis() as u64,
        result_rows = results.len(),
        "resolve_filter_to_ids SQL completed",
    );
    let mut ids: Vec<i64> = results.iter().map(|(_, inst, _, _)| inst.id).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Resolve a Scope's fields into a set of instance IDs for filtering.
async fn resolve_scope_ids(
    ids: &[i64],
    filter: &Option<CompositeFilter>,
    role: Option<&ScopeRole>,
    eph_ids: &[i64],
    conn: &mut Connection,
) -> Result<Vec<i64>> {
    let mut all_ids = ids.to_vec();
    if let Some(ref f) = filter {
        all_ids.extend(resolve_filter_to_ids(f, role, eph_ids, conn).await?);
        all_ids.sort_unstable();
        all_ids.dedup();
    }
    Ok(all_ids)
}

// ============================================================================
// Shared query builders — used by both find_symbol and find_*_instance_ids
// ============================================================================

fn build_parents_query(source_ids: Vec<i64>, eph_ids: &[i64]) -> ParentsQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

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
        // Ephemeral visibility — filter canonical and aliased tables
        .filter(
            symbol_refs::eph_layer
                .is_null()
                .or(symbol_refs::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            symbols::eph_layer
                .is_null()
                .or(symbols::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            symbol_instances::eph_layer
                .is_null()
                .or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            parent_decls
                .field(symbol_instances::eph_layer)
                .is_null()
                .or(parent_decls
                    .field(symbol_instances::eph_layer)
                    .eq_any(eph_ids_owned.clone())),
        )
        .filter(
            parent_symbols
                .field(symbols::eph_layer)
                .is_null()
                .or(parent_symbols
                    .field(symbols::eph_layer)
                    .eq_any(eph_ids_owned)),
        )
        .select((
            SymbolRef::as_select(),
            Symbol::as_select(),
            SymbolInstance::as_select(),
            parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

fn build_children_query(source_ids: Vec<i64>, eph_ids: &[i64]) -> ChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

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
        // Ephemeral visibility — filter canonical and aliased tables
        .filter(
            symbol_refs::eph_layer
                .is_null()
                .or(symbol_refs::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            symbols::eph_layer
                .is_null()
                .or(symbols::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            symbol_instances::eph_layer
                .is_null()
                .or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            parent_decls
                .field(symbol_instances::eph_layer)
                .is_null()
                .or(parent_decls
                    .field(symbol_instances::eph_layer)
                    .eq_any(eph_ids_owned.clone())),
        )
        .filter(
            parent_symbols
                .field(symbols::eph_layer)
                .is_null()
                .or(parent_symbols
                    .field(symbols::eph_layer)
                    .eq_any(eph_ids_owned)),
        )
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

fn build_has_parents_query(source_ids: Vec<i64>, eph_ids: &[i64]) -> HasParentsQuery<'static> {
    use crate::schema_diesel::*;

    let container_instance = CONTAINER_INSTANCE_ALIAS;
    let container_symbol = CONTAINER_SYMBOL_ALIAS;
    let container_type = CONTAINER_TYPE_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

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
        // Ephemeral visibility — filter both source and aliased (container) tables
        .filter(
            symbols::eph_layer
                .is_null()
                .or(symbols::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            symbol_instances::eph_layer
                .is_null()
                .or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())),
        )
        .filter(
            container_symbol
                .field(symbols::eph_layer)
                .is_null()
                .or(container_symbol
                    .field(symbols::eph_layer)
                    .eq_any(eph_ids_owned.clone())),
        )
        .filter(
            container_instance
                .field(symbol_instances::eph_layer)
                .is_null()
                .or(container_instance
                    .field(symbol_instances::eph_layer)
                    .eq_any(eph_ids_owned)),
        )
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            container_symbol.fields(crate::schema_diesel::symbols::all_columns),
            container_instance.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

#[derive(Clone)]
pub struct Index {
    pub(super) pool: bb8::Pool<AsyncPgConnection>,
    /// Stored for test helpers that need sync DDL connections (migrations, batch_execute).
    database_url: Option<String>,
}

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
        let field = match constraint {
            "symbol_instances_symbol_fkey" => Some(("ephemeral_instance", "symbol_id")),
            "symbol_instances_object_id_fkey" => Some(("ephemeral_instance", "object_id")),
            "symbol_instances_eph_layer_fkey" => Some(("ephemeral_instance", "eph_layer")),
            "symbol_refs_to_symbol_fkey" => Some(("ephemeral_ref", "to_symbol")),
            "symbol_refs_eph_layer_fkey" => Some(("ephemeral_ref", "eph_layer")),
            "symbols_eph_layer_fkey" => Some(("ephemeral_symbol", "eph_layer")),
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
/// `ON DELETE CASCADE` on the `eph_layer` FK cleans up dependent
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
/// transaction's uncommitted `eph_layers` row: the layer would commit
/// AFTER the purge with content computed from pre-mutation data and then
/// be served forever.  `LOCK TABLE … IN EXCLUSIVE MODE` closes the race
/// in both directions: the lock request queues behind every open layer
/// transaction's `ROW EXCLUSIVE` (their upsert), so in-flight populates
/// commit first and their rows become visible to the DELETE; and new
/// layer creations block at `create_eph_layer`'s INSERT until the purging
/// transaction commits, so post-purge populates read only post-mutation
/// data.  Reads (`SELECT`) are unaffected.  No deadlock cycle: layer
/// transactions only ever lock `eph_layers` rows and their own batch
/// tables, never `projects`, so lock ordering is one-directional.
pub async fn purge_eph_cache(conn: &mut AsyncPgConnection) -> Result<usize, diesel::result::Error> {
    use crate::schema_diesel::eph_layers;
    use diesel_async::RunQueryDsl;
    diesel::sql_query("LOCK TABLE index.eph_layers IN EXCLUSIVE MODE")
        .execute(&mut *conn)
        .await?;
    diesel::delete(eph_layers::table.filter(eph_layers::kind.ne(EphLayerKind::Canary.as_str())))
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
/// design (a supplement's rows point into upstream layers).  A plain
/// `DELETE FROM eph_layers WHERE <seed>` therefore cascade-deletes rows out
/// of OTHER still-cached layers while leaving their `eph_layers` rows
/// `populated = true` — hollow entries whose keys never change, served as
/// valid cache hits with silently missing results.  The recursive CTE
/// computes, BEFORE anything cascades, every layer transitively reachable
/// from the seed through four dependency edges — instance→symbol references,
/// ref→symbol references, chain parentage (`parent_id`), and pair coupling
/// (`base_id`) — and deletes the whole closure atomically.  A layer is
/// either fully alive or gone; hollow-but-populated is unconstructible
/// through GC.  (Postgres allows one recursive self-reference, hence the
/// edge-list UNION inside a single recursive term.)  The canary sentinel
/// has no edges and is excluded from the seed.
fn doomed_closure_delete_sql(seed_where: &str) -> String {
    format!(
        "WITH RECURSIVE doomed AS ( \
             SELECT id FROM index.eph_layers \
             WHERE ({seed}) AND kind <> 'canary' \
           UNION \
             SELECT e.dependent FROM ( \
                 SELECT si.eph_layer AS dependent, s.eph_layer AS needed \
                   FROM index.symbol_instances si \
                   JOIN index.symbols s ON si.symbol = s.id \
                  WHERE si.eph_layer IS NOT NULL AND s.eph_layer IS NOT NULL \
                    AND si.eph_layer <> s.eph_layer \
               UNION ALL \
                 SELECT sr.eph_layer, s.eph_layer \
                   FROM index.symbol_refs sr \
                   JOIN index.symbols s ON sr.to_symbol = s.id \
                  WHERE sr.eph_layer IS NOT NULL AND s.eph_layer IS NOT NULL \
                    AND sr.eph_layer <> s.eph_layer \
               UNION ALL \
                 SELECT id, parent_id FROM index.eph_layers \
                  WHERE parent_id IS NOT NULL \
               UNION ALL \
                 SELECT id, base_id FROM index.eph_layers \
                  WHERE base_id IS NOT NULL \
             ) e JOIN doomed d ON e.needed = d.id \
         ) \
         DELETE FROM index.eph_layers \
         WHERE id IN (SELECT id FROM doomed)",
        seed = seed_where,
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

/// Discriminator for an ephemeral layer's origin.  Stored in the
/// `eph_layers.kind` column and bound into the layer's hash chain.
///
/// `Canary` is reserved for the leak-detection sentinel (see
/// `migrations/2026-06-06-000001_eph_layers/up.sql:86-109` and
/// [`Index::validate_canary`]); only `Layer` and `Loc` are created at
/// query time.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EphLayerKind {
    /// The leak-detection sentinel layer.  Never created at runtime;
    /// inserted once by migration and preserved across
    /// `purge_eph_cache` / `purge_old_eph_layers`.
    Canary,
    /// User-created `layer { … }` blocks bundling ephemeral verbs.
    Layer,
    /// `loc(path, line)` cache rows.
    Loc,
    /// Synthesised by `Command::layer_spec` when a single statement
    /// contains multiple layer-creating verbs.  The composite's hash
    /// chains the per-verb specs in source order; the populate batch
    /// runs each verb's contribution in turn.  Single-verb statements
    /// keep their original `Layer`/`Loc` kind for cache continuity.
    Composite,
    /// `search(query, ...)` cache rows.  The layer holds every byte-range
    /// match across the upstream filters' visible objects, capped by the
    /// caller's `limit`; truncation sets `eph_layers.truncated = true` so
    /// the warning surfaces on both cache miss and cache hit.
    Search,
    /// The eph-chained half of a partitioned cache entry (see
    /// [`Index::with_partitioned_layers`]).  Holds only the eph-derived
    /// delta of a base layer; the kind is generic because the supplement's
    /// hash folds the base hash, which already encodes the verb identity.
    /// Diagnostics: `kind='supplement'` + `parent_id` → the upstream layer
    /// it chains on.
    Supplement,
}

impl EphLayerKind {
    /// String form stored in `eph_layers.kind` and bound into the hash.
    pub const fn as_str(&self) -> &'static str {
        match self {
            Self::Canary => "canary",
            Self::Layer => "layer",
            Self::Loc => "loc",
            Self::Composite => "composite",
            Self::Search => "search",
            Self::Supplement => "supplement",
        }
    }
}

impl std::fmt::Display for EphLayerKind {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.as_str())
    }
}

/// Identity of a materialized base layer, handed to a supplement's populate
/// closure by [`Index::with_partitioned_layers`].  Unused today; future
/// mask/tombstone rows will need to reference the base they mask.  Note the
/// two fields age differently: `hash` is stable across TTL purge + recreate
/// of the base row, `layer_id` is not.
#[derive(Debug, Clone, Copy)]
pub struct BaseLayerRef {
    pub layer_id: i64,
    pub hash: [u8; 32],
}

/// Outcome of materializing one `eph_layers` row through the cache gate.
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

/// Result of [`Index::with_partitioned_layers`].
#[derive(Debug, Clone, Copy)]
pub struct PartitionedLayerResult {
    pub base: LayerOutcome,
    /// `None` iff the upstream eph chain was empty (`parent_id = None`).
    /// This is a chain-topology rule, not an overlay-emptiness shortcut:
    /// under a non-empty chain the supplement row is always materialized,
    /// even when its populate inserts zero rows.
    pub supplement: Option<LayerOutcome>,
}

/// Cache key of a supplement layer: parent chain identity + base identity +
/// the verb's own supplement inputs (`extra`).
///
/// `extra` is whatever the supplement populate reads BEYOND the chain and
/// the base — empty for verbs whose delta is fully determined by
/// (parent, base) (search, loc), a hash of the eph-referencing ops for
/// `layer { … }` blocks.  Without it, two blocks sharing base content under
/// the same chain but differing in their eph-referencing ops would collide
/// on the supplement key.  Length-prefixed to keep the encoding injective.
///
/// Folds the parent layer *id*, not its hash — consistent with how verbs
/// chain today (`hash.update(eph.last())`); ids are stable for the lifetime
/// of the row, which is exactly the lifetime of the cache entry.  The domain
/// tag keeps supplement hashes disjoint from every verb-computed hash.
pub fn supplement_hash(parent_id: i64, base_hash: &[u8; 32], extra: &[u8]) -> [u8; 32] {
    use sha2::{Digest, Sha256};
    let mut h = Sha256::new();
    h.update(b"eph-supplement-v1");
    h.update(parent_id.to_le_bytes());
    h.update(base_hash);
    h.update((extra.len() as u64).to_le_bytes());
    h.update(extra);
    h.finalize().into()
}

/// Metadata of one `eph_layers` row, as read by [`Index::eph_layer_meta`].
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
/// open and the `eph_layers` row lock held, blocking every concurrent
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
    pub fn from_pool(pool: bb8::Pool<AsyncPgConnection>) -> Self {
        Self {
            pool,
            database_url: None,
        }
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
        use crate::schema_diesel::eph_layers;
        use diesel::dsl::count_star;
        use diesel_async::RunQueryDsl;
        let mut connection = self.pool.get().await.map_err(|e| {
            anyhow::anyhow!("Failed to get connection for canary validation: {}", e)
        })?;
        let c: i64 = eph_layers::table
            .filter(eph_layers::id.eq(super::selection::CANARY_LAYER_ID))
            .filter(eph_layers::kind.eq(EphLayerKind::Canary.as_str()))
            .select(count_star())
            .get_result(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to validate canary row: {}", e))?;
        if c != 1 {
            anyhow::bail!(
                "canary row missing from index.eph_layers (kind='{}', id={}); \
                 leak detection is not armed. Re-apply the eph_layers migration.",
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

    /// Same as [`Index::get_file_contents`], but on a caller-supplied
    /// connection — for deferred populate closures running inside an
    /// [`EphTransaction`] (see [`Index::search_content_matches_on`]).
    pub async fn get_file_contents_on(
        connection: &mut AsyncPgConnection,
        object_id: FileId,
    ) -> Result<String> {
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
            Some(content) => Ok(String::from_utf8_lossy(&content).to_string()),
            None => Err(anyhow::anyhow!(
                "File contents not found for object_id {}",
                object_id
            )),
        }
    }

    pub async fn find_symbol(
        &self,
        filter: &CompositeFilter,
        parent_scope: ScopeContext,
        children_scope: ScopeContext,
        eph: &EphContext,
    ) -> Result<super::selection::Checked<Selection>> {
        let eph_ids = eph.as_slice();
        use crate::schema_diesel::*;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        let connection: &mut AsyncPgConnection = &mut *connection;

        let current = {
            let _select_current: tracing::span::EnteredSpan =
                tracing::info_span!("select_current", eph_count = eph_ids.len()).entered();
            let t0 = std::time::Instant::now();

            let mut joined_query = build_current_query(eph_ids);

            if let Some(expr) = filter.compose_current() {
                joined_query = joined_query.filter(expr);
            }

            let rows = joined_query
                .load::<(Symbol, SymbolInstance, Object, Project)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = rows.len(),
                "select_current SQL completed",
            );
            rows
        };

        // Use the IDs that survived current's filters (type, name, etc.)
        // so that parents/children queries don't process instances that current excluded.
        let current_instance_ids: Vec<i64> =
            current.iter().map(|(_, inst, _, _)| inst.id).collect();

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
                let mut parents_query = build_parents_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_parents() {
                    parents_query = parents_query.filter(expr);
                }
                let rows = parents_query
                    .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    "select_parents SQL completed",
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
                let scope_ids =
                    resolve_scope_ids(ids, scope_filter, Some(&role), eph_ids, connection).await?;

                let mut parents_query = build_parents_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_parents() {
                    parents_query = parents_query.filter(expr);
                }

                // Always apply scope filter. When scope_ids is empty, eq_any([])
                // correctly returns zero rows (scope specified but matched nothing).
                let parent_decls = PARENT_DECLS_ALIAS;
                parents_query = parents_query.filter(
                    parent_decls
                        .field(symbol_instances::dsl::id)
                        .eq_any(scope_ids),
                );

                let rows = parents_query
                    .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    "select_parents SQL completed",
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
                let mut children_query =
                    build_children_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_children() {
                    children_query = children_query.filter(expr);
                }
                let rows = children_query
                    .load::<(
                        Symbol,
                        Symbol,
                        SymbolInstance,
                        SymbolInstance,
                        SymbolRef,
                        Object,
                    )>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    "select_children SQL completed",
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
                let scope_ids =
                    resolve_scope_ids(ids, scope_filter, Some(&role), eph_ids, connection).await?;

                let mut children_query =
                    build_children_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_children() {
                    children_query = children_query.filter(expr);
                }

                // Always apply scope filter.
                children_query = children_query.filter(symbol_instances::dsl::id.eq_any(scope_ids));

                let rows = children_query
                    .load::<(
                        Symbol,
                        Symbol,
                        SymbolInstance,
                        SymbolInstance,
                        SymbolRef,
                        Object,
                    )>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?;
                tracing::info!(
                    elapsed_ms = t0.elapsed().as_millis() as u64,
                    result_rows = rows.len(),
                    "select_children SQL completed",
                );
                rows
            }
        };

        let has_parents = {
            let _has_parents_span: tracing::span::EnteredSpan = tracing::info_span!(
                "select_has_parents",
                source_count = current_instance_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();

            let mut has_parents_query =
                build_has_parents_query(current_instance_ids.clone(), eph_ids);
            if let Some(expr) = filter.compose_has_parents() {
                has_parents_query = has_parents_query.filter(expr);
            }

            let rows = has_parents_query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load containment parents: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = rows.len(),
                "select_has_parents SQL completed",
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

            let maybe_filter = filter.compose_has_children();
            let rows = if let Some(expr) = maybe_filter {
                build_has_children_query(current_instance_ids, eph_ids)
                    .filter(expr)
                    .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
            } else {
                // Fast path: CTE-materialised source set (planner-hint fix).
                CteHasChildren {
                    cte_body: build_has_children_cte_body(current_instance_ids, eph_ids),
                    outer: build_has_children_query_against_cte(eph_ids),
                }
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
            };
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = rows.len(),
                "select_has_children SQL completed",
            );
            rows
        };

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
        let eph_ids = eph.as_slice();
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut all_ids: Vec<i64> = Vec::new();

        if include_has {
            let _span = tracing::info_span!(
                "find_child_instance_ids_has",
                source_count = parent_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();
            let maybe_filter = filter.compose_has_children();
            let results = if let Some(expr) = maybe_filter {
                build_has_children_query(parent_ids.to_vec(), eph_ids)
                    .filter(expr)
                    .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(
                        &mut *connection,
                    )
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to find has-child instance IDs: {}", e))?
            } else {
                // Fast path: CTE-materialised source set (planner-hint fix).
                CteHasChildren {
                    cte_body: build_has_children_cte_body(parent_ids.to_vec(), eph_ids),
                    outer: build_has_children_query_against_cte(eph_ids),
                }
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-child instance IDs: {}", e))?
            };
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                "find_child_instance_ids (has) SQL completed",
            );
            for (ps, pi, cs, ci, _) in &results {
                if is_eph_leak(ps.eph_layer, eph_ids)
                    || is_eph_leak(pi.eph_layer, eph_ids)
                    || is_eph_leak(cs.eph_layer, eph_ids)
                    || is_eph_leak(ci.eph_layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "eph_layer leak in find_child_instance_ids (has)");
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
            let mut query = build_children_query(parent_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_children() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(
                    Symbol,
                    Symbol,
                    SymbolInstance,
                    SymbolInstance,
                    SymbolRef,
                    Object,
                )>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-child instance IDs: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                "find_child_instance_ids (refs) SQL completed",
            );
            for (ps, cs, ci, fi, sr, _) in &results {
                if is_eph_leak(ps.eph_layer, eph_ids)
                    || is_eph_leak(cs.eph_layer, eph_ids)
                    || is_eph_leak(ci.eph_layer, eph_ids)
                    || is_eph_leak(fi.eph_layer, eph_ids)
                    || is_eph_leak(sr.eph_layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "eph_layer leak in find_child_instance_ids (refs)");
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
        let eph_ids = eph.as_slice();
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut all_ids: Vec<i64> = Vec::new();

        if include_refs {
            let _span = tracing::info_span!(
                "find_parent_instance_ids_refs",
                source_count = child_ids.len(),
                eph_count = eph_ids.len(),
            )
            .entered();
            let t0 = std::time::Instant::now();
            let mut query = build_parents_query(child_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_parents() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-parent instance IDs: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                "find_parent_instance_ids (refs) SQL completed",
            );
            for (sr, s, ci, pi) in &results {
                if is_eph_leak(sr.eph_layer, eph_ids)
                    || is_eph_leak(s.eph_layer, eph_ids)
                    || is_eph_leak(ci.eph_layer, eph_ids)
                    || is_eph_leak(pi.eph_layer, eph_ids)
                {
                    tracing::error!(
                        ?eph_ids,
                        "eph_layer leak in find_parent_instance_ids (refs)"
                    );
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
            let mut query = build_has_parents_query(child_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_has_parents() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-parent instance IDs: {}", e))?;
            tracing::info!(
                elapsed_ms = t0.elapsed().as_millis() as u64,
                result_rows = results.len(),
                "find_parent_instance_ids (has) SQL completed",
            );
            for (cs, ci, ps, pi) in &results {
                if is_eph_leak(cs.eph_layer, eph_ids)
                    || is_eph_leak(ci.eph_layer, eph_ids)
                    || is_eph_leak(ps.eph_layer, eph_ids)
                    || is_eph_leak(pi.eph_layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "eph_layer leak in find_parent_instance_ids (has)");
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
        let eph_ids = eph.as_slice();
        if instance_ids.is_empty() {
            return Ok(vec![]);
        }

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

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
        // its single bind (eph_ids for `sr.eph_layer`) goes through
        // `push_bind_param` so the final query has only typed binds.
        let results = CteFindEdgesBetween {
            cte_body: build_find_edges_cte_body(instance_ids.to_vec(), eph_ids.to_vec()),
            eph_ids: eph_ids.to_vec(),
        }
        .load::<ImplicitEdge>(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to find edges between instances: {}", e))?;

        tracing::info!(
            elapsed_ms = t0.elapsed().as_millis() as u64,
            result_rows = results.len(),
            "find_edges_between SQL completed",
        );

        for edge in &results {
            if is_eph_leak(edge.sr_eph_layer, eph_ids)
                || is_eph_leak(edge.from_eph_layer, eph_ids)
                || is_eph_leak(edge.to_eph_layer, eph_ids)
            {
                tracing::error!(?eph_ids, "eph_layer leak in find_edges_between");
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
    /// Opens a database transaction and performs an atomic upsert on `eph_layers`.
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
    /// `ON CONFLICT DO UPDATE` clause only touches `last_used`.  If a second
    /// request hits the cache (`created = false`) with a different
    /// `parent_id` from the original creator, the row's `parent_id`
    /// remains the original.  Do not read `parent_id` to infer the current
    /// request's ancestry — it reflects the *first* creator's parent
    /// chain, not the caller's.
    pub async fn create_eph_layer(
        &self,
        parent_id: Option<i64>,
        base_id: Option<i64>,
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
        use crate::schema_diesel::eph_layers;
        use diesel::sql_types::Bool;
        let upsert_result = diesel::insert_into(eph_layers::table)
            .values((
                eph_layers::parent_id.eq(parent_id),
                eph_layers::base_id.eq(base_id),
                eph_layers::hash.eq(hash),
                eph_layers::kind.eq(kind.as_str()),
                eph_layers::populated.eq(false),
                eph_layers::truncated.eq(false),
            ))
            .on_conflict(eph_layers::hash)
            .do_update()
            .set(eph_layers::last_used.eq(diesel::dsl::now))
            .returning((
                eph_layers::id,
                diesel::dsl::sql::<Bool>("xmax = 0"),
                eph_layers::populated,
                eph_layers::truncated,
            ))
            .get_result::<(i64, bool, bool, bool)>(&mut *conn)
            .await;
        let (id, created, populated, truncated_on_open) = match upsert_result {
            Ok(row) => row,
            Err(e) => {
                let _ = diesel::sql_query("ROLLBACK").execute(&mut *conn).await;
                // A parent/base FK violation here means the referenced layer
                // was purged between this statement's earlier materialization
                // steps and this insert (e.g. `purge_eph_cache` won the
                // eph_layers lock in the gap between a base commit and its
                // supplement).  The cache is coherent — this request just
                // straddled the purge — so tell the user to retry rather
                // than surfacing a raw constraint error.
                use diesel::result::{DatabaseErrorKind, Error as E};
                if let E::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, info) = &e {
                    let constraint = info.constraint_name().unwrap_or("");
                    if constraint.starts_with("eph_layers_") {
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
                "eph_layers row id={} found with populated=false on cache hit; \
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
        base_id: Option<i64>,
        hash: &[u8],
        kind: EphLayerKind,
        body: F,
    ) -> Result<LayerOutcome>
    where
        F: for<'b> FnOnce(&'b mut EphTransaction<'s>) -> EphScopedFut<'b, bool>,
    {
        let mut txn = self
            .create_eph_layer(parent_id, base_id, hash, kind)
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

    /// Materialize a verb's partitioned cache entry: a parentless *base*
    /// layer keyed only on `base_hash` (persistent data only, unmasked —
    /// masks are composed at read time, never applied here) plus, iff an
    /// upstream chain exists, a *supplement* layer chained on `parent_id`
    /// holding the eph-derived delta.  The supplement is always materialized
    /// under a non-empty chain — even with zero rows — so downstream
    /// chaining keys stay deterministic.
    ///
    /// Runs as two sequential [`Index::with_eph_layer`] calls (each with the
    /// full 2-PC populated protocol).  A failure after the base commits
    /// leaves a valid, reusable base — benign.  Because populate closures
    /// now run the expensive queries themselves, the work happens while the
    /// layer's row lock is held: concurrent identical requests serialize on
    /// the row and the losers get a cache hit instead of duplicating the
    /// computation.
    pub async fn with_partitioned_layers<'s, FB, FS>(
        &'s self,
        parent_id: Option<i64>,
        base_hash: &[u8; 32],
        base_kind: EphLayerKind,
        supplement_extra: &[u8],
        base_populate: FB,
        supplement_populate: FS,
    ) -> Result<PartitionedLayerResult>
    where
        FB: for<'b> FnOnce(&'b mut EphTransaction<'s>) -> EphScopedFut<'b, bool>,
        FS: for<'b> FnOnce(&'b mut EphTransaction<'s>, BaseLayerRef) -> EphScopedFut<'b, bool>,
    {
        let base = self
            .with_eph_layer(None, None, base_hash, base_kind, base_populate)
            .await?;

        let supplement = match parent_id {
            None => None,
            Some(parent) => {
                let s_hash = supplement_hash(parent, base_hash, supplement_extra);
                let base_ref = BaseLayerRef {
                    layer_id: base.layer_id,
                    hash: *base_hash,
                };
                // `base_id` couples the supplement's lifetime to its base
                // (ON DELETE CASCADE): the base is always the older half of
                // the pair, so when it ages out or is deleted, the
                // supplement — whose key folds the stable base *hash*, not
                // the id — can no longer produce a hit against a recreated
                // base of a different incarnation.
                Some(
                    self.with_eph_layer(
                        Some(parent),
                        Some(base.layer_id),
                        &s_hash,
                        EphLayerKind::Supplement,
                        move |txn| supplement_populate(txn, base_ref),
                    )
                    .await?,
                )
            }
        };

        Ok(PartitionedLayerResult { base, supplement })
    }

    /// Get all instance IDs belonging to any of the given ephemeral layers.
    /// Union read across a base+supplement pair — the point where the two
    /// halves of a partitioned entry compose (and where future mask rows
    /// will subtract).
    pub async fn get_eph_instance_ids_for_layers(&self, layer_ids: &[i64]) -> Result<Vec<i64>> {
        use crate::schema_diesel::symbol_instances;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let ids = symbol_instances::table
            .filter(symbol_instances::eph_layer.eq_any(layer_ids))
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
            .filter(symbol_instances::eph_layer.is_not_null())
            .select(symbol_instances::eph_layer.assume_not_null())
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
        use crate::schema_diesel::eph_layers;
        use diesel::dsl::count_star;
        use diesel_async::RunQueryDsl;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        eph_layers::table
            .filter(eph_layers::kind.ne(EphLayerKind::Canary.as_str()))
            .select(count_star())
            .get_result::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to count eph layers: {}", e))
    }

    /// Read the metadata row of a single ephemeral layer.  Used by tests to
    /// assert cache state directly (populated / truncated flags, kind,
    /// parent chaining) instead of inferring it from query results.
    pub async fn eph_layer_meta(&self, layer_id: i64) -> Result<EphLayerMeta> {
        use crate::schema_diesel::eph_layers;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let (id, parent_id, kind, populated, truncated) = eph_layers::table
            .filter(eph_layers::id.eq(layer_id))
            .select((
                eph_layers::id,
                eph_layers::parent_id,
                eph_layers::kind,
                eph_layers::populated,
                eph_layers::truncated,
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
            .filter(symbols::eph_layer.eq(layer_id))
            .select(count_star())
            .get_result::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to count eph symbols for layer: {}", e))?;

        let instance_count = symbol_instances::table
            .filter(symbol_instances::eph_layer.eq(layer_id))
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
        use crate::schema_diesel::eph_layers;
        use diesel::sql_types::Bool;
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::update(
            eph_layers::table
                .filter(eph_layers::id.eq_any(layer_ids))
                .filter(diesel::dsl::sql::<Bool>(
                    "last_used < now() - interval '1 hour'",
                )),
        )
        .set(eph_layers::last_used.eq(diesel::dsl::now))
        .execute(&mut *connection)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to touch eph layers: {}", e))?;

        Ok(())
    }

    /// Check if a symbol exists in the persistent index or in the given ephemeral layers.
    pub async fn symbol_exists(&self, symbol_id: i64, eph: &EphContext) -> Result<bool> {
        use crate::schema_diesel::symbols;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let eph_ids_owned = eph.as_slice().to_vec();
        let exists = symbols::table
            .filter(symbols::id.eq(symbol_id))
            .filter(
                symbols::eph_layer
                    .is_null()
                    .or(symbols::eph_layer.eq_any(eph_ids_owned)),
            )
            .select(symbols::id)
            .first::<i64>(&mut *connection)
            .await
            .optional()
            .map_err(|e| anyhow::anyhow!("Failed to check symbol existence: {}", e))?;

        Ok(exists.is_some())
    }

    /// Find file objects matching the given path suffix, optionally filtered
    /// by project name.  Takes a caller-supplied connection: the only caller
    /// is loc's deferred populate closure, which runs inside an
    /// [`EphTransaction`] and must not check out a second pool connection
    /// while the layer's row lock is held (see
    /// [`Index::search_content_matches_on`]).
    pub async fn find_objects_by_path_on(
        connection: &mut AsyncPgConnection,
        path: &str,
        project_name: Option<&str>,
    ) -> Result<Vec<(FileId, crate::symbols::ProjectId)>> {
        use crate::schema_diesel::*;

        let escaped = path
            .replace('\\', r"\\")
            .replace('%', r"\%")
            .replace('_', r"\_");
        let mut query = objects::table
            .inner_join(projects::table.on(projects::id.eq(objects::project_id)))
            .filter(objects::filesystem_path.like(format!("%{}", escaped)))
            .select((objects::id, projects::id))
            .into_boxed::<Pg>();

        if let Some(name) = project_name {
            query = query.filter(projects::project_name.eq(name));
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
}

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

/// LIKE-escape a raw user query and wrap with the surrounding wildcards.  The
/// three LIKE metacharacters (`\`, `%`, `_`) are escaped with a backslash so
/// they match literally in the pre-filter.  Without this, a query like
/// `"mana_ib_reg_user_"` would have each `_` treated as a single-character
/// wildcard, ballooning the pg_trgm recheck candidate set by an order of
/// magnitude — see the EXPLAIN ANALYZE numbers in the plan.
fn make_like_pattern(query: &str) -> String {
    let escaped = query
        .replace('\\', r"\\")
        .replace('%', r"\%")
        .replace('_', r"\_");
    format!("%{}%", escaped)
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
fn build_search_sql(whole_word: bool, case_sensitive: bool, with_project_filter: bool) -> String {
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

    // Project-level scoping.  When the composite filter contributes an
    // objects_expr (today: ProjectFilterMixin), the caller resolves it to
    // a Vec<i32> of project_ids and we filter `o.project_id` directly.
    // Content shared across projects is naturally scoped: for each cs row
    // the JOIN produces one object per project that references it, and
    // `o.project_id = ANY($N)` keeps only those in the caller's project
    // set.  Slot shifts based on whether $3 holds the LIKE pattern.
    let project_filter = match (with_project_filter, uses_like_pattern) {
        (false, _) => "",
        (true, true) => " AND o.project_id = ANY($4)",
        (true, false) => " AND o.project_id = ANY($3)",
    };

    format!(
        "SELECT \
            o.id AS object_id, \
            o.project_id, \
            p.start_byte, \
            p.end_byte \
         FROM index.content_store cs \
         JOIN index.objects o ON o.content_hash = cs.content_hash \
         CROSS JOIN LATERAL index.find_substr_byte_ranges({haystack}, {needle}, $2) AS p \
         WHERE {pre_filter}{project_filter}{boundary} \
         ORDER BY o.project_id, o.id, p.start_byte \
         LIMIT $2",
        haystack = haystack_expr,
        needle = needle_expr,
        pre_filter = pre_filter,
        project_filter = project_filter,
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
    /// the flag on the eph_layer and surface the warning.
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
    ) -> Result<(Vec<SearchMatchRow>, bool)> {
        use crate::schema_diesel::objects;
        use diesel::sql_types::{Array, Integer, Text};

        let limit_plus_one = (limit as i32).saturating_add(1);

        // Two-step: resolve visible_project_ids via the typed objects_expr
        // hop when the composite filter constrains objects; pass None
        // otherwise so the SQL skips the project JOIN entirely.  No special-
        // casing per filter type — any future FilterLeaf that implements
        // objects_expr will be picked up automatically.
        //
        // We pick out the *distinct project_ids* from the matching objects so
        // the SQL can JOIN content_store_projects on a tiny int[] (typically
        // a single project) rather than the 99k-row visible_obj_ids list a
        // wide project would otherwise emit.
        let visible_project_ids: Option<Vec<i32>> =
            if let Some(expr) = composite_filter.compose_objects() {
                let ids = objects::table
                    .filter(expr)
                    .select(objects::project_id)
                    .distinct()
                    .load::<i32>(&mut *connection)
                    .await
                    .map_err(|e| {
                        anyhow::anyhow!("Failed to resolve visible project ids for search: {}", e)
                    })?;
                Some(ids)
            } else {
                None
            };

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

        let sql = build_search_sql(whole_word, case_sensitive, visible_project_ids.is_some());

        let rows: Vec<SearchMatchRow> = match (like_pattern.as_ref(), visible_project_ids.as_ref())
        {
            (Some(pat), Some(ids)) => {
                diesel::sql_query(&sql)
                    .bind::<Text, _>(query)
                    .bind::<Integer, _>(limit_plus_one)
                    .bind::<Text, _>(pat)
                    .bind::<Array<Integer>, _>(ids)
                    .load::<SearchMatchRow>(&mut *connection)
                    .await
            }
            (Some(pat), None) => {
                diesel::sql_query(&sql)
                    .bind::<Text, _>(query)
                    .bind::<Integer, _>(limit_plus_one)
                    .bind::<Text, _>(pat)
                    .load::<SearchMatchRow>(&mut *connection)
                    .await
            }
            (None, Some(ids)) => {
                diesel::sql_query(&sql)
                    .bind::<Text, _>(query)
                    .bind::<Integer, _>(limit_plus_one)
                    .bind::<Array<Integer>, _>(ids)
                    .load::<SearchMatchRow>(&mut *connection)
                    .await
            }
            (None, None) => {
                diesel::sql_query(&sql)
                    .bind::<Text, _>(query)
                    .bind::<Integer, _>(limit_plus_one)
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
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    pub sr_eph_layer: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    pub from_eph_layer: Option<i64>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::BigInt>)]
    pub to_eph_layer: Option<i64>,
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
    /// Value of `eph_layers.truncated` as observed when the layer was opened.
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

    /// Insert a batch of rows into this layer within the open transaction.
    /// Returns the IDs of inserted symbols (in insertion order), empty if no symbols.
    pub async fn insert_batch(&mut self, batch: &LayerBatch) -> Result<Vec<i64>> {
        let symbol_ids = self.insert_symbols(&batch.symbols).await?;
        self.insert_instances(&batch.instances).await?;
        self.insert_refs(&batch.refs).await?;
        Ok(symbol_ids)
    }

    async fn insert_symbols(&mut self, rows: &[EphSymbolRow]) -> Result<Vec<i64>> {
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
            "INSERT INTO index.symbols (id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name, eph_layer) \
             SELECT nextval('index.eph_symbol_id_seq'), \
                    t.name, t.path::ltree, t.project_id, t.symbol_type, t.scope, t.leaf_name, $7 \
             FROM UNNEST($1::text[], $2::text[], $3::int4[], $4::int4[], $5::int4[], $6::text[]) \
             AS t(name, path, project_id, symbol_type, scope, leaf_name) \
             RETURNING id"
        )
            .bind::<Array<Text>, _>(&names)
            .bind::<Array<Text>, _>(&paths)
            .bind::<Array<Integer>, _>(&project_ids)
            .bind::<Array<Integer>, _>(&symbol_types)
            .bind::<Array<Nullable<Integer>>, _>(&scopes)
            .bind::<Array<Text>, _>(&leaf_names)
            .bind::<BigInt, _>(layer_id)
            .get_results(conn)
            .await
            .map_err(|e| explain_eph_insert_err("Failed to batch insert eph symbols", e))?;

        Ok(inserted.into_iter().map(|r| r.id).collect())
    }

    async fn insert_instances(&mut self, rows: &[EphInstanceRow]) -> Result<()> {
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
            "INSERT INTO index.symbol_instances (id, symbol, object_id, offset_range, instance_type, eph_layer) \
             SELECT nextval('index.eph_instance_id_seq'), \
                    t.symbol_id, t.object_id, int4range(t.start_off, t.end_off), t.instance_type, $6 \
             FROM UNNEST($1::int8[], $2::int4[], $3::int4[], $4::int4[], $5::int4[]) \
             AS t(symbol_id, object_id, start_off, end_off, instance_type) \
             ON CONFLICT DO NOTHING"
        )
            .bind::<Array<BigInt>,  _>(&sym_ids)
            .bind::<Array<Integer>, _>(&object_ids)
            .bind::<Array<Integer>, _>(&starts)
            .bind::<Array<Integer>, _>(&ends)
            .bind::<Array<Integer>, _>(&instance_types)
            .bind::<BigInt, _>(layer_id)
            .execute(conn)
            .await
            .map_err(|e| explain_eph_insert_err("Failed to batch insert eph instances", e))?;
        Ok(())
    }

    async fn insert_refs(&mut self, rows: &[EphRefRow]) -> Result<()> {
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
            "INSERT INTO index.symbol_refs (id, to_symbol, from_object, from_offset_range, eph_layer) \
             SELECT nextval('index.eph_ref_id_seq'), \
                    t.to_symbol, t.from_object, int4range(t.start_off, t.end_off), $5 \
             FROM UNNEST($1::int8[], $2::int4[], $3::int4[], $4::int4[]) \
             AS t(to_symbol, from_object, start_off, end_off) \
             ON CONFLICT DO NOTHING"
        )
            .bind::<Array<BigInt>,  _>(&to_symbols)
            .bind::<Array<Integer>, _>(&from_objects)
            .bind::<Array<Integer>, _>(&starts)
            .bind::<Array<Integer>, _>(&ends)
            .bind::<BigInt, _>(layer_id)
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
        use crate::schema_diesel::eph_layers;
        diesel::update(eph_layers::table.filter(eph_layers::id.eq(self.layer_id)))
            .set(eph_layers::populated.eq(true))
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
        use crate::schema_diesel::eph_layers;
        diesel::update(eph_layers::table.filter(eph_layers::id.eq(self.layer_id)))
            .set(eph_layers::truncated.eq(truncated))
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
