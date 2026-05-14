use diesel::pg::Pg;
use diesel::query_builder::{AstPass, Query, QueryFragment, QueryId};
use diesel::sql_types::{Array, BigInt, Integer, Nullable, Text};

// ============================================================================
// EphemeralOverlay — per-query in-memory rows injected via CTE
// ============================================================================

/// Rows that will be UNION ALL'd onto the persistent index tables via CTE.
///
/// ID space reservations (to avoid collisions with persistent rows):
/// - `symbols.id`:          `i64::MAX - 1_000_000_000 ..= i64::MAX`
/// - `symbol_instances.id`: `i32::MAX - 1_000_000     ..= i32::MAX`
/// - `symbol_refs.id`:      `i32::MAX - 1_000_000     ..= i32::MAX`
///
/// For thread-safe ID allocation across concurrent selectors, callers must
/// hold `Arc<AtomicI64>` / `Arc<AtomicI32>` counters (added in PR 2).
#[derive(Debug, Clone, Default)]
pub struct EphemeralOverlay {
    // ---- symbols ----
    pub symbol_ids: Vec<i64>,
    pub symbol_names: Vec<String>,
    /// Stored as text; cast to `ltree` inside the CTE.
    pub symbol_paths: Vec<String>,
    pub symbol_project_ids: Vec<i32>,
    pub symbol_types: Vec<i32>,
    /// `None` → NULL in the DB column.
    pub symbol_scopes: Vec<Option<i32>>,
    pub symbol_leaf_names: Vec<String>,

    // ---- symbol_instances ----
    pub instance_ids: Vec<i32>,
    pub instance_symbols: Vec<i64>,
    pub instance_object_ids: Vec<i32>,
    /// Parallel to `instance_offset_ends`; together they form `int4range([start, end))`.
    pub instance_offset_starts: Vec<i32>,
    pub instance_offset_ends: Vec<i32>,
    pub instance_types: Vec<i32>,

    // ---- symbol_refs ----
    pub ref_ids: Vec<i32>,
    pub ref_to_symbols: Vec<i64>,
    pub ref_from_objects: Vec<i32>,
    /// Parallel to `ref_from_offset_ends`.
    pub ref_from_offset_starts: Vec<i32>,
    pub ref_from_offset_ends: Vec<i32>,
}

impl EphemeralOverlay {
    pub fn empty() -> Self {
        Self::default()
    }

    pub fn is_empty(&self) -> bool {
        self.symbol_ids.is_empty()
            && self.instance_ids.is_empty()
            && self.ref_ids.is_empty()
    }

    /// Merge `other` into `self`, appending all its rows.
    pub fn merge(&mut self, other: EphemeralOverlay) {
        self.symbol_ids.extend(other.symbol_ids);
        self.symbol_names.extend(other.symbol_names);
        self.symbol_paths.extend(other.symbol_paths);
        self.symbol_project_ids.extend(other.symbol_project_ids);
        self.symbol_types.extend(other.symbol_types);
        self.symbol_scopes.extend(other.symbol_scopes);
        self.symbol_leaf_names.extend(other.symbol_leaf_names);

        self.instance_ids.extend(other.instance_ids);
        self.instance_symbols.extend(other.instance_symbols);
        self.instance_object_ids.extend(other.instance_object_ids);
        self.instance_offset_starts.extend(other.instance_offset_starts);
        self.instance_offset_ends.extend(other.instance_offset_ends);
        self.instance_types.extend(other.instance_types);

        self.ref_ids.extend(other.ref_ids);
        self.ref_to_symbols.extend(other.ref_to_symbols);
        self.ref_from_objects.extend(other.ref_from_objects);
        self.ref_from_offset_starts.extend(other.ref_from_offset_starts);
        self.ref_from_offset_ends.extend(other.ref_from_offset_ends);
    }

