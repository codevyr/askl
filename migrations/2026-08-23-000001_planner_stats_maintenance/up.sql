-- Planner-statistics maintenance: make autovacuum notice bulk imports.
--
-- THE INCIDENT
--
-- index.symbol_instances.layer carried statistics saying n_distinct = 2 --
-- captured when the deployment held two projects.  Three more were indexed
-- afterwards and nothing re-analysed, so a query scoping to a newer project's
-- layer was estimated at 1 row against 287,754 actual.  The planner chose
-- nested loops, a containment self-join became ~8e10 comparisons, and a
-- 0.4 ms query timed out at 120 s.  A plain ANALYZE (2.5 s for the whole
-- database) took the perf corpus from 95.3 s to 21.6 s.
--
-- Autovacuum never intervened because its analyze threshold is
--   autovacuum_analyze_threshold + autovacuum_analyze_scale_factor * reltuples
-- = 50 + 0.1 * 14.8M ~= 1.5M modifications.  An import moves far fewer rows
-- than that per project, so on a big table the default scale factor is
-- effectively "never".
--
-- WHAT THIS MIGRATION IS AND IS NOT
--
-- It is defence in depth.  The load-bearing mechanisms are in the
-- application: the post-commit refresh in IndexStore::finalize_project and
-- IndexStore::delete_project, and the boot-time pass in askld
-- (index::db_diesel::stats).  Two honest limitations:
--
--   * These reloptions do nothing at all unless autovacuum is running.
--     Verify with:
--       SELECT name, setting, source FROM pg_settings
--        WHERE name IN ('autovacuum', 'track_counts');
--     (Checked on the reference deployment at the time of writing: both on,
--     with a worker active.)
--
--   * They do NOT self-heal a wiped counter on a quiescent table.  Since
--     PG15 the cumulative counters (n_mod_since_analyze and friends) live in
--     shared memory and are DISCARDED by crash recovery; if a table then sees
--     no further writes, n_mod_since_analyze restarts at 0 and never reaches
--     any threshold, absolute or not.  The application hooks cover that case.
--     (Planner statistics themselves -- pg_statistic, pg_class.reltuples --
--     are ordinary catalog data and survive crashes.)
--
-- RULE FOR FUTURE TABLES: give a table its own autovacuum reloptions once it
-- routinely exceeds ~1M rows.  Below that the default scale factors already
-- fire often enough, and replacing them with absolute thresholds would make a
-- SMALL table analyse LESS, not more.

-- 1. The four multi-million-row tables.
--
--    Tuned to stay sane across two orders of magnitude, because the staging
--    corpus (~15M rows) is not the ceiling -- a layer per release tag puts
--    production nearer 1.5B:
--
--      analyze  = 25000 + 0.2%  ->    55k mods at 15M,   ~3M at 1.5B
--      vacuum   = 25000 + 1%    ->   175k mods at 15M,  ~15M at 1.5B
--
--    The defaults (50 + 10%) would be ~1.5M and ~150M respectively, i.e.
--    effectively never on the big tables.  A pure absolute threshold was
--    rejected in the other direction: at 1.5B rows it would re-analyse on
--    every few minutes of ephemeral churn.
--
--    Do not mistake this for import coverage.  A per-tag import might add
--    5M rows -- 0.3% of a 1.5B-row table -- and still not reach any of these
--    thresholds, while introducing a brand new `layer` value that the
--    planner has never seen.  That case is precisely the incident, and it is
--    covered by the post-commit hooks in the application, not here.  What
--    this section buys is bounded drift between those events.
ALTER TABLE index.symbols SET (
    autovacuum_analyze_scale_factor       = 0.002,
    autovacuum_analyze_threshold          = 25000,
    autovacuum_vacuum_scale_factor        = 0.01,
    autovacuum_vacuum_threshold           = 25000,
    autovacuum_vacuum_insert_scale_factor = 0.01,
    autovacuum_vacuum_insert_threshold    = 25000
);

ALTER TABLE index.symbol_instances SET (
    autovacuum_analyze_scale_factor       = 0.002,
    autovacuum_analyze_threshold          = 25000,
    autovacuum_vacuum_scale_factor        = 0.01,
    autovacuum_vacuum_threshold           = 25000,
    autovacuum_vacuum_insert_scale_factor = 0.01,
    autovacuum_vacuum_insert_threshold    = 25000
);

ALTER TABLE index.symbol_refs SET (
    autovacuum_analyze_scale_factor       = 0.002,
    autovacuum_analyze_threshold          = 25000,
    autovacuum_vacuum_scale_factor        = 0.01,
    autovacuum_vacuum_threshold           = 25000,
    autovacuum_vacuum_insert_scale_factor = 0.01,
    autovacuum_vacuum_insert_threshold    = 25000
);

ALTER TABLE index.objects SET (
    autovacuum_analyze_scale_factor       = 0.002,
    autovacuum_analyze_threshold          = 25000,
    autovacuum_vacuum_scale_factor        = 0.01,
    autovacuum_vacuum_threshold           = 25000,
    autovacuum_vacuum_insert_scale_factor = 0.01,
    autovacuum_vacuum_insert_threshold    = 25000
);

-- 2. Bring existing deployments current at deploy time rather than waiting
--    for the next boot or import.  ANALYZE is transaction-legal (VACUUM is
--    not), and diesel runs each migration in a transaction -- precedent:
--    2026-07-27-000001_root_layers/up.sql.  On a fresh database these are
--    no-ops on empty tables.
--
--    Every table, and derived from the catalog rather than written out --
--    the same rule the runtime helper follows.  The manual recovery that
--    preceded this migration hand-picked four tables and missed
--    content_store, which feeds search() and was the slowest thing in the
--    corpus afterwards.  A literal list here would also break the moment the
--    schema drifts: the first draft of this file named a table that does not
--    exist yet at this point in the migration history.
DO $$
DECLARE
    t record;
BEGIN
    FOR t IN
        SELECT quote_ident(n.nspname) || '.' || quote_ident(c.relname) AS qname
          FROM pg_class c
          JOIN pg_namespace n ON n.oid = c.relnamespace
         WHERE n.nspname = 'index' AND c.relkind IN ('r', 'p')
         ORDER BY c.relname
    LOOP
        EXECUTE 'ANALYZE ' || t.qname;
    END LOOP;
END
$$;

-- 3. OPERATIONAL, AND DELIBERATELY NOT AUTOMATED HERE.
--
--    VACUUM cannot run inside a transaction block, so it cannot live in a
--    diesel migration (same constraint as the VACUUM FULL note in
--    2026-06-19-000002_content_store_lz4).  If relallvisible is 0 on a large
--    table, no VACUUM has ever run there: the visibility map is empty, so
--    index-only scans degrade to heap fetches.  On the reference deployment
--    that was true of index.content_store.
--
--    Check:
--      SELECT relname, relallvisible, relpages, age(relfrozenxid)
--        FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace
--       WHERE n.nspname = 'index' AND c.relkind = 'r';
--
--    Then, once, in a maintenance window (takes no exclusive lock;
--    concurrent DML is unaffected):
--      VACUUM (ANALYZE, VERBOSE) index.content_store;
--      VACUUM (ANALYZE, VERBOSE) index.symbols;
--      VACUUM (ANALYZE, VERBOSE) index.symbol_instances;
--      VACUUM (ANALYZE, VERBOSE) index.symbol_refs;
--      VACUUM (ANALYZE, VERBOSE) index.objects;
