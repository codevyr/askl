-- Create symbol_types table
CREATE TABLE IF NOT EXISTS index.symbol_types (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL UNIQUE,
    level INTEGER NOT NULL UNIQUE
);

INSERT INTO index.symbol_types (id, name, level) VALUES
    (1, 'function', 1),
    (2, 'file', 2),
    (3, 'module', 3),
    (4, 'directory', 4);

-- Drop and recreate symbols with type column and nullable scope
DROP TABLE IF EXISTS index.symbol_refs CASCADE;
DROP TABLE IF EXISTS index.symbol_instances CASCADE;
DROP TABLE IF EXISTS index.symbols CASCADE;

CREATE TABLE IF NOT EXISTS index.symbols (
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    symbol_path LTREE NOT NULL,
    module INTEGER NOT NULL REFERENCES index.modules(id) ON DELETE CASCADE,
    symbol_type INTEGER NOT NULL REFERENCES index.symbol_types(id),
    symbol_scope INTEGER,  -- NULL for non-function types
    UNIQUE (name, module)
);

CREATE INDEX IF NOT EXISTS symbols_name_trgm_idx ON index.symbols USING GIN (name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS symbols_path_idx ON index.symbols USING GIST (symbol_path);
CREATE INDEX IF NOT EXISTS symbols_type_idx ON index.symbols (symbol_type);

-- Recreate symbol_instances WITHOUT symbol_type column
CREATE TABLE IF NOT EXISTS index.symbol_instances (
    id SERIAL PRIMARY KEY,
    symbol INTEGER NOT NULL REFERENCES index.symbols(id) ON DELETE CASCADE,
    object_id INTEGER NOT NULL REFERENCES index.objects(id) ON DELETE CASCADE,
    offset_range INT4RANGE NOT NULL,
    UNIQUE (symbol, object_id, offset_range),
    CHECK (lower(offset_range) < upper(offset_range))
);

CREATE INDEX IF NOT EXISTS symbol_instances_offset_range_idx
    ON index.symbol_instances USING GIST (offset_range);
CREATE INDEX IF NOT EXISTS symbol_instances_object_offset_gist_idx
    ON index.symbol_instances USING GIST (object_id, offset_range);

-- Recreate symbol_refs
CREATE TABLE IF NOT EXISTS index.symbol_refs (
    id SERIAL PRIMARY KEY,
    to_symbol INTEGER NOT NULL REFERENCES index.symbols(id) ON DELETE CASCADE,
    from_object INTEGER NOT NULL REFERENCES index.objects(id) ON DELETE CASCADE,
    from_offset_range INT4RANGE NOT NULL,
    UNIQUE (to_symbol, from_object, from_offset_range)
);

CREATE INDEX IF NOT EXISTS symbol_refs_to_symbol_idx ON index.symbol_refs(to_symbol);
CREATE INDEX IF NOT EXISTS symbol_refs_from_object_idx ON index.symbol_refs(from_object);

-- Recreate symbol_path trigger
CREATE OR REPLACE FUNCTION index.set_symbol_path()
RETURNS trigger LANGUAGE plpgsql AS $$
BEGIN
    NEW.symbol_path := index.symbol_name_to_ltree(NEW.name);
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS symbols_set_path ON index.symbols;
CREATE TRIGGER symbols_set_path
BEFORE INSERT OR UPDATE OF name ON index.symbols
FOR EACH ROW EXECUTE FUNCTION index.set_symbol_path();
