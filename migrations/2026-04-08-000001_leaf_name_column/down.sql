-- Restore single-column GIST index
CREATE INDEX IF NOT EXISTS symbols_path_idx
    ON index.symbols USING GIST (symbol_path);

-- Drop B-tree leaf_name indices
DROP INDEX IF EXISTS index.symbols_project_type_leafname_idx;
DROP INDEX IF EXISTS index.symbols_project_leafname_idx;

-- Restore trigger without leaf_name
CREATE OR REPLACE FUNCTION index.set_symbol_path()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.symbol_path := index.symbol_name_to_ltree(
        NEW.name,
        COALESCE((SELECT dot_is_separator FROM index.symbol_types WHERE id = NEW.symbol_type), true)
    );
    RETURN NEW;
END;
$$;

-- Drop column
ALTER TABLE index.symbols DROP COLUMN leaf_name;
