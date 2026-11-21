//! The bodies and outer queries of the reads that are written in steps (see
//! [`super::relation`]): containment children and the edges between a set of
//! instances.
//!
//! Why `has_children` has a step at all.  PG's planner picks a pathological
//! nested-loop join order for it when its `id = ANY($source_ids)` filter is
//! parameterised and the `eph` array is empty: it underestimates the
//! candidate set's cardinality and drives the join from the wrong side,
//! GIST-scanning the 27M-row `symbol_instances` table.  Materialising the
//! source rows first ([`has_children_candidates`]) gives the planner an exact
//! row count, so it picks a from-candidates-driven plan with index lookups.
//! On the user's reference query, the worst-case `select_has_children` call
//! drops from ~42 ms to ~20 ms in PG.
//!
//! The outer query stays a typed Diesel query either way, so results load
//! into the natural tuple `(Symbol, SymbolInstance, Symbol, SymbolInstance,
//! Object)` via the existing `Queryable` impls; only how its source rows are
//! named differs, and that is an [`InstanceSet`].
//!
//! The step is used only when the caller's filter has no
//! `compose_has_children()` expression (the common case).  When a filter is
//! present, the caller binds the ids directly.

use diesel::pg::Pg;
use diesel::prelude::*;
use diesel::query_builder::{
    AstPass, BoxedSelectStatement, FromClause, Query, QueryFragment, QueryId,
};

use super::mixins::{
    EphVisibility, HasChildrenQuery, CONTAINED_INSTANCE_ALIAS, CONTAINED_SYMBOL_ALIAS,
    CONTAINED_TYPE_ALIAS,
};
use super::relation::{InstanceSet, Relation};
use crate::models_diesel::{Object, Symbol, SymbolInstance};

/// The `has_children` query: what the `source` instances contain.
///
/// `source` says which `symbol_instances` rows take part in the containment
/// join: ids bound directly, or the rows of [`has_children_candidates`]
/// materialised in the same statement.  The joins, the eph-visibility
/// filters and the projection are the same either way.
///
/// Diesel accumulates `.filter(...)` calls into a single WHERE clause
/// (ANDed together) regardless of their position relative to the joins, so
/// where the source test is applied does not change the SQL's meaning.
pub(super) fn build_has_children_query(
    source: &InstanceSet,
    vis: &EphVisibility,
) -> HasChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let contained_instance = CONTAINED_INSTANCE_ALIAS;
    let contained_symbol = CONTAINED_SYMBOL_ALIAS;
    let contained_type = CONTAINED_TYPE_ALIAS;

    symbol_instances::dsl::symbol_instances
        .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
        .inner_join(
            symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)),
        )
        .inner_join(objects::dsl::objects.on(objects::dsl::id.eq(symbol_instances::dsl::object_id)))
        .inner_join(
            contained_instance.on(contained_instance
                .field(symbol_instances::dsl::object_id)
                .eq(symbol_instances::dsl::object_id)),
        )
        .inner_join(
            contained_symbol.on(contained_symbol
                .field(symbols::dsl::id)
                .eq(contained_instance.field(symbol_instances::dsl::symbol))),
        )
        .inner_join(
            contained_type.on(contained_type
                .field(symbol_types::dsl::id)
                .eq(contained_symbol.field(symbols::dsl::symbol_type))),
        )
        .filter(source.holds("symbol_instances.id"))
        // Descendants by DFS interval; see DirectOnlyMixin for why ranges
        // cannot answer this.
        .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
            "symbol_instances.dfs_enter < contained_instances.dfs_enter \
             AND contained_instances.dfs_enter < symbol_instances.dfs_exit",
        ))
        .filter(symbol_instances::dsl::id.ne(contained_instance.field(symbol_instances::dsl::id)))
        // Ephemeral visibility — source and aliased (contained) tables,
        // rendered by the partition token.
        .filter(vis.pred("symbols.layer"))
        .filter(vis.pred("symbol_instances.layer"))
        .filter(vis.pred("contained_symbols.layer"))
        .filter(vis.pred("contained_instances.layer"))
        .select((
            Symbol::as_select(),
            SymbolInstance::as_select(),
            contained_symbol.fields(crate::schema_diesel::symbols::all_columns),
            contained_instance.fields(crate::schema_diesel::symbol_instances::all_columns),
            Object::as_select(),
        ))
        .into_boxed::<Pg>()
}

/// The source instances of a `has_children` read as a step of their own:
/// the visible ones among `source_ids`, by id.  Typed Diesel so the binds
/// (`source_ids`, `eph_ids`) are emitted via the normal DSL mechanism.
pub(super) fn has_children_candidates(source_ids: Vec<i64>, vis: &EphVisibility) -> Relation {
    use crate::schema_diesel::symbol_instances;
    Relation::new(
        "candidates",
        symbol_instances::table
            .filter(symbol_instances::id.eq_any(source_ids))
            // Subquery position: candidate-set membership, not a selected row.
            .filter(vis.subquery_pred("symbol_instances.layer"))
            .select(symbol_instances::id)
            .into_boxed::<Pg>(),
    )
}

