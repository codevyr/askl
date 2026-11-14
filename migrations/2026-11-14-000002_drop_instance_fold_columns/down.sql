-- Restores the columns, not their contents: every row comes back unfolded at
-- depth 0, which is what an index produced after folding moved to the symbol
-- carried anyway.
ALTER TABLE index.symbol_instances
    ADD COLUMN IF NOT EXISTS folded     BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS fold_depth INTEGER NOT NULL DEFAULT 0;

ALTER TABLE index.symbol_refs
    ADD COLUMN IF NOT EXISTS folded     BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN IF NOT EXISTS fold_depth INTEGER NOT NULL DEFAULT 0;
