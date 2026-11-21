//! Statements built from named steps.
//!
//! Few reads here are one `SELECT`.  The joins that matter are interval joins
//! on DFS numbers, which the planner cannot estimate, so a read is written as
//! steps whose order we choose: a small set is computed first, materialised,
//! and the next step starts from it.  Three pieces say that once:
//!
//! * [`Relation`] is a named step: `name AS MATERIALIZED (body)`, optionally
//!   bounded in the rows it may read.
//! * [`InstanceSet`] is the set of instances a fragment is written against.
//!   It has the two bindings there are: ids already held, bound as an array,
//!   or the rows of a relation of the same statement.  A fragment takes an
//!   `InstanceSet` and does not know which it got.
//! * [`With`] is the statement: `WITH <relations> <outer>`.  It also carries
//!   the one protocol bounded relations need, a row that says "a step ran
//!   past its budget, so this is not an answer".
//!
//! Ids are the cheap binding and stay the default: a set that is already
//! resolved needs no step of its own.

use diesel::pg::Pg;
use diesel::query_builder::{AstPass, Query, QueryFragment, QueryId};
use diesel::sql_types::Bool;

use super::mixins::EphSqlFragment;

/// The name of a [`Relation`], as the rest of its statement refers to it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RelationName(String);

impl RelationName {
    /// `<outer> IN (SELECT <columns> FROM <name>)`: the outer row is one of
    /// the relation's.  `outer` and `columns` are a column or a parenthesised
    /// / comma-separated list of the same length.
    pub(crate) fn holds(&self, outer: &str, columns: &str) -> String {
        format!("{outer} IN (SELECT {columns} FROM {})", self.0)
    }

    fn count_sql(&self, cmp: &str, rows: i64) -> String {
        format!("(SELECT count(*) FROM {}) {cmp} {rows}", self.0)
    }
}

impl std::fmt::Display for RelationName {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

/// A named, materialised step of a statement.
///
/// MATERIALIZED always: a relation exists to be computed before what reads
/// it, with a row count the planner can see.  Inlined, the planner is free to
/// drive the join from the other side, which is the failure these steps were
/// introduced for (see `perf/PROBE_BASELINE.md`).
pub(crate) struct Relation {
    name: RelationName,
    body: Box<dyn QueryFragment<Pg> + Send>,
    budget: Option<i64>,
}

impl Relation {
    pub(crate) fn new(
        name: impl Into<String>,
        body: impl QueryFragment<Pg> + Send + 'static,
    ) -> Self {
        Self {
            name: RelationName(name.into()),
            body: Box::new(body),
            budget: None,
        }
    }

    /// Let the relation read at most `budget` rows.
    ///
    /// A materialised step is computed in full before anything after it can
    /// stop it, so an unbounded one makes "too big" the most expensive answer
    /// a statement has.  A bounded one reads `budget + 1` rows; one that read
    /// that many did not see everything, and nothing read through it is an
    /// answer.  The statement says so (see [`With::or_overflow_row`]), and its
    /// outer query should not run at all then (see [`Self::complete_sql`]).
    ///
    /// The bound is on ROWS READ, because rows are what cost.  Which rows a
    /// `LIMIT` without an order returns is arbitrary, but whether there are
    /// more than the budget is not, so the outcome stays deterministic.
    pub(crate) fn bounded(mut self, budget: i64) -> Self {
        self.budget = Some(budget);
        self
    }

    pub(crate) fn name(&self) -> &RelationName {
        &self.name
    }

    /// SQL that holds when the relation was read in full, for the outer
    /// query to guard itself with.  `None` for an unbounded relation, which
    /// always is.
    pub(crate) fn complete_sql(&self) -> Option<String> {
        self.budget.map(|budget| self.name.count_sql("<=", budget))
    }

