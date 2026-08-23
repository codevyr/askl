-- Restore cluster-default autovacuum behaviour.
--
-- RESET removes the reloption entirely so the table falls back to the GUCs.
-- That is deliberately not the same as writing today's GUC values in: a
-- future cluster-wide tuning change should reach these tables.
--
-- The ANALYZEs in up.sql are intentionally not reverted.  There is no such
-- thing as un-analysing a table, and stale statistics are never the desired
-- state.

ALTER TABLE index.objects RESET (
    autovacuum_analyze_scale_factor,
    autovacuum_analyze_threshold,
    autovacuum_vacuum_scale_factor,
    autovacuum_vacuum_threshold,
    autovacuum_vacuum_insert_scale_factor,
    autovacuum_vacuum_insert_threshold
);

ALTER TABLE index.symbol_refs RESET (
    autovacuum_analyze_scale_factor,
    autovacuum_analyze_threshold,
    autovacuum_vacuum_scale_factor,
    autovacuum_vacuum_threshold,
    autovacuum_vacuum_insert_scale_factor,
    autovacuum_vacuum_insert_threshold
);

ALTER TABLE index.symbol_instances RESET (
    autovacuum_analyze_scale_factor,
    autovacuum_analyze_threshold,
    autovacuum_vacuum_scale_factor,
    autovacuum_vacuum_threshold,
    autovacuum_vacuum_insert_scale_factor,
    autovacuum_vacuum_insert_threshold
);

ALTER TABLE index.symbols RESET (
    autovacuum_analyze_scale_factor,
    autovacuum_analyze_threshold,
    autovacuum_vacuum_scale_factor,
    autovacuum_vacuum_threshold,
    autovacuum_vacuum_insert_scale_factor,
    autovacuum_vacuum_insert_threshold
);
