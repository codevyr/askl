use diesel::pg::Pg;
use diesel::query_builder::{AstPass, Query, QueryFragment, QueryId};
use diesel::sql_types::{Array, BigInt, Integer, Nullable, Text};
use std::collections::HashSet;
use std::sync::atomic::{AtomicI32, AtomicI64, Ordering};

// ============================================================================
// Per-table overlay row sets
// ============================================================================

/// Ephemeral rows for `index.symbols`.
#[derive(Debug, Clone, Default)]
pub struct OverlaySymbols {
    pub ids: Vec<i64>,
    pub names: Vec<String>,
    /// Stored as text; cast to `ltree` inside the CTE.
    pub paths: Vec<String>,
    pub project_ids: Vec<i32>,
    pub types: Vec<i32>,
    /// `None` → NULL in the DB column.
    pub scopes: Vec<Option<i32>>,
    pub leaf_names: Vec<String>,
}

impl OverlaySymbols {
    /// Append one symbol row.
    pub fn push(
        &mut self,
        id: i64,
        name: String,
        path: String,
        project_id: i32,
        symbol_type: i32,
        scope: Option<i32>,
        leaf_name: String,
    ) {
        self.ids.push(id);
        self.names.push(name);
        self.paths.push(path);
        self.project_ids.push(project_id);
        self.types.push(symbol_type);
        self.scopes.push(scope);
        self.leaf_names.push(leaf_name);
    }

    /// Merge another set into this one.
    ///
    /// Duplicate symbol rows are harmless in JOINs, so no deduplication is
    /// performed here.
    pub fn merge(&mut self, other: OverlaySymbols) {
        self.ids.extend(other.ids);
        self.names.extend(other.names);
        self.paths.extend(other.paths);
        self.project_ids.extend(other.project_ids);
        self.types.extend(other.types);
        self.scopes.extend(other.scopes);
        self.leaf_names.extend(other.leaf_names);
    }
}

/// Ephemeral rows for `index.symbol_instances`.
#[derive(Debug, Clone, Default)]
pub struct OverlayInstances {
    pub ids: Vec<i32>,
    pub symbols: Vec<i64>,
    pub object_ids: Vec<i32>,
    /// Parallel with `offset_ends`; together they form `int4range([start, end))`.
    pub offset_starts: Vec<i32>,
    pub offset_ends: Vec<i32>,
    pub types: Vec<i32>,
}

impl OverlayInstances {
    /// Append one instance row.
    pub fn push(
        &mut self,
        id: i32,
        symbol: i64,
        object_id: i32,
        offset_start: i32,
        offset_end: i32,
        instance_type: i32,
    ) {
        self.ids.push(id);
        self.symbols.push(symbol);
        self.object_ids.push(object_id);
        self.offset_starts.push(offset_start);
        self.offset_ends.push(offset_end);
        self.types.push(instance_type);
    }

    /// Merge another set into this one, deduplicating by instance ID.
    pub fn merge(&mut self, other: OverlayInstances) {
        let seen: HashSet<i32> = self.ids.iter().cloned().collect();
        for (i, &id) in other.ids.iter().enumerate() {
            if !seen.contains(&id) {
                self.push(
                    id,
                    other.symbols[i],
                    other.object_ids[i],
                    other.offset_starts[i],
                    other.offset_ends[i],
                    other.types[i],
                );
            }
        }
    }
}

/// Ephemeral rows for `index.symbol_refs`.
#[derive(Debug, Clone, Default)]
pub struct OverlayRefs {
    pub ids: Vec<i32>,
    pub to_symbols: Vec<i64>,
    pub from_objects: Vec<i32>,
    /// Parallel with `from_offset_ends`.
    pub from_offset_starts: Vec<i32>,
    pub from_offset_ends: Vec<i32>,
}

impl OverlayRefs {
    /// Append one ref row.
    pub fn push(
        &mut self,
        id: i32,
        to_symbol: i64,
        from_object: i32,
        from_offset_start: i32,
        from_offset_end: i32,
    ) {
        self.ids.push(id);
        self.to_symbols.push(to_symbol);
        self.from_objects.push(from_object);
        self.from_offset_starts.push(from_offset_start);
        self.from_offset_ends.push(from_offset_end);
    }

    /// Merge another set into this one, deduplicating by ref ID.
    pub fn merge(&mut self, other: OverlayRefs) {
        let seen: HashSet<i32> = self.ids.iter().cloned().collect();
        for (i, &id) in other.ids.iter().enumerate() {
            if !seen.contains(&id) {
                self.push(
                    id,
                    other.to_symbols[i],
                    other.from_objects[i],
                    other.from_offset_starts[i],
                    other.from_offset_ends[i],
                );
            }
        }
    }
}

