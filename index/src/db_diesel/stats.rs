//! Postgres planner-statistics maintenance.
//!
//! # Why this exists
//!
//! `index.symbol_instances.layer` once carried statistics saying
//! `n_distinct = 2` — captured when the deployment held two projects.  Three
//! more were indexed afterwards and nothing ever re-analysed, so a query
//! scoping to a newer project's layer was estimated at **1 row against
//! 287,754 actual**.  The planner picked nested loops, a containment
//! self-join became ~8e10 comparisons, and a 0.4 ms query timed out at 120 s.
//! A plain `ANALYZE` — 2.5 s for the whole database — took the perf corpus
//! from 95.3 s to 21.6 s.
//!
//! Autovacuum could not save it: its analyze threshold is
//! `50 + 0.1 * reltuples`, i.e. ~1.5M modifications on a 15M-row table, and
//! an import moves far fewer rows than that per project.  So the refresh has
//! to be driven from the events that change the distribution — a project
//! finishing or being deleted — plus a boot-time backstop.
//!
//! # Two things that look like evidence and are not
//!
//! * **`pg_stat_user_tables` is not durable.**  `n_live_tup`,
//!   `n_mod_since_analyze`, `last_analyze` and friends live in shared memory
//!   and are discarded by crash recovery.  `last_analyze IS NULL` therefore
//!   does *not* mean ANALYZE never ran, and staleness detection must not be
//!   built on those columns.  `pg_statistic` and `pg_class.reltuples` are
//!   ordinary catalog data and survive.
//! * **ANALYZE without ownership does not fail.**  Postgres 16 answers with a
//!   `WARNING` and success; diesel cannot see warnings.  A non-superuser
//!   deployment would silently never refresh anything.  The only reliable
//!   detector is to re-read the catalog afterwards, which is what
//!   [`ensure_planner_stats`] does.

use diesel::sql_types::{BigInt, Float, Integer, Text};
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use std::time::{Duration, Instant};

/// Bounded wait for the ShareUpdateExclusive lock ANALYZE needs.
///
/// A *waiting* ANALYZE parks at the head of the lock queue, and every later
/// RowExclusive request — i.e. every ephemeral-layer creation, i.e. every
/// cold query — queues behind it even though those never conflict with
/// ANALYZE itself.  An unbounded wait turns maintenance into an outage.
pub const ANALYZE_LOCK_TIMEOUT: &str = "5s";

/// Per-table server-side bound.  The upload pool deliberately sets no
/// `statement_timeout`, so without this an ANALYZE that detoasts a large
/// `bytea` column is unbounded.
pub const ANALYZE_STATEMENT_TIMEOUT: &str = "120s";

/// Tables smaller than this are never judged stale: at 256 KiB the size
/// comparison below is noise, and being wrong about their statistics is
/// cheap.
pub const STALE_MIN_PAGES: i64 = 32;

/// How far the statistics-recorded size may drift from the on-disk size
/// before the statistics are considered untrustworthy.
pub const STALE_DRIFT_FACTOR: i64 = 8;

/// Why a table's statistics look untrustworthy.
///
/// Deliberately size-based: `pg_class` survives crash recovery, unlike the
/// `pg_stat_*` counters.  The limitation is stated in [`ensure_planner_stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatsStaleness {
    /// `reltuples < 0` — Postgres' "never analysed or vacuumed" sentinel.
    NeverAnalyzed,
    /// The table occupies real pages but the statistics claim it is empty.
    NoStats,
    /// The recorded size and the on-disk size disagree by
    /// [`STALE_DRIFT_FACTOR`]x or more, in either direction.
    SizeDrift,
}

/// One table's recorded statistics next to its true size.
#[derive(Debug, Clone, diesel::QueryableByName)]
pub struct TableStatsHealth {
    /// Schema-qualified and quoted by Postgres itself, so it is safe to
    /// interpolate into `ANALYZE`.
    #[diesel(sql_type = Text)]
    pub qualified_name: String,
    #[diesel(sql_type = Text)]
    pub relname: String,
    /// `pg_class.reltuples` is `float4`; negative means "unknown".
    #[diesel(sql_type = Float)]
    pub reltuples: f32,
    #[diesel(sql_type = Integer)]
    pub relpages: i32,
    #[diesel(sql_type = BigInt)]
    pub actual_pages: i64,
}