    /// Returns a SQL `WITH` prefix that defines `all_symbols`, `all_instances`,
    /// and `all_refs` as UNION ALL of the persistent tables and empty ephemeral
    /// sets (as inline SQL literals). Use this for raw `diesel::sql_query` calls
    /// where the CTE must come before the main query and bound parameters for
    /// the CTE would require complex parameter-offset bookkeeping.
    ///
    /// For typed Diesel queries, use [`WithOverlay`] instead, which uses proper
    /// bound parameters.
    pub fn static_cte_prefix() -> &'static str {
        "\
WITH eph_symbols(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name) AS (\
    SELECT id, name, symbol_path::ltree, project_id, symbol_type, symbol_scope, leaf_name \
    FROM unnest(\
        ARRAY[]::int8[], ARRAY[]::text[], ARRAY[]::text[], \
        ARRAY[]::int4[], ARRAY[]::int4[], ARRAY[]::int4[], ARRAY[]::text[]\
    ) AS t(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name)\
), \
all_symbols(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name) AS (\
    SELECT id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name \
    FROM index.symbols \
    UNION ALL \
    SELECT id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name \
    FROM eph_symbols\
), \
eph_instances(id, symbol, object_id, offset_range, instance_type) AS (\
    SELECT id, symbol, object_id, int4range(offset_range_start, offset_range_end, '[)'), instance_type \
    FROM unnest(\
        ARRAY[]::int4[], ARRAY[]::int8[], ARRAY[]::int4[], \
        ARRAY[]::int4[], ARRAY[]::int4[], ARRAY[]::int4[]\
    ) AS t(id, symbol, object_id, offset_range_start, offset_range_end, instance_type)\
), \
all_instances(id, symbol, object_id, offset_range, instance_type) AS (\
    SELECT id, symbol, object_id, offset_range, instance_type \
    FROM index.symbol_instances \
    UNION ALL \
    SELECT id, symbol, object_id, offset_range, instance_type \
    FROM eph_instances\
), \
eph_refs(id, to_symbol, from_object, from_offset_range) AS (\
    SELECT id, to_symbol, from_object, int4range(from_offset_range_start, from_offset_range_end, '[)') \
    FROM unnest(\
        ARRAY[]::int4[], ARRAY[]::int8[], ARRAY[]::int4[], \
        ARRAY[]::int4[], ARRAY[]::int4[]\
    ) AS t(id, to_symbol, from_object, from_offset_range_start, from_offset_range_end)\
), \
all_refs(id, to_symbol, from_object, from_offset_range) AS (\
    SELECT id, to_symbol, from_object, from_offset_range \
    FROM index.symbol_refs \
    UNION ALL \
    SELECT id, to_symbol, from_object, from_offset_range \
    FROM eph_refs\
) "
    }
}

// ============================================================================
// WithOverlay<Q> — prepends the ephemeral CTE prelude before any Diesel query
// ============================================================================

/// Wraps a Diesel query with an ephemeral-overlay CTE prelude.
///
/// The wrapped query must reference `all_symbols`, `all_instances`, and
/// `all_refs` (via the renamed `table!` macros in `schema_diesel.rs`) rather
/// than `index.symbols`, `index.symbol_instances`, `index.symbol_refs`.
///
/// `WithOverlay::walk_ast` always emits the CTE prelude.  When the overlay is
/// empty the `eph_*` CTEs produce zero rows (via empty `unnest` arrays), and
/// `all_*` is equivalent to the underlying persistent table.
pub struct WithOverlay<Q> {
    pub overlay: EphemeralOverlay,
    pub inner: Q,
}

impl<Q> WithOverlay<Q> {
    pub fn new(overlay: EphemeralOverlay, inner: Q) -> Self {
        Self { overlay, inner }
    }
}

impl<Q: QueryId> QueryId for WithOverlay<Q> {
    type QueryId = ();
    // Never cache — the CTE prelude content varies with overlay contents.
    const HAS_STATIC_QUERY_ID: bool = false;
}

impl<Q: Query> Query for WithOverlay<Q> {
    type SqlType = Q::SqlType;
}

impl<Q: QueryFragment<Pg> + Send> QueryFragment<Pg> for WithOverlay<Q> {
    fn walk_ast<'b>(&'b self, mut pass: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        // Disable prepared-statement caching: the CTE content changes per overlay.
        pass.unsafe_to_cache_prepared();

