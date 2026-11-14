-- The per-instance and per-reference fold columns go.
--
-- They held a flag and a count of folded ancestors, and the default traversal
-- compared the counts.  Folding is the symbol's now (`index.symbols.folded`,
-- 2026-11-14-000001_symbol_folded): nothing reads these, and since that
-- change nothing writes them either.
--
-- DROP COLUMN only marks the column dropped in the catalog; it does not
-- rewrite the table.  No index, constraint or view names any of the four.
ALTER TABLE index.symbol_instances
    DROP COLUMN IF EXISTS fold_depth,
    DROP COLUMN IF EXISTS folded;

ALTER TABLE index.symbol_refs
    DROP COLUMN IF EXISTS fold_depth,
    DROP COLUMN IF EXISTS folded;