impl TableStatsHealth {
    /// Classify this table, or `None` when its statistics look plausible.
    ///
    /// A partitioned parent has no storage of its own, so it reports zero
    /// pages and falls out at the size floor below: the heuristic can never
    /// judge one.  That is a reason to prefer [`BootAnalyze::Force`], not a
    /// reason to special-case it here — a parent's statistics are only ever
    /// as good as its last explicit ANALYZE.
    pub fn staleness(&self) -> Option<StatsStaleness> {
        if self.actual_pages < STALE_MIN_PAGES {
            return None;
        }
        if self.reltuples < 0.0 {
            return Some(StatsStaleness::NeverAnalyzed);
        }
        if self.relpages == 0 {
            return Some(StatsStaleness::NoStats);
        }
        let recorded = self.relpages as i64;
        // Drift in either direction: a table that grew 8x since its last
        // ANALYZE, or one whose rows were deleted and whose statistics still
        // describe the old size.  A pure density test (few rows over many
        // pages) is deliberately NOT used — a legitimately bloated table
        // would trip it on every boot forever, and ANALYZE cannot fix bloat.
        if self.actual_pages > recorded.saturating_mul(STALE_DRIFT_FACTOR)
            || recorded > self.actual_pages.saturating_mul(STALE_DRIFT_FACTOR)
        {
            return Some(StatsStaleness::SizeDrift);
        }
        None
    }
}

/// Outcome of one [`analyze_index_schema`] run.  Per-table failures are data,
/// not errors: the caller has usually already committed a mutation.
#[derive(Debug, Default)]
pub struct AnalyzeReport {
    pub analyzed: Vec<(String, Duration)>,
    pub failed: Vec<(String, String)>,
    pub elapsed: Duration,
}

impl AnalyzeReport {
    /// True when nothing at all succeeded on a non-empty schema — the shape
    /// of a permissions or connectivity problem rather than a lock hiccup.
    pub fn wholly_failed(&self) -> bool {
        self.analyzed.is_empty() && !self.failed.is_empty()
    }
}

/// Boot-time policy for [`ensure_planner_stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAnalyze {
    /// Do nothing.
    Off,
    /// Analyse only tables whose catalog entries look implausible.
    Auto,
    /// Always analyse.  The default, because the size heuristic cannot see
    /// *distribution* staleness — the failure that caused the incident.
    Force,
}

/// One table name, from the catalog alone.
#[derive(Debug, Clone, diesel::QueryableByName)]
struct TableNameRow {
    #[diesel(sql_type = Text)]
    qualified_name: String,
}

/// Every heap table in schema `index`, quoted by Postgres itself.
///
/// The list comes from the catalog rather than from a constant, because a
/// hand-written list is exactly what went wrong during the manual recovery:
/// it omitted `content_store`, the table feeding `search()`.  New tables are
/// covered the day they are created, and there is nothing to escape.
///
/// **Touches only catalog rows, and that is deliberate**: it takes no lock on
/// the tables it names, so enumeration cannot block behind DDL.  Anything
/// that reports a table's true size must go through
/// [`index_table_stats_health`], which is lock-bounded.
///
/// Includes partitioned parents (`relkind = 'p'`) as well as ordinary heaps.
/// Nothing is partitioned today, but growing the corpus by an order of
/// magnitude — a layer per release tag, say — makes partitioning the obvious
/// move, and a parent needs its own ANALYZE for inheritance statistics.
/// Filtering to `'r'` would silently stop covering exactly the tables that
/// had outgrown a single heap.  The cost is real: analysing a parent scans
/// its partitions.
pub async fn index_table_names(
    conn: &mut AsyncPgConnection,
) -> Result<Vec<String>, diesel::result::Error> {
    let rows: Vec<TableNameRow> = diesel::sql_query(
        "SELECT quote_ident(n.nspname) || '.' || quote_ident(c.relname) AS qualified_name \
           FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
          WHERE n.nspname = 'index' AND c.relkind IN ('r', 'p') \
          ORDER BY c.relname",
    )
    .load(conn)
    .await?;
    Ok(rows.into_iter().map(|r| r.qualified_name).collect())
}

/// Every heap table with its recorded statistics next to its true size.
///
/// `pg_relation_size()` opens each relation with an `AccessShareLock`, so
/// this query — unlike [`index_table_names`] — blocks behind an
/// `ACCESS EXCLUSIVE` lock.  It is therefore wrapped in its own transaction
/// with [`ANALYZE_LOCK_TIMEOUT`]: a health check that hangs is worse than one
/// that declines to answer.  A lock timeout surfaces as `Err`; callers log
/// and carry on.
pub async fn index_table_stats_health(
    conn: &mut AsyncPgConnection,
) -> Result<Vec<TableStatsHealth>, diesel::result::Error> {
    conn.transaction::<_, diesel::result::Error, _>(async move |conn| {
        diesel::sql_query(format!(
            "SET LOCAL lock_timeout = '{}'",
            ANALYZE_LOCK_TIMEOUT
        ))
        .execute(&mut *conn)
        .await?;
        diesel::sql_query(
            "SELECT quote_ident(n.nspname) || '.' || quote_ident(c.relname) AS qualified_name, \
                    c.relname::text AS relname, \
                    c.reltuples AS reltuples, \
                    c.relpages AS relpages, \
                    (pg_relation_size(c.oid) / current_setting('block_size')::bigint) \
                        AS actual_pages \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
              WHERE n.nspname = 'index' AND c.relkind IN ('r', 'p') \
              ORDER BY c.relname",
        )
        .load::<TableStatsHealth>(&mut *conn)
        .await
    })
    .await
}

