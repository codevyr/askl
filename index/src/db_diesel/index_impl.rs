use anyhow::Result;
use diesel::connection::SimpleConnection;
use diesel::pg::{Pg, PgConnection};
use diesel::prelude::*;
use diesel::PgRangeExpressionMethods;
use diesel_async::pooled_connection::bb8;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncPgConnection, RunQueryDsl};
use diesel_migrations::MigrationHarness;

use crate::models_diesel::{ContentRow, Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::FileId;

use super::mixins::{
    CompositeFilter, CurrentQuery, OwnedSql,
    PARENT_DECLS_ALIAS, PARENT_SYMBOLS_ALIAS,
    CONTAINER_INSTANCE_ALIAS, CONTAINER_SYMBOL_ALIAS, CONTAINER_TYPE_ALIAS,
    CONTAINED_INSTANCE_ALIAS, CONTAINED_SYMBOL_ALIAS, CONTAINED_TYPE_ALIAS,
    ParentsQuery, ChildrenQuery, HasParentsQuery, HasChildrenQuery,
};
use super::selection::{ChildReference, HasChildReference, HasParentReference, ParentReference, Selection, SelectionNode};
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
        ids: Vec<i32>,
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
    Children(Vec<i32>),
    /// Resolved instances must be parents of these child IDs.
    Parents(Vec<i32>),
}

/// Build a base CurrentQuery (symbols ⋈ instances ⋈ projects ⋈ objects).
fn build_current_query() -> CurrentQuery<'static> {
    use crate::schema_diesel::*;
    symbols::dsl::symbols
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
        .into_boxed::<Pg>()
}

/// Resolve a CompositeFilter to instance IDs by running a CurrentQuery.
async fn resolve_filter_to_ids(
    filter: &CompositeFilter,
    role: Option<&ScopeRole>,
    conn: &mut Connection,
) -> Result<Vec<i32>> {
    use diesel::sql_types::Bool;

    let mut query = build_current_query();
    if let Some(expr) = filter.compose_current() {
        query = query.filter(expr);
    }

    // Add reference-based constraint when resolving scoped filters.
    match role {
        Some(ScopeRole::Children(parent_ids)) if !parent_ids.is_empty() => {
            let ids_csv = parent_ids.iter()
                .map(|id| id.to_string()).collect::<Vec<_>>().join(",");
            query = query.filter(OwnedSql::<Bool>::new(format!(
                "symbol_instances.id IN (\
                    SELECT si.id FROM index.symbol_refs sr \
                    JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                    JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                      AND pd.offset_range @> sr.from_offset_range \
                    WHERE pd.id IN ({ids_csv}))"
            )));
        }
        Some(ScopeRole::Parents(child_ids)) if !child_ids.is_empty() => {
            let ids_csv = child_ids.iter()
                .map(|id| id.to_string()).collect::<Vec<_>>().join(",");
            query = query.filter(OwnedSql::<Bool>::new(format!(
                "symbol_instances.id IN (\
                    SELECT pd.id FROM index.symbol_refs sr \
                    JOIN index.symbol_instances pd ON pd.object_id = sr.from_object \
                      AND pd.offset_range @> sr.from_offset_range \
                    JOIN index.symbol_instances si ON si.symbol = sr.to_symbol \
                    WHERE si.id IN ({ids_csv}))"
            )));
        }
        _ => {}
    }

    let results = query
        .load::<(Symbol, SymbolInstance, Object, Project)>(conn)
        .await
        .map_err(|e| anyhow::anyhow!("Failed to resolve filter to IDs: {}", e))?;
    let mut ids: Vec<i32> = results.iter().map(|(_, inst, _, _)| inst.id).collect();
    ids.sort_unstable();
    ids.dedup();
    Ok(ids)
}

/// Resolve a Scope's fields into a set of instance IDs for filtering.
async fn resolve_scope_ids(
    ids: &[i32],
    filter: &Option<CompositeFilter>,
    role: Option<&ScopeRole>,
    conn: &mut Connection,
) -> Result<Vec<i32>> {
    let mut all_ids = ids.to_vec();
    if let Some(ref f) = filter {
        all_ids.extend(resolve_filter_to_ids(f, role, conn).await?);
        all_ids.sort_unstable();
        all_ids.dedup();
    }
    Ok(all_ids)
}

// ============================================================================
// Shared query builders — used by both find_symbol and find_*_instance_ids
// ============================================================================