// ============================================================================
// EphemeralOverlay — per-query in-memory rows injected via CTE
// ============================================================================

/// Rows that will be UNION ALL'd onto the persistent index tables via CTE.
///
/// ID space reservations (to avoid collisions with persistent rows):
/// - `symbols.id`:          `i64::MAX - 1_000_000_000 ..= i64::MAX`
/// - `symbol_instances.id`: `i32::MAX - 1_000_000     ..= i32::MAX`
/// - `symbol_refs.id`:      `i32::MAX - 1_000_000     ..= i32::MAX`
#[derive(Debug, Clone, Default)]
pub struct EphemeralOverlay {
    pub symbols: OverlaySymbols,
    pub instances: OverlayInstances,
    pub refs: OverlayRefs,
}

impl EphemeralOverlay {
    pub fn empty() -> Self {
        Self::default()
    }

    /// Merge `other` into `self`, appending all its rows.
    ///
    /// Instance and ref rows are deduplicated by ID so that a selector whose
    /// `select_from_all_impl` is somehow invoked twice does not produce
    /// duplicate CTE rows (which would cause duplicate nodes in results).
    /// Symbol rows are not deduplicated because they only participate in JOINs
    /// and duplicate symbol rows are harmless.
    pub fn merge(&mut self, other: EphemeralOverlay) {
        self.symbols.merge(other.symbols);
        self.instances.merge(other.instances);
        self.refs.merge(other.refs);
    }

    /// Returns a SQL `WITH` prefix for use in raw `diesel::sql_query` calls.
    ///
    /// Uses positional placeholders `$1`–`$18` for the 18 overlay array
    /// parameters (symbols $1–$7, instances $8–$13, refs $14–$18).
    /// Bind these in order, then bind any query-specific parameters at `$19+`.
    ///
    /// Parameter order:
    /// - $1  `symbols.ids`              `int8[]`
    /// - $2  `symbols.names`            `text[]`
    /// - $3  `symbols.paths`            `text[]`
    /// - $4  `symbols.project_ids`      `int4[]`
    /// - $5  `symbols.types`            `int4[]`
    /// - $6  `symbols.scopes`           `int4[]`
    /// - $7  `symbols.leaf_names`       `text[]`
    /// - $8  `instances.ids`            `int4[]`
    /// - $9  `instances.symbols`        `int8[]`
    /// - $10 `instances.object_ids`     `int4[]`
    /// - $11 `instances.offset_starts`  `int4[]`
    /// - $12 `instances.offset_ends`    `int4[]`
    /// - $13 `instances.types`          `int4[]`
    /// - $14 `refs.ids`                 `int4[]`
    /// - $15 `refs.to_symbols`          `int8[]`
    /// - $16 `refs.from_objects`        `int4[]`
    /// - $17 `refs.from_offset_starts`  `int4[]`
    /// - $18 `refs.from_offset_ends`    `int4[]`
    pub fn parameterized_cte_prefix() -> &'static str {
        "\
WITH eph_symbols(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name) AS (\
    SELECT id, name, symbol_path::ltree, project_id, symbol_type, symbol_scope, leaf_name \
    FROM unnest(\
        $1::int8[], $2::text[], $3::text[], \
        $4::int4[], $5::int4[], $6::int4[], $7::text[]\
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
        $8::int4[], $9::int8[], $10::int4[], \
        $11::int4[], $12::int4[], $13::int4[]\
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
        $14::int4[], $15::int8[], $16::int4[], \
        $17::int4[], $18::int4[]\
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

        // Always bind overlay arrays as parameters.
        // Empty arrays produce zero rows from unnest() so the all_* CTEs are
        // equivalent to the persistent tables when the overlay is empty.
        // Parameter order: symbols (7) → instances (6) → refs (5) = 18 total.
        // Inner query parameters start at $19.
        let sym = &self.overlay.symbols;
        let inst = &self.overlay.instances;
        let refs = &self.overlay.refs;