/// `ANALYZE` every table in schema `index`, one transaction per table.
///
/// Per-table, never `ANALYZE a, b, c` and never one wrapping transaction: an
/// explicit transaction holds every ShareUpdateExclusive lock until COMMIT,
/// which blocks `purge_eph_cache`'s `LOCK TABLE index.layers IN EXCLUSIVE
/// MODE` and so stalls every concurrent finalize and delete.  Per-table also
/// means a lock timeout costs one table instead of the whole run.
///
/// Only a failure to enumerate the tables is an `Err`; individual tables that
/// fail land in [`AnalyzeReport::failed`].
pub async fn analyze_index_schema(
    conn: &mut AsyncPgConnection,
) -> Result<AnalyzeReport, diesel::result::Error> {
    // Enumerate without taking table locks: the mutation hooks call this
    // right after committing an import, and blocking there behind unrelated
    // DDL would stall the upload response indefinitely.
    let tables = index_table_names(conn).await?;
    analyze_tables(conn, tables).await
}

async fn analyze_tables(
    conn: &mut AsyncPgConnection,
    tables: impl IntoIterator<Item = String>,
) -> Result<AnalyzeReport, diesel::result::Error> {
    let started = Instant::now();
    let mut report = AnalyzeReport::default();

    for table in tables {
        let table_for_sql = table.clone();
        let table_started = Instant::now();
        let outcome = conn
            .transaction::<_, diesel::result::Error, _>(async move |conn| {
                // SET LOCAL, not SET: the pool recycles connections with
                // ROLLBACK, which does not reset session-level settings, so a
                // plain SET leaking from a cancelled future would silently
                // apply to every later user of that pooled connection.
                diesel::sql_query(format!(
                    "SET LOCAL lock_timeout = '{}'",
                    ANALYZE_LOCK_TIMEOUT
                ))
                .execute(&mut *conn)
                .await?;
                diesel::sql_query(format!(
                    "SET LOCAL statement_timeout = '{}'",
                    ANALYZE_STATEMENT_TIMEOUT
                ))
                .execute(&mut *conn)
                .await?;
                diesel::sql_query(format!("ANALYZE {}", table_for_sql))
                    .execute(&mut *conn)
                    .await
            })
            .await;

        match outcome {
            Ok(_) => report.analyzed.push((table, table_started.elapsed())),
            Err(e) => report.failed.push((table, e.to_string())),
        }
    }

    report.elapsed = started.elapsed();
    Ok(report)
}

/// Check, maybe analyse, then verify — the boot-time entry point.
///
/// Infallible by construction: stale statistics are a performance problem,
/// not a correctness one, so a server must start regardless.  Returning `()`
/// makes that a type-level guarantee rather than a call-site convention.
///
/// The verification pass at the end is not belt-and-braces: it is the only
/// way to notice that ANALYZE silently did nothing (see the module docs on
/// ownership), and it also catches lock and statement timeouts.
///
/// **Known limitation.** The size heuristic cannot detect statistics that are
/// the right *shape* and the wrong *distribution* — which is precisely what
/// caused the incident this module exists for.  That is why [`BootAnalyze`]
/// defaults to [`BootAnalyze::Force`]; `Auto` is a boot-latency optimisation,
/// not a safety net.
pub async fn ensure_planner_stats(conn: &mut AsyncPgConnection, mode: BootAnalyze) {
    if mode == BootAnalyze::Off {
        tracing::debug!("planner statistics: check disabled");
        return;
    }

    // A health read can legitimately fail (it takes AccessShareLock on every
    // table and gives up after ANALYZE_LOCK_TIMEOUT).  Under `Force` that
    // must not cancel the refresh — the whole point of `Force` is not to
    // depend on the diagnosis.
    let before = match index_table_stats_health(conn).await {
        Ok(rows) => rows,
        Err(e) => {
            tracing::warn!(error = %e, "planner statistics: health check failed");
            if mode == BootAnalyze::Auto {
                return;
            }
            Vec::new()
        }
    };

    let stale: Vec<&TableStatsHealth> = before.iter().filter(|t| t.staleness().is_some()).collect();

    for t in &stale {
        tracing::warn!(
            table = %t.relname,
            reason = ?t.staleness(),
            reltuples = t.reltuples,
            relpages = t.relpages,
            actual_pages = t.actual_pages,
            "planner statistics look stale",
        );
    }

    let targets: Vec<String> = match mode {
        BootAnalyze::Force => match index_table_names(conn).await {
            Ok(names) => names,
            Err(e) => {
                tracing::warn!(error = %e, "planner statistics: could not enumerate tables");
                return;
            }
        },
        BootAnalyze::Auto => stale.iter().map(|t| t.qualified_name.clone()).collect(),
        BootAnalyze::Off => unreachable!("handled above"),
    };

    if targets.is_empty() {
        tracing::info!(
            tables = before.len(),
            "planner statistics: no refresh needed"
        );
        return;
    }

    let report = match analyze_tables(conn, targets).await {
        Ok(r) => r,
        Err(e) => {
            tracing::warn!(error = %e, "planner statistics: refresh failed");
            return;
        }
    };
    log_report(&report, "boot");

    // Verify.  A table still stale here means ANALYZE did not actually run —
    // most likely because this role does not own it.
    match index_table_stats_health(conn).await {
        Ok(after) => {
            for t in after.iter().filter(|t| t.staleness().is_some()) {
                tracing::error!(
                    table = %t.relname,
                    reason = ?t.staleness(),
                    reltuples = t.reltuples,
                    relpages = t.relpages,
                    actual_pages = t.actual_pages,
                    "planner statistics still stale after ANALYZE — does this role own the table?",
                );
            }
        }
        Err(e) => tracing::warn!(error = %e, "planner statistics: verification read failed"),
    }
}

