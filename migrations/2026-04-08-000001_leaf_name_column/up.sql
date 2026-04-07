-- 1. Add column (nullable for backfill)
ALTER TABLE index.symbols ADD COLUMN leaf_name TEXT;

-- 2. Backfill from existing symbol_path
UPDATE index.symbols
SET leaf_name = subpath(symbol_path, nlevel(symbol_path) - 1)::text;

-- 3. Make NOT NULL
ALTER TABLE index.symbols ALTER COLUMN leaf_name SET NOT NULL;

-- 4. B-tree indices for fast leaf lookups
-- Typed queries (TypeSelector): symbol_type early for selectivity
CREATE INDEX symbols_project_type_leafname_idx
    ON index.symbols (project_id, symbol_type, leaf_name);
-- Untyped queries (NameSelector): no symbol_type, seek on (project_id, leaf_name)
CREATE INDEX symbols_project_leafname_idx
    ON index.symbols (project_id, leaf_name);

-- 5. Update trigger to compute leaf_name on INSERT/UPDATE
CREATE OR REPLACE FUNCTION index.set_symbol_path()
RETURNS trigger
LANGUAGE plpgsql
AS $$
DECLARE
    computed_path ltree;
BEGIN
    computed_path := index.symbol_name_to_ltree(
        NEW.name,
        COALESCE((SELECT dot_is_separator FROM index.symbol_types WHERE id = NEW.symbol_type), true)
    );
    NEW.symbol_path := computed_path;
    NEW.leaf_name := subpath(computed_path, nlevel(computed_path) - 1)::text;
    RETURN NEW;
END;
$$;

-- 6. Drop redundant single-column GIST index — the composite GIST
-- (project_id, symbol_path) from 2026-04-07 supersedes it
DROP INDEX IF EXISTS index.symbols_path_idx;

-- 7. Update planner statistics after bulk backfill + new indices
ANALYZE index.symbols;