fn build_parents_query(
    source_ids: Vec<i32>,
) -> ParentsQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

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
        .select((
            SymbolRef::as_select(),
            Symbol::as_select(),
            SymbolInstance::as_select(),
            parent_decls.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

fn build_children_query(
    source_ids: Vec<i32>,
) -> ChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let parent_decls = PARENT_DECLS_ALIAS;
    let parent_symbols = PARENT_SYMBOLS_ALIAS;

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
    source_ids: Vec<i32>,
) -> HasParentsQuery<'static> {
    use crate::schema_diesel::*;

    let container_instance = CONTAINER_INSTANCE_ALIAS;
    let container_symbol = CONTAINER_SYMBOL_ALIAS;
    let container_type = CONTAINER_TYPE_ALIAS;

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
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            container_symbol.fields(crate::schema_diesel::symbols::all_columns),
            container_instance.fields(crate::schema_diesel::symbol_instances::all_columns),
        ))
        .into_boxed::<Pg>()
}

fn build_has_children_query(
    source_ids: Vec<i32>,
) -> HasChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let contained_instance = CONTAINED_INSTANCE_ALIAS;
    let contained_symbol = CONTAINED_SYMBOL_ALIAS;
    let contained_type = CONTAINED_TYPE_ALIAS;

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

impl Index {
    pub fn from_pool(pool: bb8::Pool<AsyncPgConnection>) -> Self {
        Self { pool, database_url: None }
    }