/// Shared logging for a completed run.  `context` names the trigger so the
/// boot pass and the post-import passes are distinguishable in a log.
pub fn log_report(report: &AnalyzeReport, context: &str) {
    for (table, err) in &report.failed {
        tracing::warn!(context, table = %table, error = %err, "ANALYZE failed");
    }
    if report.wholly_failed() {
        tracing::error!(
            context,
            tables = report.failed.len(),
            "planner statistics: every ANALYZE failed",
        );
    } else {
        let slowest = report
            .analyzed
            .iter()
            .max_by_key(|(_, d)| *d)
            .map(|(t, d)| format!("{} {}ms", t, d.as_millis()))
            .unwrap_or_default();
        tracing::info!(
            context,
            tables = report.analyzed.len(),
            failed = report.failed.len(),
            elapsed_ms = report.elapsed.as_millis() as u64,
            slowest = %slowest,
            "planner statistics refreshed",
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn health(reltuples: f32, relpages: i32, actual_pages: i64) -> TableStatsHealth {
        TableStatsHealth {
            qualified_name: "index.t".into(),
            relname: "t".into(),
            reltuples,
            relpages,
            actual_pages,
        }
    }

    #[test]
    fn small_tables_are_never_stale() {
        // Below the page floor nothing is judged, not even the sentinel.
        assert_eq!(health(-1.0, 0, STALE_MIN_PAGES - 1).staleness(), None);
    }

    #[test]
    fn never_analyzed_sentinel_is_flagged() {
        assert_eq!(
            health(-1.0, 0, 1000).staleness(),
            Some(StatsStaleness::NeverAnalyzed)
        );
    }

    #[test]
    fn sized_table_with_zero_pages_recorded_is_flagged() {
        assert_eq!(
            health(0.0, 0, 1000).staleness(),
            Some(StatsStaleness::NoStats)
        );
    }

    #[test]
    fn growth_and_shrinkage_both_count_as_drift() {
        // Grew 250x since the last ANALYZE (the incident's shape).
        assert_eq!(
            health(278.0, 4, 1000).staleness(),
            Some(StatsStaleness::SizeDrift)
        );
        // Recorded far larger than reality: a mass delete, statistics stale.
        assert_eq!(
            health(1e6, 1000, 100).staleness(),
            Some(StatsStaleness::SizeDrift)
        );
    }

    #[test]
    fn plausible_statistics_pass() {
        assert_eq!(health(1e6, 1000, 1000).staleness(), None);
        // Drift below the factor is tolerated — ANALYZE churn is not free.
        assert_eq!(health(1e6, 1000, 4000).staleness(), None);
    }

    #[test]
    fn wholly_failed_needs_a_failure_and_no_success() {
        let mut r = AnalyzeReport::default();
        assert!(!r.wholly_failed(), "an empty schema is not a failure");
        r.failed.push(("index.t".into(), "boom".into()));
        assert!(r.wholly_failed());
        r.analyzed
            .push(("index.u".into(), Duration::from_millis(1)));
        assert!(!r.wholly_failed());
    }
}
