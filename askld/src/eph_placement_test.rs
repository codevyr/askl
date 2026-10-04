//! Does every query-time row actually sit where the engine thinks it does?
//!
//! [`crate::index_store::upload`]'s `validate_object_tree` checks the tree an
//! *indexer* uploads.  Nothing checked the rows the engine inserts itself, and
//! that is where this area's bugs have lived: a batch-global slot ordinal that
//! put a match outside its owner, an owner lookup that chose a different
//! ephemeral layer's row, a zero-width range that matched every instance in the
//! file, a folded owner that should not have owned text at all.  Each of those
//! produced a plausible wrong answer rather than an error.
//!
//! So this sweeps the whole fixture database after running real queries, and
//! restates the placement contract as SQL:
//!
//!   * every ephemeral interval is non-empty;
//!   * it has an owner whenever one *exists* — the innermost **unfolded
//!     persistent** instance whose extent contains it.  Having none is
//!     legitimate and common: a symbol-less file still has searchable content,
//!     and a hand-written `ephemeral_instance` may name a range no declaration
//!     covers.  The check is therefore "a container exists but no owner was
//!     recorded", not "no owner";
//!   * when it has one, the row sits strictly inside that owner's interval;
//!   * and its `nesting_level` and `fold_depth` agree with that owner, so the
//!     two axes cannot drift apart.

use crate::test_util::{get_shared_db_url, run_query, TEST_INPUT_SEARCH};
use diesel::prelude::*;
use diesel::sql_types::{BigInt, Integer, Text};

#[derive(QueryableByName, Debug)]
struct Violation {
    #[diesel(sql_type = BigInt)]
    id: i64,
    #[diesel(sql_type = Integer)]
    object_id: i32,
    #[diesel(sql_type = Text)]
    rng: String,
    #[diesel(sql_type = Text)]
    reason: String,
}

/// Recomputes the owner exactly as `insert_instance_level` does, then reports
/// every way a row can be misplaced.  Mirroring the lookup rather than trusting
/// it is the point: if the two ever disagree, that disagreement is the bug.
const SWEEP: &str = "\
WITH eph AS ( \
    SELECT id, object_id, offset_range, nesting_level, dfs_enter, dfs_exit, fold_depth \
    FROM index.symbol_instances WHERE layer < 0 \
), owned AS ( \
    SELECT e.*, \
           o.nesting_level AS o_level, o.dfs_enter AS o_enter, \
           o.dfs_exit AS o_exit, o.fold_depth AS o_fold, o.folded AS o_folded \
    FROM eph e \
    LEFT JOIN LATERAL ( \
        SELECT si.nesting_level, si.dfs_enter, si.dfs_exit, si.fold_depth, si.folded \
        FROM index.symbol_instances si \
        WHERE si.object_id = e.object_id \
          AND si.layer > 0 \
          AND NOT si.folded \
          AND si.offset_range @> e.offset_range \
        ORDER BY upper(si.offset_range) - lower(si.offset_range) ASC, \
                 si.nesting_level DESC, si.dfs_enter ASC LIMIT 1 \
    ) o ON TRUE \
) \
SELECT id, object_id, offset_range::text AS rng, \
       CASE \
         WHEN dfs_exit <= dfs_enter THEN 'empty interval' \
         WHEN o_enter IS NULL THEN 'an instance contains this range, yet no owner was recorded' \
         WHEN NOT (o_enter < dfs_enter AND dfs_exit <= o_exit) THEN 'outside its owner' \
         WHEN nesting_level <> o_level + 1 THEN 'nesting_level disagrees with its owner' \
         WHEN fold_depth <> o_fold + CASE WHEN o_folded THEN 1 ELSE 0 END \
           THEN 'fold_depth disagrees with its owner' \
       END AS reason \
FROM owned \
WHERE dfs_exit <= dfs_enter \
   OR (o_enter IS NOT NULL AND NOT (o_enter < dfs_enter AND dfs_exit <= o_exit)) \
   OR (o_enter IS NOT NULL AND nesting_level <> o_level + 1) \
   OR (o_enter IS NOT NULL \
       AND fold_depth <> o_fold + CASE WHEN o_folded THEN 1 ELSE 0 END) \
   OR (o_enter IS NULL AND EXISTS ( \
        SELECT 1 FROM index.symbol_instances si2 \
        WHERE si2.object_id = owned.object_id AND si2.layer > 0 AND NOT si2.folded \
          AND si2.offset_range @> owned.offset_range)) \
ORDER BY id";

#[test]
fn ephemeral_rows_sit_inside_their_owners() {
    // Run the queries that exercise the interesting shapes first: a batch wider
    // than the slot grid, matches spread over many objects, a co-extensive
    // cluster, nested macros, a macro argument, and a symbol-less file.  Each
    // leaves its layer behind in the shared fixture database, so the sweep then
    // sees every row they placed.
    for q in [
        r#"search("zqx")"#,
        r#"search("foo")"#,
        r#"search("wq9")"#,
        r#"search("k7z")"#,
        r#"search("zq8")"#,
        r#"search("Foo")"#,
        r#"search("shared_token")"#,
        r#"func("fn_many") { search("zqx") }"#,
        r#"{ search("zq8") }"#,
    ] {
        let _ = run_query(TEST_INPUT_SEARCH, q);
    }

    let url = get_shared_db_url(TEST_INPUT_SEARCH);
    let mut conn = PgConnection::establish(url).expect("fixture database");

    let total: i64 = diesel::sql_query(
        "SELECT COUNT(*) AS id, 0::int AS object_id, ''::text AS rng, ''::text AS reason \
         FROM index.symbol_instances WHERE layer < 0",
    )
    .get_result::<Violation>(&mut conn)
    .expect("count ephemeral rows")
    .id;
    assert!(
        total > 0,
        "the sweep is vacuous unless the queries above actually placed rows"
    );

    let violations: Vec<Violation> = diesel::sql_query(SWEEP)
        .get_results(&mut conn)
        .expect("run the placement sweep");

    assert!(
        violations.is_empty(),
        "{} of {} ephemeral rows are misplaced:\n{}",
        violations.len(),
        total,
        violations
            .iter()
            .map(|v| format!(
                "  id={} object={} range={} — {}",
                v.id, v.object_id, v.rng, v.reason
            ))
            .collect::<Vec<_>>()
            .join("\n")
    );
}