    fn build_async_manager(database_url: &str) -> AsyncDieselConnectionManager<AsyncPgConnection> {
        AsyncDieselConnectionManager::new(database_url)
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
            .test_on_check_out(true)
            .build(manager)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to create connection pool: {}", e))?;
        Ok(Self { pool, database_url: Some(database_url.to_string()) })
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
            .test_on_check_out(true)
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

    pub async fn load_test_input(&self, input_path: &str) -> Result<()> {
        let database_url = self.database_url.as_ref()
            .expect("load_test_input requires Index created via connect/connect_with_test_input");
        let connection = &mut <PgConnection as diesel::Connection>::establish(database_url)
            .map_err(|e| anyhow::anyhow!("Failed to establish connection: {}", e))?;

        connection.revert_all_migrations(super::MIGRATIONS).unwrap();
        connection
            .run_pending_migrations(super::MIGRATIONS)
            .unwrap();

        // Clear prepared statement cache after DDL changes to avoid
        // "cached plan must not change result type" errors.
        connection
            .batch_execute("DEALLOCATE ALL")
            .unwrap();

        Self::load_sql(connection, input_path);

        // Also clear async pool connections' prepared statement caches
        {
            let mut async_conn = self.pool.get().await
                .map_err(|e| anyhow::anyhow!("Failed to get async connection: {}", e))?;
            diesel::sql_query("DEALLOCATE ALL")
                .execute(&mut *async_conn)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to deallocate: {}", e))?;
        }

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
    ) -> Result<Selection> {
        use crate::schema_diesel::*;

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;
        let connection: &mut AsyncPgConnection = &mut *connection;

        let current = {
            let _select_current: tracing::span::EnteredSpan =
                tracing::info_span!("select_current").entered();

            let mut joined_query = build_current_query();

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
        let current_instance_ids: Vec<i32> =
            current.iter().map(|(_, inst, _, _)| inst.id).collect();

        let parents = match parent_scope {
            ScopeContext::Skip => vec![],
            ScopeContext::Unscoped => {
                let _parents_span: tracing::span::EnteredSpan =
                    tracing::info_span!("select_parents").entered();
                let mut parents_query = build_parents_query(current_instance_ids.clone());
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
                    tracing::info_span!("select_parents").entered();

                let role = ScopeRole::Parents(current_instance_ids.clone());
                let scope_ids = resolve_scope_ids(ids, scope_filter, Some(&role), connection).await?;

                let mut parents_query = build_parents_query(current_instance_ids.clone());
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
                    tracing::info_span!("select_children").entered();
                let mut children_query = build_children_query(current_instance_ids.clone());
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
                    tracing::info_span!("select_children").entered();

                let role = ScopeRole::Children(current_instance_ids.clone());
                let scope_ids = resolve_scope_ids(ids, scope_filter, Some(&role), connection).await?;

                let mut children_query = build_children_query(current_instance_ids.clone());
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
                tracing::info_span!("select_has_parents").entered();

            let mut has_parents_query = build_has_parents_query(current_instance_ids.clone());
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
                tracing::info_span!("select_has_children").entered();

            let mut has_children_query = build_has_children_query(current_instance_ids);
            if let Some(expr) = filter.compose_has_children() {
                has_children_query = has_children_query.filter(expr);
            }

            has_children_query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to load containment children: {}", e))?
        };

        let selection = {
            let _collect_span: tracing::span::EnteredSpan =
                tracing::info_span!("collect").entered();

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

        selection
    }

    /// Query child instance IDs directly from DB given parent instance IDs.
    pub async fn find_child_instance_ids(
        &self,
        parent_ids: &[i32],
        include_refs: bool,
        include_has: bool,
        filter: &CompositeFilter,
    ) -> Result<Vec<crate::symbols::SymbolInstanceId>> {
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut all_ids: Vec<i32> = Vec::new();

        if include_has {
            let mut query = build_has_children_query(parent_ids.to_vec());
            if let Some(expr) = filter.compose_has_children() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-child instance IDs: {}", e))?;
            all_ids.extend(results.iter().map(|(_, _, _, child_inst, _)| child_inst.id));
        }

        if include_refs {
            let mut query = build_children_query(parent_ids.to_vec());
            if let Some(expr) = filter.compose_children() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, Symbol, SymbolInstance, SymbolInstance, SymbolRef, Object)>(
                    &mut *connection,
                )
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-child instance IDs: {}", e))?;
            all_ids.extend(results.iter().map(|(_, _, callee_inst, _, _, _)| callee_inst.id));
        }

        all_ids.sort_unstable();
        all_ids.dedup();
        Ok(all_ids.into_iter().map(crate::symbols::SymbolInstanceId::new).collect())
    }

    /// Query parent instance IDs directly from DB given child instance IDs.
    pub async fn find_parent_instance_ids(
        &self,
        child_ids: &[i32],
        include_refs: bool,
        include_has: bool,
        filter: &CompositeFilter,
    ) -> Result<Vec<crate::symbols::SymbolInstanceId>> {
        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let mut all_ids: Vec<i32> = Vec::new();

        if include_refs {
            let mut query = build_parents_query(child_ids.to_vec());
            if let Some(expr) = filter.compose_parents() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(SymbolRef, Symbol, SymbolInstance, SymbolInstance)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find ref-parent instance IDs: {}", e))?;
            all_ids.extend(results.iter().map(|(_, _, _, parent_inst)| parent_inst.id));
        }

        if include_has {
            let mut query = build_has_parents_query(child_ids.to_vec());
            if let Some(expr) = filter.compose_has_parents() {
                query = query.filter(expr);
            }
            let results = query
                .load::<(Symbol, SymbolInstance, Symbol, SymbolInstance)>(&mut *connection)
                .await
                .map_err(|e| anyhow::anyhow!("Failed to find has-parent instance IDs: {}", e))?;
            all_ids.extend(results.iter().map(|(_, _, _, container_inst)| container_inst.id));
        }

        all_ids.sort_unstable();
        all_ids.dedup();
        Ok(all_ids.into_iter().map(crate::symbols::SymbolInstanceId::new).collect())
    }

    /// Discover all reference edges between a set of selected instances.
    pub async fn find_edges_between(
        &self,
        instance_ids: &[i32],
    ) -> Result<Vec<ImplicitEdge>> {
        if instance_ids.is_empty() {
            return Ok(vec![]);
        }

        let connection = &mut self
            .pool
            .get()
            .await
            .map_err(|e| anyhow::anyhow!("Failed to get connection: {}", e))?;

        let _span = tracing::info_span!("find_edges_between", count = instance_ids.len()).entered();

        let results = diesel::sql_query(
            "SELECT DISTINCT ON (from_inst.id, sr.id) \
                    sr.id AS ref_id, sr.to_symbol, sr.from_object, sr.from_offset_range, \
                    to_inst.id AS to_instance_id, \
                    from_inst.id AS from_instance_id \
             FROM index.symbol_instances from_inst \
             JOIN index.symbol_refs sr \
                 ON sr.from_object = from_inst.object_id \
                 AND from_inst.offset_range @> sr.from_offset_range \
             JOIN index.symbol_instances to_inst \
                 ON to_inst.symbol = sr.to_symbol \
             WHERE from_inst.id = ANY($1) \
               AND to_inst.id = ANY($1) \
               AND from_inst.id != to_inst.id \
             ORDER BY from_inst.id, sr.id, to_inst.id"
        )
            .bind::<diesel::sql_types::Array<diesel::sql_types::Integer>, _>(instance_ids)
            .load::<ImplicitEdge>(&mut *connection)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find edges between instances: {}", e))?;

        Ok(results)
    }

}

/// Edge discovered between two selected instances via DB query.
#[derive(diesel::QueryableByName, Debug, Clone)]
pub struct ImplicitEdge {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub ref_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub to_symbol: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub from_object: i32,
    #[diesel(sql_type = diesel::sql_types::Int4range)]
    pub from_offset_range: (std::ops::Bound<i32>, std::ops::Bound<i32>),
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub to_instance_id: i32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    pub from_instance_id: i32,
}
