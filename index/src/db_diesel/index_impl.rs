use anyhow::Result;
use diesel::connection::SimpleConnection;
use diesel::pg::{Pg, PgConnection};
use diesel::prelude::*;
use diesel::PgRangeExpressionMethods;
use diesel_async::pooled_connection::bb8;
use diesel_async::pooled_connection::{AsyncDieselConnectionManager, ManagerConfig, RecyclingMethod};
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

use super::mixins::{
    CompositeFilter, CurrentQuery,
    PARENT_DECLS_ALIAS, PARENT_SYMBOLS_ALIAS,
    CONTAINER_INSTANCE_ALIAS, CONTAINER_SYMBOL_ALIAS, CONTAINER_TYPE_ALIAS,
    CONTAINED_INSTANCE_ALIAS, CONTAINED_SYMBOL_ALIAS, CONTAINED_TYPE_ALIAS,
    ParentsQuery, ChildrenQuery, HasParentsQuery, HasChildrenQuery,
};
use super::selection::{ChildReference, HasChildReference, HasParentReference, ParentReference, Selection, SelectionNode, is_eph_leak};
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
        .inner_join(
            projects::dsl::projects.on(symbols::dsl::project_id.eq(projects::dsl::id)),
        )
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
        symbols::eph_layer.is_null()
            .or(symbols::eph_layer.eq_any(eph_ids_owned.clone()))
    );
    query = query.filter(
        symbol_instances::eph_layer.is_null()
            .or(symbol_instances::eph_layer.eq_any(eph_ids_owned))
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

    let mut query = build_current_query(eph_ids);
    if let Some(expr) = filter.compose_current() {
        query = query.filter(expr);
    }

    // Add reference-based constraint when resolving scoped filters.
    match role {
        Some(ScopeRole::Children(parent_ids)) if !parent_ids.is_empty() => {
            query = query.filter(
                EphSqlFragment::<Bool>::builder()
                    .sql("symbol_instances.id IN (\
                            SELECT si.id FROM index.symbol_refs sr \
                            JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                            JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                              AND pd.offset_range @> sr.from_offset_range \
                            WHERE ")
                    .eph_visibility("sr.eph_layer", eph_ids.to_vec())
                    .sql(" AND ")
                    .eph_visibility("si.eph_layer", eph_ids.to_vec())
                    .sql(" AND ")
                    .eph_visibility("pd.eph_layer", eph_ids.to_vec())
                    .sql(" AND pd.id = ANY(")
                    .bind(parent_ids.clone())
                    .sql("))")
                    .build()
            );
        }
        Some(ScopeRole::Parents(child_ids)) if !child_ids.is_empty() => {
            query = query.filter(
                EphSqlFragment::<Bool>::builder()
                    .sql("symbol_instances.id IN (\
                            SELECT pd.id FROM index.symbol_refs sr \
                            JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                              AND pd.offset_range @> sr.from_offset_range \
                            JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                            WHERE ")
                    .eph_visibility("sr.eph_layer", eph_ids.to_vec())
                    .sql(" AND ")
                    .eph_visibility("si.eph_layer", eph_ids.to_vec())
                    .sql(" AND ")
                    .eph_visibility("pd.eph_layer", eph_ids.to_vec())
                    .sql(" AND si.id = ANY(")
                    .bind(child_ids.clone())
                    .sql("))")
                    .build()
            );
        }
        _ => {}
    }

    let results = query
        .load::<(Symbol, SymbolInstance, Object, Project)>(conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve filter to IDs: {}", e))?;
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

fn build_parents_query(
    source_ids: Vec<i64>,
    eph_ids: &[i64],
) -> ParentsQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

    symbol_refs::dsl::symbol_refs
        .inner_join(
            symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::dsl::id)),
        )
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
        .filter(
            symbol_instances::dsl::id.eq_any(source_ids),
        )
        // Ephemeral visibility — filter canonical and aliased tables
        .filter(symbol_refs::eph_layer.is_null().or(symbol_refs::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(symbols::eph_layer.is_null().or(symbols::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(symbol_instances::eph_layer.is_null().or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(parent_decls.field(symbol_instances::eph_layer).is_null()
            .or(parent_decls.field(symbol_instances::eph_layer).eq_any(eph_ids_owned.clone())))
        .filter(parent_symbols.field(symbols::eph_layer).is_null()
            .or(parent_symbols.field(symbols::eph_layer).eq_any(eph_ids_owned)))
        .select((
            SymbolRef::as_select(),
            Symbol::as_select(),
            SymbolInstance::as_select(),
            parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

fn build_children_query(
    source_ids: Vec<i64>,
    eph_ids: &[i64],
) -> ChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

    symbol_refs::dsl::symbol_refs
        .inner_join(symbols::dsl::symbols.on(symbol_refs::dsl::to_symbol.eq(symbols::id)))
        .inner_join(
            symbol_instances::dsl::symbol_instances.on(symbols::dsl::id.eq(symbol_instances::symbol)),
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
        .filter(symbol_refs::eph_layer.is_null().or(symbol_refs::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(symbols::eph_layer.is_null().or(symbols::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(symbol_instances::eph_layer.is_null().or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(parent_decls.field(symbol_instances::eph_layer).is_null()
            .or(parent_decls.field(symbol_instances::eph_layer).eq_any(eph_ids_owned.clone())))
        .filter(parent_symbols.field(symbols::eph_layer).is_null()
            .or(parent_symbols.field(symbols::eph_layer).eq_any(eph_ids_owned)))
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

fn build_has_parents_query(
    source_ids: Vec<i64>,
    eph_ids: &[i64],
) -> HasParentsQuery<'static> {
    use crate::schema_diesel::*;

    let container_instance = CONTAINER_INSTANCE_ALIAS;
    let container_symbol = CONTAINER_SYMBOL_ALIAS;
    let container_type = CONTAINER_TYPE_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

    symbol_instances::dsl::symbol_instances
        .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
        .inner_join(symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)))
        .filter(symbol_instances::dsl::id.eq_any(source_ids))
        .inner_join(
            container_instance.on(
                container_instance.field(symbol_instances::dsl::object_id)
                    .eq(symbol_instances::dsl::object_id)
            ),
        )
        .inner_join(
            container_symbol.on(
                container_symbol.field(symbols::dsl::id)
                    .eq(container_instance.field(symbol_instances::dsl::symbol))
            ),
        )
        .inner_join(
            container_type.on(
                container_type.field(symbol_types::dsl::id)
                    .eq(container_symbol.field(symbols::dsl::symbol_type))
            ),
        )
        .filter(
            diesel::dsl::sql::<diesel::sql_types::Bool>(
                "container_instances.offset_range @> symbol_instances.offset_range"
            )
        )
        .filter(
            container_type.field(symbol_types::dsl::level)
                .ge(symbol_types::dsl::level)
        )
        .filter(
            container_instance.field(symbol_instances::dsl::id)
                .ne(symbol_instances::dsl::id)
        )
        // Ephemeral visibility — filter both source and aliased (container) tables
        .filter(symbols::eph_layer.is_null().or(symbols::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(symbol_instances::eph_layer.is_null().or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(container_symbol.field(symbols::eph_layer).is_null()
            .or(container_symbol.field(symbols::eph_layer).eq_any(eph_ids_owned.clone())))
        .filter(container_instance.field(symbol_instances::eph_layer).is_null()
            .or(container_instance.field(symbol_instances::eph_layer).eq_any(eph_ids_owned)))
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            container_symbol.fields(crate::schema_diesel::symbols::all_columns),
            container_instance.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

fn build_has_children_query(
    source_ids: Vec<i64>,
    eph_ids: &[i64],
) -> HasChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let contained_instance = CONTAINED_INSTANCE_ALIAS;
    let contained_symbol = CONTAINED_SYMBOL_ALIAS;
    let contained_type = CONTAINED_TYPE_ALIAS;

    let eph_ids_owned = eph_ids.to_vec();

    symbol_instances::dsl::symbol_instances
        .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
        .inner_join(symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)))
        .filter(symbol_instances::dsl::id.eq_any(source_ids))
        .inner_join(objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)))
        .inner_join(
            contained_instance.on(
                contained_instance.field(symbol_instances::dsl::object_id)
                    .eq(symbol_instances::dsl::object_id)
            ),
        )
        .inner_join(
            contained_symbol.on(
                contained_symbol.field(symbols::dsl::id)
                    .eq(contained_instance.field(symbol_instances::dsl::symbol))
            ),
        )
        .inner_join(
            contained_type.on(
                contained_type.field(symbol_types::dsl::id)
                    .eq(contained_symbol.field(symbols::dsl::symbol_type))
            ),
        )
        .filter(
            diesel::dsl::sql::<diesel::sql_types::Bool>(
                "symbol_instances.offset_range @> contained_instances.offset_range"
            )
        )
        .filter(
            symbol_types::dsl::level
                .ge(contained_type.field(symbol_types::dsl::level))
        )
        .filter(
            symbol_instances::dsl::id
                .ne(contained_instance.field(symbol_instances::dsl::id))
        )
        // Ephemeral visibility — filter both source and aliased (contained) tables
        .filter(symbols::eph_layer.is_null().or(symbols::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(symbol_instances::eph_layer.is_null().or(symbol_instances::eph_layer.eq_any(eph_ids_owned.clone())))
        .filter(contained_symbol.field(symbols::eph_layer).is_null()
            .or(contained_symbol.field(symbols::eph_layer).eq_any(eph_ids_owned.clone())))
        .filter(contained_instance.field(symbol_instances::eph_layer).is_null()
            .or(contained_instance.field(symbol_instances::eph_layer).eq_any(eph_ids_owned)))
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            contained_symbol.fields(crate::schema_diesel::symbols::all_columns),
            contained_instance.fields(crate::schema_diesel::symbol_instances::all_columns),
            Object::as_select(),
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
    }).collect()
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
fn explain_eph_insert_err(default_prefix: &'static str, err: diesel::result::Error) -> anyhow::Error {
    use diesel::result::{DatabaseErrorKind, Error as E};
    if let E::DatabaseError(DatabaseErrorKind::ForeignKeyViolation, info) = &err {
        let constraint = info.constraint_name().unwrap_or("");
        let field = match constraint {
            "symbol_instances_symbol_fkey"      => Some(("ephemeral_instance", "symbol_id")),
            "symbol_instances_object_id_fkey"   => Some(("ephemeral_instance", "object_id")),
            "symbol_instances_eph_layer_fkey"   => Some(("ephemeral_instance", "eph_layer")),
            "symbol_refs_to_symbol_fkey"        => Some(("ephemeral_ref",      "to_symbol")),
            "symbol_refs_eph_layer_fkey"        => Some(("ephemeral_ref",      "eph_layer")),
            "symbols_eph_layer_fkey"            => Some(("ephemeral_symbol",   "eph_layer")),
            _ => None,
        };
        if let Some((verb, fname)) = field {
            return anyhow::anyhow!(
                "{}: '{}' refers to a row that does not exist \
                 (or is in a different layer than this insert can see). \
                 Postgres constraint: {}",
                verb, fname, constraint
            );
        }
    }
    anyhow::anyhow!("{}: {}", default_prefix, err)
}

/// Helper row for the canary startup check in [`Index::connect`].
#[derive(diesel::QueryableByName)]
struct CountRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    c: i64,
}

/// SQL predicate matching every row in `index.eph_layers` **except** the
/// canary.  Use this in any DELETE / UPDATE on the table that must leave
/// the canary intact — running without the canary disarms the leak
/// detection wired up in [`Index::validate_canary`] and
/// [`super::selection::Checked`].
const NOT_CANARY_PREDICATE: &str = "kind != 'canary'";

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
pub async fn purge_eph_cache(
    conn: &mut AsyncPgConnection,
) -> Result<usize, diesel::result::Error> {
    use diesel_async::RunQueryDsl;
    let sql = format!("DELETE FROM index.eph_layers WHERE {}", NOT_CANARY_PREDICATE);
    RunQueryDsl::execute(diesel::sql_query(sql), conn).await
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

impl Index {
    pub fn from_pool(pool: bb8::Pool<AsyncPgConnection>) -> Self {
        Self { pool, database_url: None }
    }

    fn build_async_manager(database_url: &str) -> AsyncDieselConnectionManager<AsyncPgConnection> {
        let mut config = ManagerConfig::default();
        config.recycling_method = RecyclingMethod::CustomQuery("ROLLBACK".into());
        AsyncDieselConnectionManager::new_with_config(database_url, config)
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
        let index = Self { pool, database_url: Some(database_url.to_string()) };
        index.validate_canary().await?;
        Ok(index)
    }

    /// Confirm the canary row exists.  The canary is load-bearing for the
    /// leak-detection defence; running without it silently disables
    /// `Selection::has_eph_leak` / `Checked::new` as detectors (they keep
    /// running, just have nothing to catch).  Call this after migrations
    /// have run and the pool is ready.
    pub async fn validate_canary(&self) -> Result<()> {
        use diesel_async::RunQueryDsl;
        let mut connection = self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection for canary validation: {}", e))?;
        let row: CountRow = diesel::sql_query(
            "SELECT COUNT(*) AS c FROM index.eph_layers \
             WHERE id = -999999 AND kind = 'canary'"
        )
            .get_result(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to validate canary row: {}", e))?;
        if row.c != 1 {
            anyhow::bail!(
                "canary row missing from index.eph_layers (kind='canary', id=-999999); \
                 leak detection is not armed. Re-apply the eph_layers migration."
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
        Ok(Self { pool, database_url: Some(database_url.to_string()) })
    }

    pub async fn new_in_memory() -> Result<Self> {
        Err(anyhow::anyhow!(
            "In-memory Postgres is not supported; use Index::connect with a test database"
        ))
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

    fn load_sql(connection: &mut PgConnection, input_path: &str) {
        match input_path {
            "test_input_a.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_a.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_b.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_b.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_modules.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_modules.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_symbol_tokens.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_symbol_tokens.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "verb_test.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/verb_test.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_containment.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_containment.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_tree_browser.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_tree_browser.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_nested_func.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_nested_func.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            "test_input_type_filter.sql" => {
                connection
                    .batch_execute(include_str!("../../../sql/test_input_type_filter.sql"))
                    .map_err(|e| anyhow::anyhow!("Failed to execute SQL file: {}", e))
                    .unwrap();
            }
            _ => panic!("Impossible input file"),
        };
    }

    pub async fn load_test_input(&mut self, input_path: &str) -> Result<()> {
        let database_url = self.database_url.as_ref()
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

        let object_id: i32 = object_id.into();
        let result: Option<Vec<u8>> = diesel::sql_query(
            r#"
            SELECT COALESCE(oc.content, cs.content) AS content
            FROM index.objects o
            LEFT JOIN index.object_contents oc ON oc.object_id = o.id
            LEFT JOIN index.content_store cs ON cs.content_hash = o.content_hash
            WHERE o.id = $1
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
        eph_ids: &[i64],
    ) -> Result<super::selection::Checked<Selection>> {
        use crate::schema_diesel::*;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        let connection: &mut AsyncPgConnection = &mut *connection;

        let current = {
            let _select_current: tracing::span::EnteredSpan =
                tracing::debug_span!("select_current").entered();

            let mut joined_query = build_current_query(eph_ids);

            if let Some(expr) = filter.compose_current() {
                joined_query = joined_query.filter(expr);
            }

            joined_query
                .load::<(Symbol, SymbolInstance, Object, Project)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load symbols: {}", e))?
        };

        // Use the IDs that survived current's filters (type, name, etc.)
        // so that parents/children queries don't process instances that current excluded.
        let current_instance_ids: Vec<i64> =
            current.iter().map(|(_, inst, _, _)| inst.id).collect();

        let parents = match parent_scope {
            ScopeContext::Skip => vec![],
            ScopeContext::Unscoped => {
                let _parents_span: tracing::span::EnteredSpan =
                    tracing::debug_span!("select_parents").entered();
                let mut parents_query = build_parents_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_parents() {
                    parents_query = parents_query.filter(expr);
                }
                parents_query
                    .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
            }
            ScopeContext::Scope { ref ids, filter: ref scope_filter } => {
                let _parents_span: tracing::span::EnteredSpan =
                    tracing::debug_span!("select_parents").entered();

                let role = ScopeRole::Parents(current_instance_ids.clone());
                let scope_ids = resolve_scope_ids(ids, scope_filter, Some(&role), eph_ids, connection).await?;

                let mut parents_query = build_parents_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_parents() {
                    parents_query = parents_query.filter(expr);
                }

                // Always apply scope filter. When scope_ids is empty, eq_any([])
                // correctly returns zero rows (scope specified but matched nothing).
                let parent_decls = PARENT_DECLS_ALIAS;
                parents_query = parents_query.filter(
                    parent_decls.field(symbol_instances::dsl::id).eq_any(scope_ids)
                );

                parents_query
                    .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
            }
        };

        let children = match children_scope {
            ScopeContext::Skip => vec![],
            ScopeContext::Unscoped => {
                let _select_children: tracing::span::EnteredSpan =
                    tracing::debug_span!("select_children").entered();
                let mut children_query = build_children_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_children() {
                    children_query = children_query.filter(expr);
                }
                children_query
                    .load::<(Symbol, Symbol, SymbolInstance, SymbolInstance, SymbolRef, Object)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
            }
            ScopeContext::Scope { ref ids, filter: ref scope_filter } => {
                let _select_children: tracing::span::EnteredSpan =
                    tracing::debug_span!("select_children").entered();

                let role = ScopeRole::Children(current_instance_ids.clone());
                let scope_ids = resolve_scope_ids(ids, scope_filter, Some(&role), eph_ids, connection).await?;

                let mut children_query = build_children_query(current_instance_ids.clone(), eph_ids);
                if let Some(expr) = filter.compose_children() {
                    children_query = children_query.filter(expr);
                }

                // Always apply scope filter.
                children_query = children_query.filter(
                    symbol_instances::dsl::id.eq_any(scope_ids)
                );

                children_query
                    .load::<(Symbol, Symbol, SymbolInstance, SymbolInstance, SymbolRef, Object)>(connection)
                    .await
                    .map_err(|e| anyhow::anyhow!("Failed to load symbol references: {}", e))?
            }
        };

        let has_parents = {
            let _has_parents_span: tracing::span::EnteredSpan =
                tracing::debug_span!("select_has_parents").entered();

            let mut has_parents_query = build_has_parents_query(current_instance_ids.clone(), eph_ids);
            if let Some(expr) = filter.compose_has_parents() {
                has_parents_query = has_parents_query.filter(expr);
            }

            has_parents_query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load containment parents: {}", e))?
        };

        let has_children = {
            let _has_children_span: tracing::span::EnteredSpan =
                tracing::debug_span!("select_has_children").entered();

            let mut has_children_query = build_has_children_query(current_instance_ids, eph_ids);
            if let Some(expr) = filter.compose_has_children() {
                has_children_query = has_children_query.filter(expr);
            }

            has_children_query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
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
                .map(|(child_symbol, child_instance, parent_symbol, parent_instance)| {
                    HasParentReference {
                        child_symbol,
                        child_instance,
                        parent_symbol,
                        parent_instance,
                    }
                })
                .collect();

            let mut has_children: Vec<_> = has_children
                .into_iter()
                .map(|(parent_symbol, parent_instance, child_symbol, child_instance, parent_object)| {
                    HasChildReference {
                        parent_symbol,
                        parent_instance,
                        child_symbol,
                        child_instance,
                        parent_object,
                    }
                })
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
        super::selection::Checked::new(selection?, eph_ids)
    }

    /// Query child instance IDs directly from DB given parent instance IDs.
    pub async fn find_child_instance_ids(
        &self,
        parent_ids: &[i64],
        include_refs: bool,
        include_has: bool,
        filter: &CompositeFilter,
        eph_ids: &[i64],
    ) -> Result<Vec<crate::symbols::SymbolInstanceId>> {
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut all_ids: Vec<i64> = Vec::new();

        if include_has {
            let mut query = build_has_children_query(parent_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_has_children() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-child instance IDs: {}", e))?;
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
            let mut query = build_children_query(parent_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_children() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, Symbol, SymbolInstance, SymbolInstance, SymbolRef, Object)>(
                    &mut *connection,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-child instance IDs: {}", e))?;
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
            all_ids.extend(results.iter().map(|(_, _, callee_inst, _, _, _)| callee_inst.id));
        }

        all_ids.sort_unstable();
        all_ids.dedup();
        Ok(all_ids.into_iter().map(crate::symbols::SymbolInstanceId::new).collect())
    }

    /// Query parent instance IDs directly from DB given child instance IDs.
    pub async fn find_parent_instance_ids(
        &self,
        child_ids: &[i64],
        include_refs: bool,
        include_has: bool,
        filter: &CompositeFilter,
        eph_ids: &[i64],
    ) -> Result<Vec<crate::symbols::SymbolInstanceId>> {
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut all_ids: Vec<i64> = Vec::new();

        if include_refs {
            let mut query = build_parents_query(child_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_parents() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-parent instance IDs: {}", e))?;
            for (sr, s, ci, pi) in &results {
                if is_eph_leak(sr.eph_layer, eph_ids)
                    || is_eph_leak(s.eph_layer, eph_ids)
                    || is_eph_leak(ci.eph_layer, eph_ids)
                    || is_eph_leak(pi.eph_layer, eph_ids)
                {
                    tracing::error!(?eph_ids, "eph_layer leak in find_parent_instance_ids (refs)");
                    anyhow::bail!("internal error: ephemeral layer isolation violation");
                }
            }
            all_ids.extend(results.iter().map(|(_, _, _, parent_inst)| parent_inst.id));
        }

        if include_has {
            let mut query = build_has_parents_query(child_ids.to_vec(), eph_ids);
            if let Some(expr) = filter.compose_has_parents() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-parent instance IDs: {}", e))?;
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
            all_ids.extend(results.iter().map(|(_, _, _, container_inst)| container_inst.id));
        }

        all_ids.sort_unstable();
        all_ids.dedup();
        Ok(all_ids.into_iter().map(crate::symbols::SymbolInstanceId::new).collect())
    }

    /// Discover all reference edges between a set of selected instances.
    pub async fn find_edges_between(
        &self,
        instance_ids: &[i64],
        eph_ids: &[i64],
    ) -> Result<Vec<ImplicitEdge>> {
        if instance_ids.is_empty() {
            return Ok(vec![]);
        }

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let _span = tracing::debug_span!("find_edges_between", count = instance_ids.len()).entered();

        let results = diesel::sql_query(
            "SELECT DISTINCT ON (from_inst.id, sr.id) \
                    sr.id AS ref_id, sr.to_symbol, sr.from_object, sr.from_offset_range, \
                    to_inst.id AS to_instance_id, \
                    from_inst.id AS from_instance_id, \
                    sr.eph_layer AS sr_eph_layer, \
                    from_inst.eph_layer AS from_eph_layer, \
                    to_inst.eph_layer AS to_eph_layer \
             FROM index.symbol_instances from_inst \
             JOIN index.symbol_refs sr \
                 ON sr.from_object = from_inst.object_id \
                 AND from_inst.offset_range @> sr.from_offset_range \
             JOIN index.symbol_instances to_inst \
                 ON to_inst.symbol = sr.to_symbol \
             WHERE from_inst.id = ANY($1) \
               AND to_inst.id = ANY($1) \
               AND from_inst.id != to_inst.id \
               AND (sr.eph_layer IS NULL OR sr.eph_layer = ANY($2)) \
               AND (from_inst.eph_layer IS NULL OR from_inst.eph_layer = ANY($2)) \
               AND (to_inst.eph_layer IS NULL OR to_inst.eph_layer = ANY($2)) \
             ORDER BY from_inst.id, sr.id, to_inst.id"
        )
            .bind::<diesel::sql_types::Array<diesel::sql_types::BigInt>, _>(instance_ids)
            .bind::<diesel::sql_types::Array<diesel::sql_types::BigInt>, _>(eph_ids)
            .load::<ImplicitEdge>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find edges between instances: {}", e))?;

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
    /// commit/rollback through a scoped closure.
    ///
    /// The `created` flag uses PostgreSQL's `xmax = 0` trick: a freshly inserted row
    /// has xmax=0, while an ON CONFLICT UPDATE sets xmax to the updating transaction.
    ///
    /// ## Implicit serialization between concurrent creators
    ///
    /// Two requests with the same `hash` that race for the layer will both
    /// reach the `INSERT … ON CONFLICT` statement; the loser blocks on a
    /// row-level lock until the winner's transaction commits or rolls back.
    /// That ordering is the only thing keeping the loser from seeing an
    /// empty layer: the winner is expected to populate-and-commit (or
    /// rollback) before returning.  Callers must hold this contract — *do
    /// not* commit the layer row separately from the populate batch.
    /// A future migration tracked at
    /// `feedback_design_preferences` → "project-eph-layer-2phase" will make
    /// this explicit via a `populated` flag and two-phase commit.
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
        hash: &[u8],
        kind: &str,
    ) -> Result<EphTransaction<'_>> {
        let mut conn = self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::sql_query("BEGIN")
            .execute(&mut *conn)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to BEGIN transaction: {}", e))?;

        let row = match diesel::sql_query(
            "INSERT INTO index.eph_layers (parent_id, hash, kind) \
             VALUES ($1, $2, $3) \
             ON CONFLICT (hash) DO UPDATE SET last_used = now() \
             RETURNING id, (xmax = 0) AS created"
        )
            .bind::<diesel::sql_types::Nullable<diesel::sql_types::BigInt>, _>(parent_id)
            .bind::<diesel::sql_types::Bytea, _>(hash)
            .bind::<diesel::sql_types::Text, _>(kind)
            .get_result::<CreateLayerRow>(&mut *conn)
            .await
        {
            Ok(row) => row,
            Err(e) => {
                let _ = diesel::sql_query("ROLLBACK").execute(&mut *conn).await;
                return Err(anyhow::anyhow!("Failed to upsert eph layer: {}", e));
            }
        };

        Ok(EphTransaction {
            conn,
            layer_id: row.id,
            created: row.created,
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
    pub async fn with_eph_layer<'s, R, F>(
        &'s self,
        parent_id: Option<i64>,
        hash: &[u8],
        kind: &str,
        body: F,
    ) -> Result<(i64, bool, R)>
    where
        F: for<'b> FnOnce(&'b mut EphTransaction<'s>) -> EphScopedFut<'b, R>,
    {
        let mut txn = self.create_eph_layer(parent_id, hash, kind).await?;
        let layer_id = txn.layer_id();
        let created = txn.created();
        match body(&mut txn).await {
            Ok(r) => {
                txn.commit().await?;
                Ok((layer_id, created, r))
            }
            Err(e) => {
                let _ = txn.rollback().await;
                Err(e)
            }
        }
    }

    /// Get all instance IDs belonging to a given ephemeral layer.
    pub async fn get_eph_instance_ids_for_layer(&self, layer_id: i64) -> Result<Vec<i64>> {
        use crate::schema_diesel::symbol_instances;

        let connection = &mut self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let ids = symbol_instances::table
            .filter(symbol_instances::eph_layer.eq(layer_id))
            .select(symbol_instances::id)
            .load::<i64>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get eph instance IDs: {}", e))?;

        Ok(ids)
    }

    /// Get the ephemeral layer ID(s) for a given instance ID.
    pub async fn get_eph_layer_for_instance(&self, instance_id: i64) -> Result<Vec<i64>> {
        use crate::schema_diesel::symbol_instances;

        let connection = &mut self.pool.get().await
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


    /// Delete ephemeral layers older than the given duration. CASCADE cleans up rows.
    pub async fn purge_old_eph_layers(&self, older_than: Duration) -> Result<u64> {
        let connection = &mut self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let interval_secs = older_than.as_secs() as i64;
        let sql = format!(
            "DELETE FROM index.eph_layers \
             WHERE last_used < now() - make_interval(secs => $1) AND {}",
            NOT_CANARY_PREDICATE,
        );
        let result = diesel::sql_query(sql)
            .bind::<diesel::sql_types::BigInt, _>(interval_secs)
            .execute(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to purge old eph layers: {}", e))?;

        Ok(result as u64)
    }

    /// Delete a single ephemeral layer by ID. CASCADE cleans up symbol/instance/ref rows.
    pub async fn delete_eph_layer(&self, layer_id: i64) -> Result<()> {
        let connection = &mut self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::sql_query("DELETE FROM index.eph_layers WHERE id = $1")
            .bind::<diesel::sql_types::BigInt, _>(layer_id)
            .execute(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to delete eph layer: {}", e))?;

        Ok(())
    }

    /// Touch the last_used timestamp of an ephemeral layer (batched: only if stale).
    pub async fn touch_eph_layer(&self, layer_id: i64) -> Result<()> {
        let connection = &mut self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        diesel::sql_query(
            "UPDATE index.eph_layers SET last_used = now() \
             WHERE id = $1 AND last_used < now() - interval '1 hour'"
        )
            .bind::<diesel::sql_types::BigInt, _>(layer_id)
            .execute(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to touch eph layer: {}", e))?;

        Ok(())
    }

    /// Check if a symbol exists in the persistent index or in the given ephemeral layers.
    pub async fn symbol_exists(&self, symbol_id: i64, eph_ids: &[i64]) -> Result<bool> {
        use crate::schema_diesel::symbols;

        let connection = &mut self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let eph_ids_owned = eph_ids.to_vec();
        let exists = symbols::table
            .filter(symbols::id.eq(symbol_id))
            .filter(
                symbols::eph_layer.is_null()
                    .or(symbols::eph_layer.eq_any(eph_ids_owned))
            )
            .select(symbols::id)
            .first::<i64>(&mut *connection)
            .await
            .optional()
            .map_err(|e| anyhow::anyhow!("Failed to check symbol existence: {}", e))?;

        Ok(exists.is_some())
    }

    /// Find file objects matching the given path suffix, optionally filtered by project name.
    pub async fn find_objects_by_path(
        &self,
        path: &str,
        project_name: Option<&str>,
    ) -> Result<Vec<(FileId, crate::symbols::ProjectId)>> {
        use crate::schema_diesel::*;

        let connection = &mut self.pool.get().await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let escaped = path.replace('\\', r"\\").replace('%', r"\%").replace('_', r"\_");
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


/// Helper for `create_eph_layer` RETURNING id + created flag.
#[derive(diesel::QueryableByName)]
struct CreateLayerRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    id: i64,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    created: bool,
}

/// Edge discovered between two selected instances via DB query.
#[derive(diesel::QueryableByName, Debug, Clone)]
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
    finished: bool,
}

impl<'a> EphTransaction<'a> {
    pub fn layer_id(&self) -> i64 {
        self.layer_id
    }

    pub fn created(&self) -> bool {
        self.created
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

        let names:        Vec<&str>        = rows.iter().map(|r| r.name.as_str()).collect();
        let paths:        Vec<&str>        = rows.iter().map(|r| r.path.as_str()).collect();
        let project_ids:  Vec<i32>         = rows.iter().map(|r| r.project_id).collect();
        let symbol_types: Vec<i32>         = rows.iter().map(|r| r.symbol_type).collect();
        let scopes:       Vec<Option<i32>> = rows.iter().map(|r| r.scope).collect();
        let leaf_names:   Vec<&str>        = rows.iter().map(|r| r.leaf_name.as_str()).collect();

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

        let sym_ids:        Vec<i64> = rows.iter().map(|r| r.symbol_id).collect();
        let object_ids:     Vec<i32> = rows.iter().map(|r| r.object_id).collect();
        let starts:         Vec<i32> = try_offsets(rows.iter().map(|r| r.start), "start")?;
        let ends:           Vec<i32> = try_offsets(rows.iter().map(|r| r.end),   "end")?;
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

        let to_symbols:   Vec<i64> = rows.iter().map(|r| r.to_symbol).collect();
        let from_objects: Vec<i32> = rows.iter().map(|r| r.from_object).collect();
        let starts:       Vec<i32> = try_offsets(rows.iter().map(|r| r.start), "start")?;
        let ends:         Vec<i32> = try_offsets(rows.iter().map(|r| r.end),   "end")?;

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
        let _ = diesel::sql_query("ROLLBACK")
            .execute(&mut *self.conn)
            .await;
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