    fn overflowed_sql(&self) -> Option<String> {
        self.budget.map(|budget| self.name.count_sql(">", budget))
    }
}

/// The instances a fragment is written against.
#[derive(Debug, Clone)]
pub(crate) enum InstanceSet {
    /// Instances already resolved: their ids, bound as one array.
    Ids(Vec<i64>),
    /// The rows of a relation of the same statement, by its `id` column.
    Of(RelationName),
}

impl InstanceSet {
    /// `column` is the id of one of these instances.
    pub(crate) fn holds(&self, column: &str) -> EphSqlFragment<Bool> {
        EphSqlFragment::<Bool>::builder()
            .in_set(column, self)
            .build()
    }
}

/// `(a) UNION (b) UNION …`: the rows of several queries of one shape, each
/// once.  A relation body; the arms are walked as they are.
pub(crate) struct UnionOf {
    arms: Vec<Box<dyn QueryFragment<Pg> + Send>>,
}

impl UnionOf {
    pub(crate) fn new(arms: Vec<Box<dyn QueryFragment<Pg> + Send>>) -> Self {
        Self { arms }
    }
}

impl QueryFragment<Pg> for UnionOf {
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        for (i, arm) in self.arms.iter().enumerate() {
            out.push_sql(if i == 0 { "(" } else { " UNION (" });
            arm.walk_ast(out.reborrow())?;
            out.push_sql(")");
        }
        Ok(())
    }
}

/// `WITH <relations> <outer>`.
///
/// The relations are written in the order given, so one may read those
/// before it.  The outer query is walked as it is: a typed Diesel query keeps
/// loading into its natural row type.
pub(crate) struct With<Outer> {
    relations: Vec<Relation>,
    outer: Outer,
    overflow_row: Option<String>,
}

impl<Outer> With<Outer> {
    pub(crate) fn new(relations: Vec<Relation>, outer: Outer) -> Self {
        Self {
            relations,
            outer,
            overflow_row: None,
        }
    }

    /// Return `row` as well when any bounded relation ran past its budget.
    ///
    /// `row` is a select list of the outer query's type that no real row can
    /// equal; the caller recognises it and reports "not known" instead of an
    /// answer.
    pub(crate) fn or_overflow_row(mut self, row: impl Into<String>) -> Self {
        self.overflow_row = Some(row.into());
        self
    }
}

impl<Outer> QueryId for With<Outer> {
    type QueryId = ();
    const HAS_STATIC_QUERY_ID: bool = false;
}

impl<Outer: Query> Query for With<Outer> {
    type SqlType = Outer::SqlType;
}

impl<Outer: QueryFragment<Pg>> QueryFragment<Pg> for With<Outer> {
    fn walk_ast<'b>(&'b self, mut out: AstPass<'_, 'b, Pg>) -> diesel::QueryResult<()> {
        for (i, relation) in self.relations.iter().enumerate() {
            out.push_sql(if i == 0 { "WITH " } else { ", " });
            out.push_sql(&relation.name.0);
            out.push_sql(" AS MATERIALIZED (");
            relation.body.walk_ast(out.reborrow())?;
            if let Some(budget) = relation.budget {
                out.push_sql(&format!(" LIMIT {}", budget + 1));
            }
            out.push_sql(") ");
        }

        let overflowed: Vec<String> = self
            .relations
            .iter()
            .filter_map(Relation::overflowed_sql)
            .collect();
        match &self.overflow_row {
            Some(row) if !overflowed.is_empty() => {
                out.push_sql("(");
                self.outer.walk_ast(out.reborrow())?;
                out.push_sql(&format!(
                    ") UNION ALL (SELECT {row} WHERE {})",
                    overflowed.join(" OR ")
                ));
            }
            _ => self.outer.walk_ast(out.reborrow())?,
        }
        Ok(())
    }
}