// ============================================================================
// The edges between a set of instances (`find_edges_between`)
// ============================================================================

use diesel::sql_types::{BigInt, Int4range, Integer, SmallInt};

/// The two steps of `find_edges_between`, in order: `candidates`, the full
/// `symbol_instances` rows (id + symbol + object_id + offset_range + layer +
/// DFS interval) of the given instances with the ephemeral-visibility
/// predicate applied, and `targets`, the instances an edge may end on (see
/// [`EdgeTargets`]).  The candidates' binds are typed Diesel.
pub(super) fn find_edges_relations(source_ids: Vec<i64>, vis: &EphVisibility) -> Vec<Relation> {
    vec![
        Relation::new("candidates", find_edges_candidates(source_ids, vis)),
        Relation::new("targets", EdgeTargets),
    ]
}

fn find_edges_candidates(
    source_ids: Vec<i64>,
    vis: &EphVisibility,
) -> BoxedSelectStatement<
    'static,
    (
        BigInt,
        BigInt,
        Integer,
        Int4range,
        BigInt,
        BigInt,
        BigInt,
        Integer,
    ),
    FromClause<crate::schema_diesel::symbol_instances::table>,
    Pg,
> {
    use crate::schema_diesel::symbol_instances;
    symbol_instances::table
        .filter(symbol_instances::id.eq_any(source_ids))
        // Subquery position: the candidate set feeds BOTH edge endpoints.
        // In the eph branch it must contain persistent AND eph rows (a
        // mixed edge pairs them); the outer guard makes the branches
        // disjoint at the edge level, not here.
        .filter(vis.subquery_pred("symbol_instances.layer"))
        .select((
            symbol_instances::id,
            symbol_instances::symbol,
            symbol_instances::object_id,
            symbol_instances::offset_range,
            symbol_instances::layer,
            // The edges query joins refs by DFS interval, so `candidates` has
            // to carry it.  Without these two the join could not resolve
            // from_inst.dfs_enter and the query returned no edges at all.
            symbol_instances::dfs_enter,
            symbol_instances::dfs_exit,
            // A relation row belongs to the candidate one level above it
            // (`attribution::belongs_to`), so the level comes along as well.
            symbol_instances::nesting_level,
        ))
        .into_boxed::<Pg>()
}

/// Result-row `SqlType` for [`EdgesBetween`].
/// Matches the column order in `ImplicitEdge`.
pub(super) type FindEdgesRowSqlType = (
    BigInt,    // ref_id
    BigInt,    // to_symbol
    Integer,   // from_object
    Int4range, // from_offset_range
    BigInt,    // to_instance_id
    BigInt,    // from_instance_id
    BigInt,    // sr_layer
    BigInt,    // from_layer
    BigInt,    // to_layer
    SmallInt,  // roles
    SmallInt,  // relation
);

/// The instances an edge may end on: at most two per symbol of `candidates`.
///
/// `to_inst` contributes only its id (as the edge's target instance) and its
/// layer; `DISTINCT ON (from_inst.id, sr.id)` then throws away every row but
/// the one with the smallest `to_inst.id`.  Joining ALL co-instances of the
/// target symbol to discard all but one is quadratic in exactly the shape
/// this codebase produces: a module symbol with 931 instances made this an
/// 865,830-row self-join and 3.4M buffer hits to return 191 edges.
///
/// Two rows per symbol are provably enough to pick that winner:
///   * `from_inst.id != to_inst.id` can eliminate at most ONE row
///     (`from_inst` is a single row), so the winner is the smallest id or the
///     second smallest;
///   * the eph branch's disjointness guard eliminates only ROOT to_insts, and
///     root ids are positive while eph ids are negative -- so roots sort
///     last, and if the two smallest are roots then every later row is a root
///     as well and is eliminated with them.
/// Either way the surviving minimum is within `rn <= 2`.
struct EdgeTargets;

impl QueryFragment<Pg> for EdgeTargets {
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        out.push_sql(
            "SELECT symbol, id, layer FROM ( \
                 SELECT symbol, id, layer, \
                        row_number() OVER (PARTITION BY symbol ORDER BY id) AS rn \
                 FROM candidates \
             ) ranked WHERE rn <= 2",
        );
        Ok(())
    }
}