        pass.push_sql(
            "WITH eph_symbols(id, name, symbol_path, project_id, symbol_type, symbol_scope, leaf_name) AS (\
                SELECT id, name, symbol_path::ltree, project_id, symbol_type, symbol_scope, leaf_name \
                FROM unnest(",
        );
        pass.push_bind_param::<Array<BigInt>, _>(&sym.ids)?;
        pass.push_sql("::int8[],");
        pass.push_bind_param::<Array<Text>, _>(&sym.names)?;
        pass.push_sql("::text[],");
        pass.push_bind_param::<Array<Text>, _>(&sym.paths)?;
        pass.push_sql("::text[],");
        pass.push_bind_param::<Array<Integer>, _>(&sym.project_ids)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Integer>, _>(&sym.types)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Nullable<Integer>>, _>(&sym.scopes)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Text>, _>(&sym.leaf_names)?;
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
        pass.push_bind_param::<Array<Integer>, _>(&inst.ids)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<BigInt>, _>(&inst.symbols)?;
        pass.push_sql("::int8[],");
        pass.push_bind_param::<Array<Integer>, _>(&inst.object_ids)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Integer>, _>(&inst.offset_starts)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Integer>, _>(&inst.offset_ends)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Integer>, _>(&inst.types)?;
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
        pass.push_bind_param::<Array<Integer>, _>(&refs.ids)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<BigInt>, _>(&refs.to_symbols)?;
        pass.push_sql("::int8[],");
        pass.push_bind_param::<Array<Integer>, _>(&refs.from_objects)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Integer>, _>(&refs.from_offset_starts)?;
        pass.push_sql("::int4[],");
        pass.push_bind_param::<Array<Integer>, _>(&refs.from_offset_ends)?;
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

/// Minimum ID value that belongs to the ephemeral symbol ID space.
/// Persistent `symbols.id` values must be strictly below this threshold.
/// Must stay in sync with the CHECK constraint in the
/// `reserve_ephemeral_id_space` migration.
pub const EPHEMERAL_SYMBOL_ID_MIN: i64 = i64::MAX - 1_000_000_000; // 9_223_372_035_854_775_807

/// Minimum ID value that belongs to the ephemeral instance ID space.
/// Persistent `symbol_instances.id` values must be strictly below this threshold.
/// Must stay in sync with the CHECK constraint and sequence MAXVALUE in the
/// `reserve_ephemeral_id_space` migration.
pub const EPHEMERAL_INSTANCE_ID_MIN: i32 = i32::MAX - 1_000_000; // 2_146_483_647

/// Minimum ID value that belongs to the ephemeral ref ID space.
/// Persistent `symbol_refs.id` values must be strictly below this threshold.
/// Must stay in sync with the CHECK constraint and sequence MAXVALUE in the
/// `reserve_ephemeral_id_space` migration.
pub const EPHEMERAL_REF_ID_MIN: i32 = i32::MAX - 1_000_000; // 2_146_483_647

pub fn is_ephemeral_symbol_id(id: i64) -> bool {
    id >= EPHEMERAL_SYMBOL_ID_MIN
}

pub fn is_ephemeral_instance_id(id: i32) -> bool {
    id >= EPHEMERAL_INSTANCE_ID_MIN
}

pub fn is_ephemeral_ref_id(id: i32) -> bool {
    id >= EPHEMERAL_REF_ID_MIN
}

// ============================================================================
// Global counters for auto-allocation (used by composite verbs like loc).
// Decrement from MAX - 1; valid as long as within the ephemeral range.
// Upper bound (MAX) is deliberately skipped to avoid any sentinel confusion.
// Counter start values are EPHEMERAL_*_ID_MIN + range_size - 2, i.e. MAX - 1.
// ============================================================================

static GLOBAL_SYMBOL_ID: AtomicI64 = AtomicI64::new(i64::MAX - 1);
static GLOBAL_INSTANCE_ID: AtomicI32 = AtomicI32::new(i32::MAX - 1);
static GLOBAL_REF_ID: AtomicI32 = AtomicI32::new(i32::MAX - 1);

/// Allocate a fresh ephemeral symbol ID (auto-decrements from i64::MAX - 1).
pub fn alloc_ephemeral_symbol_id() -> i64 {
    let id = GLOBAL_SYMBOL_ID.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(
        is_ephemeral_symbol_id(id),
        "ephemeral symbol ID counter exhausted: {} is outside ephemeral range",
        id
    );
    id
}

/// Allocate a fresh ephemeral instance ID (auto-decrements from i32::MAX - 1).
pub fn alloc_ephemeral_instance_id() -> i32 {
    let id = GLOBAL_INSTANCE_ID.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(
        is_ephemeral_instance_id(id),
        "ephemeral instance ID counter exhausted: {} is outside ephemeral range",
        id
    );
    id
}

/// Allocate a fresh ephemeral ref ID (auto-decrements from i32::MAX - 1).
pub fn alloc_ephemeral_ref_id() -> i32 {
    let id = GLOBAL_REF_ID.fetch_sub(1, Ordering::Relaxed);
    debug_assert!(
        is_ephemeral_ref_id(id),
        "ephemeral ref ID counter exhausted: {} is outside ephemeral range",
        id
    );
    id
}