// `diesel_async` provides a blanket `impl<T, Conn> RunQueryDsl<Conn> for T`,
// so no explicit `RunQueryDsl` impl is needed here.

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db_diesel::mixins::OwnedSql;
    use diesel::sql_types::BigInt;

    fn sql(text: &str) -> OwnedSql<BigInt> {
        OwnedSql::new(text.to_string())
    }

    fn rendered<Q: QueryFragment<Pg>>(query: &Q) -> String {
        diesel::debug_query::<Pg, _>(query).to_string()
    }

    #[test]
    fn a_statement_without_relations_is_its_outer_query() {
        let statement = With::new(vec![], sql("SELECT 2")).or_overflow_row("0::bigint");
        assert_eq!(rendered(&statement), "SELECT 2 -- binds: []");
    }

    #[test]
    fn relations_are_materialised_in_the_order_given() {
        let small = Relation::new("small", sql("SELECT 1"));
        let next = Relation::new("next", sql("SELECT id FROM small"));
        let statement = With::new(vec![small, next], sql("SELECT id FROM next"));
        assert_eq!(
            rendered(&statement),
            "WITH small AS MATERIALIZED (SELECT 1) , \
             next AS MATERIALIZED (SELECT id FROM small) \
             SELECT id FROM next -- binds: []"
        );
    }

    #[test]
    fn a_bounded_relation_reads_one_row_past_its_budget_and_reports_it() {
        let role = Relation::new("role", sql("SELECT 1")).bounded(10);
        assert_eq!(
            role.complete_sql().as_deref(),
            Some("(SELECT count(*) FROM role) <= 10")
        );
        let free = Relation::new("free", sql("SELECT 1"));
        assert_eq!(free.complete_sql(), None, "an unbounded relation is whole");

        let statement = With::new(vec![role, free], sql("SELECT 2")).or_overflow_row("0::bigint");
        assert_eq!(
            rendered(&statement),
            "WITH role AS MATERIALIZED (SELECT 1 LIMIT 11) , \
             free AS MATERIALIZED (SELECT 1) \
             (SELECT 2) UNION ALL \
             (SELECT 0::bigint WHERE (SELECT count(*) FROM role) > 10) -- binds: []"
        );
    }

    #[test]
    fn a_statement_that_was_not_asked_reports_no_overflow() {
        let role = Relation::new("role", sql("SELECT 1")).bounded(10);
        let statement = With::new(vec![role], sql("SELECT 2"));
        assert_eq!(
            rendered(&statement),
            "WITH role AS MATERIALIZED (SELECT 1 LIMIT 11) SELECT 2 -- binds: []"
        );
    }

    #[test]
    fn a_union_takes_each_arm_in_parentheses() {
        let one = UnionOf::new(vec![Box::new(sql("SELECT 1"))]);
        assert_eq!(rendered(&one), "(SELECT 1) -- binds: []");
        let two = UnionOf::new(vec![Box::new(sql("SELECT 1")), Box::new(sql("SELECT 2"))]);
        assert_eq!(rendered(&two), "(SELECT 1) UNION (SELECT 2) -- binds: []");
    }

    #[test]
    fn an_instance_set_binds_ids_or_reads_a_relation() {
        let held = InstanceSet::Ids(vec![3, 5]);
        assert_eq!(
            rendered(&held.holds("pd.id")),
            "pd.id = ANY($1) -- binds: [[3, 5]]"
        );

        let step = Relation::new("candidates", sql("SELECT 1"));
        let of_step = InstanceSet::Of(step.name().clone());
        assert_eq!(
            rendered(&of_step.holds("pd.id")),
            "pd.id IN (SELECT id FROM candidates) -- binds: []"
        );
    }

    #[test]
    fn a_relation_name_tests_membership_by_the_columns_asked() {
        let role = Relation::new("role", sql("SELECT 1"));
        assert_eq!(
            role.name().holds("symbols.id", "symbol"),
            "symbols.id IN (SELECT symbol FROM role)"
        );
        assert_eq!(
            role.name()
                .holds("(symbols.id, symbol_instances.id)", "symbol, id"),
            "(symbols.id, symbol_instances.id) IN (SELECT symbol, id FROM role)"
        );
    }
}