/// The outer query of `find_edges_between`, over the relations of
/// [`find_edges_relations`].
///
/// Raw SQL: no Diesel model matches its projection (DISTINCT ON + custom
/// column aliases).  Its reference to the eph-IDs array is bound via
/// `push_bind_param` so all binds in the final query are typed.
///
/// Result loads as `Vec<ImplicitEdge>` via the positional `Queryable` derive
/// on `ImplicitEdge`; the `SqlType` is [`FindEdgesRowSqlType`].
pub(super) struct EdgesBetween {
    /// Branch snapshot from the partition token (see `EphVisibility`):
    /// controls the outer `sr.layer` visibility and, for the eph
    /// branch, the disjointness guard over (sr, from_inst, to_inst).
    pub vis: super::mixins::VisibilitySpec,
    /// E1 assembly cap: `Some(edge_cap + 1)` bounds out-edges per source node
    /// (the `+1` is the sentinel that lets the caller tell "exactly cap" from
    /// "more than cap"); `None` disables it entirely — the query is then
    /// byte-for-byte what it was before E1.  Assembly capping, not soundness:
    /// hitting it never flags a selection, only records the node as paginable.
    pub edge_limit: Option<i64>,
}

impl QueryId for EdgesBetween {
    type QueryId = ();
    const HAS_STATIC_QUERY_ID: bool = false;
}

impl Query for EdgesBetween {
    type SqlType = FindEdgesRowSqlType;
}

impl QueryFragment<Pg> for EdgesBetween {
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        use super::mixins::VisibilitySpec;
        use diesel::sql_types::Array;
        // E1: when a per-node edge cap is active, bracket the DISTINCT ON below
        // in an outer row_number() partition so each source node keeps at most
        // `edge_cap + 1` edges.  When disabled the query is unchanged.
        if self.edge_limit.is_some() {
            out.push_sql(
                "SELECT ref_id, to_symbol, from_object, from_offset_range, \
                        to_instance_id, from_instance_id, sr_layer, from_layer, to_layer, \
                        roles, relation \
                 FROM ( SELECT de.*, \
                          row_number() OVER (PARTITION BY de.from_instance_id \
                                             ORDER BY de.ref_id) AS edge_rn \
                        FROM ( ",
            );
        }
        out.push_sql(
            "SELECT DISTINCT ON (from_inst.id, sr.id) \
                  sr.id AS ref_id, sr.to_symbol, sr.from_object, sr.from_offset_range, \
                  to_inst.id AS to_instance_id, \
                  from_inst.id AS from_instance_id, \
                  sr.layer AS sr_layer, \
                  from_inst.layer AS from_layer, \
                  to_inst.layer AS to_layer, \
                  sr.roles, sr.relation \
              FROM candidates from_inst \
              JOIN index.symbol_refs sr \
                  ON sr.from_object = from_inst.object_id \
                  AND ",
        );
        out.push_sql(&super::attribution::belongs_to("sr", "from_inst"));
        out.push_sql(
            " \
              JOIN targets to_inst \
                  ON to_inst.symbol = sr.to_symbol \
              WHERE from_inst.id != to_inst.id \
                AND ",
        );
        match &self.vis {
            VisibilitySpec::RootOnly(roots) => {
                out.push_sql("sr.layer = ANY(");
                out.push_bind_param::<Array<BigInt>, _>(roots)?;
                out.push_sql(")");
            }
            VisibilitySpec::EphTouching(visible) => {
                out.push_sql("sr.layer = ANY(");
                out.push_bind_param::<Array<BigInt>, _>(visible)?;
                // Disjointness guard: an all-root edge already belongs to
                // the persistent branch.  Sign form — within the visible
                // set, roots are exactly the positive ids (see
                // `EphVisibility::guard` for the equivalence argument).
                out.push_sql(
                    ") AND NOT (sr.layer > 0 \
                         AND from_inst.layer > 0 \
                         AND to_inst.layer > 0)",
                );
            }
            VisibilitySpec::Combined(visible) => {
                out.push_sql("sr.layer = ANY(");
                out.push_bind_param::<Array<BigInt>, _>(visible)?;
                out.push_sql(")");
            }
        }
        out.push_sql(" ORDER BY from_inst.id, sr.id, to_inst.id");
        // Close and filter the E1 wrapper opened above.
        if let Some(ref limit) = self.edge_limit {
            out.push_sql(") de ) ranked WHERE ranked.edge_rn <= ");
            out.push_bind_param::<diesel::sql_types::BigInt, _>(limit)?;
            out.push_sql(" ORDER BY ranked.from_instance_id, ranked.ref_id");
        }
        Ok(())
    }
}

// ============================================================================
// Probes
// ============================================================================

/// The id a probe returns when one of its roles overflowed its budget.
/// Persistent ids are positive and ephemeral ones negative, so no instance
/// has it.
pub(super) const PROBE_OVERFLOW: i64 = 0;