        if self.overlay.is_empty() {
            // For empty overlays emit CTEs with inline empty-array SQL literals.
            // This avoids binding 18 parameters per query while still defining the
            // `all_*` CTE names that the inner query references.
            pass.push_sql(EphemeralOverlay::static_cte_prefix());
        } else {
            // Non-empty: bind actual overlay rows as array parameters.
            // Parameter order: symbols (7) → instances (6) → refs (5) = 18 total.
            // Inner query parameters start at $19.
            pass.push_sql(
                "WITH eph_symbols(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name) AS (\
                    SELECT id, name, symbol_path::ltree, project_id, symbol_type, symbol_scope, leaf_name \
                    FROM unnest(",
            );
            pass.push_bind_param::<Array<BigInt>, _>(&self.overlay.symbol_ids)?;
            pass.push_sql("::int8[],");
            pass.push_bind_param::<Array<Text>, _>(&self.overlay.symbol_names)?;
            pass.push_sql("::text[],");
            pass.push_bind_param::<Array<Text>, _>(&self.overlay.symbol_paths)?;
            pass.push_sql("::text[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.symbol_project_ids)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.symbol_types)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Nullable<Integer>>, _>(&self.overlay.symbol_scopes)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Text>, _>(&self.overlay.symbol_leaf_names)?;
            pass.push_sql(
                "::text[]) AS t(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name)\
                ), \
                all_symbols(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name) AS (\
                    SELECT id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name \
                    FROM index.symbols \
                    UNION ALL \
                    SELECT id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name \
                    FROM eph_symbols\
                ), \
                eph_instances(id, symbol, object_id, offset_range, instance_type) AS (\
                    SELECT id, symbol, object_id, int4range(offset_range_start, offset_range_end, '[)'), instance_type \
                    FROM unnest(",
            );
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.instance_ids)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<BigInt>, _>(&self.overlay.instance_symbols)?;
            pass.push_sql("::int8[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.instance_object_ids)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.instance_offset_starts)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.instance_offset_ends)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.instance_types)?;
            pass.push_sql(
                "::int4[]) AS t(id, symbol, object_id, offset_range_start, offset_range_end, instance_type)\
                ), \
                all_instances(id, symbol, object_id, offset_range, instance_type) AS (\
                    SELECT id, symbol, object_id, offset_range, instance_type \
                    FROM index.symbol_instances \
                    UNION ALL \
                    SELECT id, symbol, object_id, offset_range, instance_type \
                    FROM eph_instances\
                ), \
                eph_refs(id, to_symbol, from_object, from_offset_range) AS (\
                    SELECT id, to_symbol, from_object, int4range(from_offset_range_start, from_offset_range_end, '[)') \
                    FROM unnest(",
            );
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.ref_ids)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<BigInt>, _>(&self.overlay.ref_to_symbols)?;
            pass.push_sql("::int8[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.ref_from_objects)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.ref_from_offset_starts)?;
            pass.push_sql("::int4[],");
            pass.push_bind_param::<Array<Integer>, _>(&self.overlay.ref_from_offset_ends)?;
            pass.push_sql(
                "::int4[]) AS t(id, to_symbol, from_object, from_offset_range_start, from_offset_range_end)\
                ), \
                all_refs(id, to_symbol, from_object, from_offset_range) AS (\
                    SELECT id, to_symbol, from_object, from_offset_range \
                    FROM index.symbol_refs \
                    UNION ALL \
                    SELECT id, to_symbol, from_object, from_offset_range \
                    FROM eph_refs\
                ) ",
            );
        }

        self.inner.walk_ast(pass)
    }
}

/// Helper: wrap a Diesel query with an ephemeral-overlay CTE prelude.
pub fn with_overlay<Q>(overlay: &EphemeralOverlay, inner: Q) -> WithOverlay<Q> {
    WithOverlay::new(overlay.clone(), inner)
}

// ============================================================================
// ID-space helpers
// ============================================================================

pub fn is_ephemeral_symbol_id(id: i64) -> bool {
    id >= i64::MAX - 1_000_000_000
}

pub fn is_ephemeral_instance_id(id: i32) -> bool {
    id >= i32::MAX - 1_000_000
}

pub fn is_ephemeral_ref_id(id: i32) -> bool {
    id >= i32::MAX - 1_000_000
}
