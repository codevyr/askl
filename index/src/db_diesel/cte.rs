//! CTE-materialised variant of the `has_children` containment query.
//!
//! PG's planner picks a pathological nested-loop join order for the
//! `has_children` query when its `id = ANY($source_ids)` filter is
//! parameterised and the `eph` array is empty: it underestimates the
//! candidate set's cardinality and drives the join from the wrong
//! side, GIST-scanning the 27M-row `symbol_instances` table.
//!
//! The fix wraps the source-row filter in
//! `WITH candidates AS MATERIALIZED (…)`.  Materialisation gives the
//! planner an exact row count for the CTE, so it picks a
//! from-candidates-driven plan with index lookups.  On the user's
//! reference query, the worst-case `select_has_children` call drops
//! from ~42 ms to ~20 ms in PG.
//!
//! The pattern follows the `walk_ast`-delegates-to-typed-subqueries
//! technique from
//! <https://github.com/diesel-rs/diesel/discussions/4817#discussioncomment-14676297>:
//! [`CteHasChildren`] emits only the CTE prelude as raw SQL and then
//! walks two typed Diesel queries (the inner `cte_body` and the
//! `outer` select).  No flat `QueryableByName` row struct; results
//! still load into the natural tuple
//! `(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)` via
//! the existing `Queryable` impls.
//!
//! Used only when the caller's filter has no `compose_has_children()`
//! expression (the common case).  When a filter is present, the
//! caller falls back to [`build_has_children_query`]'s plain DSL form
//! because the mixin filter SQL fragments reference unaliased table
//! names that the DSL emits.

use diesel::pg::Pg;
use diesel::prelude::*;
use diesel::query_builder::{AstPass, BoxedSelectStatement, FromClause, Query, QueryFragment, QueryId};

use crate::models_diesel::{Object, Symbol, SymbolInstance};
use super::mixins::{
    HasChildrenBoolExpr, HasChildrenQuery,
    CONTAINED_INSTANCE_ALIAS, CONTAINED_SYMBOL_ALIAS, CONTAINED_TYPE_ALIAS,
};

/// Build the `has_children` query with `source_filter` as the
/// predicate that selects which source `symbol_instances` rows
/// participate in the containment join.  The rest of the joins,
/// eph_visibility filters, and projection are fixed.
///
/// Two callers parameterise this:
///   * [`build_has_children_query`] — filter is
///     `symbol_instances.id = ANY(source_ids)` (the plain form).
///   * [`build_has_children_query_against_cte`] — filter is
///     `symbol_instances.id IN (SELECT id FROM candidates)`,
///     pairing with the `WITH candidates AS MATERIALIZED (…)` CTE
///     emitted by [`CteHasChildren`].
///
/// Diesel accumulates `.filter(...)` calls into a single WHERE clause
/// (ANDed together) regardless of their position relative to the
/// joins, so applying `source_filter` after all the joins produces
/// the same SQL as inlining it mid-construction in the original
/// builder.
fn build_has_children_inner(
    eph_ids: &[i64],
    source_filter: HasChildrenBoolExpr,
) -> HasChildrenQuery<'static> {
    use crate::schema_diesel::*;

    let contained_instance = CONTAINED_INSTANCE_ALIAS;
    let contained_symbol = CONTAINED_SYMBOL_ALIAS;
    let contained_type = CONTAINED_TYPE_ALIAS;
    let eph_ids_owned = eph_ids.to_vec();

    symbol_instances::dsl::symbol_instances
        .inner_join(symbols::dsl::symbols.on(symbol_instances::dsl::symbol.eq(symbols::dsl::id)))
        .inner_join(symbol_types::dsl::symbol_types.on(symbols::dsl::symbol_type.eq(symbol_types::dsl::id)))
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
        .filter(source_filter)
        .filter(diesel::dsl::sql::<diesel::sql_types::Bool>(
            "symbol_instances.offset_range @> contained_instances.offset_range"
        ))
        .filter(symbol_types::dsl::level.ge(contained_type.field(symbol_types::dsl::level)))
        .filter(symbol_instances::dsl::id.ne(contained_instance.field(symbol_instances::dsl::id)))
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

/// Plain `has_children` query.  Used when a mixin filter on
/// `contained_instances` rules out the CTE form.
pub(super) fn build_has_children_query(
    source_ids: Vec<i64>,
    eph_ids: &[i64],
) -> HasChildrenQuery<'static> {
    use crate::schema_diesel::symbol_instances;
    let source_filter: HasChildrenBoolExpr =
        Box::new(symbol_instances::dsl::id.eq_any(source_ids));
    build_has_children_inner(eph_ids, source_filter)
}

/// Variant of [`build_has_children_query`] whose source-row filter is
/// `symbol_instances.id IN (SELECT id FROM candidates)`, where
/// `candidates` is a CTE supplied by an enclosing [`CteHasChildren`]
/// wrapper.  Result shape and joins are identical to
/// `build_has_children_query`; rows still deserialise as the natural
/// tuple `(Symbol, SymbolInstance, Symbol, SymbolInstance, Object)`.
pub(super) fn build_has_children_query_against_cte(eph_ids: &[i64]) -> HasChildrenQuery<'static> {
    let source_filter: HasChildrenBoolExpr = Box::new(
        diesel::dsl::sql::<diesel::sql_types::Bool>(
            "symbol_instances.id IN (SELECT id FROM candidates)"
        )
    );
    build_has_children_inner(eph_ids, source_filter)
}

/// Build the inner CTE body: the source-row filter, projected to just
/// `id`.  Typed Diesel so the binds (`source_ids`, `eph_ids`) are
/// emitted via the normal DSL mechanism.
pub(super) fn build_has_children_cte_body(
    source_ids: Vec<i64>,
    eph_ids: &[i64],
) -> BoxedSelectStatement<
    'static,
    diesel::sql_types::BigInt,
    FromClause<crate::schema_diesel::symbol_instances::table>,
    Pg,
> {
    use crate::schema_diesel::symbol_instances;
    let eph_ids_owned = eph_ids.to_vec();
    symbol_instances::table
        .filter(symbol_instances::id.eq_any(source_ids))
        .filter(symbol_instances::eph_layer.is_null()
            .or(symbol_instances::eph_layer.eq_any(eph_ids_owned)))
        .select(symbol_instances::id)
        .into_boxed::<Pg>()
}

/// Custom Diesel query that wraps the typed `HasChildrenQuery` with
/// `WITH candidates AS MATERIALIZED (…)`.  See the module-level docs
/// for the why; see `walk_ast` for the structural how.
pub(super) struct CteHasChildren<CteBody, Outer> {
    pub cte_body: CteBody,
    pub outer: Outer,
}

impl<CteBody, Outer> QueryId for CteHasChildren<CteBody, Outer> {
    type QueryId = ();
    const HAS_STATIC_QUERY_ID: bool = false;
}

impl<CteBody, Outer> Query for CteHasChildren<CteBody, Outer>
where
    Outer: Query,
{
    type SqlType = Outer::SqlType;
}

impl<CteBody, Outer> QueryFragment<Pg> for CteHasChildren<CteBody, Outer>
where
    CteBody: QueryFragment<Pg>,
    Outer: QueryFragment<Pg>,
{
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        out.push_sql("WITH candidates AS MATERIALIZED (");
        self.cte_body.walk_ast(out.reborrow())?;
        out.push_sql(") ");
        self.outer.walk_ast(out.reborrow())?;
        Ok(())
    }
}

// `diesel_async` provides a blanket `impl<T, Conn> RunQueryDsl<Conn> for T`,
// so no explicit `RunQueryDsl` impl is needed here.
