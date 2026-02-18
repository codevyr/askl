CREATE SCHEMA IF NOT EXISTS index;

CREATE EXTENSION IF NOT EXISTS pg_trgm;
CREATE EXTENSION IF NOT EXISTS ltree;

CREATE TABLE IF NOT EXISTS index.projects
(
    id SERIAL PRIMARY KEY,
    project_name TEXT NOT NULL UNIQUE,
    root_path TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS index.modules
(
    id SERIAL PRIMARY KEY,
    module_name TEXT NOT NULL,
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE
);

CREATE TABLE IF NOT EXISTS index.files
(
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE,
    module INTEGER REFERENCES index.modules(id) ON DELETE CASCADE,
    module_path TEXT NOT NULL,
    filesystem_path TEXT NOT NULL,
    filetype TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    UNIQUE (project_id, module, filesystem_path)
);

CREATE UNIQUE INDEX IF NOT EXISTS files_project_path_no_module_idx
    ON index.files (project_id, filesystem_path)
    WHERE module IS NULL;

CREATE TABLE IF NOT EXISTS index.symbols
(
    id SERIAL PRIMARY KEY,
    name TEXT NOT NULL,
    symbol_path LTREE NOT NULL,
    module INTEGER NOT NULL REFERENCES index.modules(id) ON DELETE CASCADE,
    symbol_scope INTEGER NOT NULL,
    UNIQUE (name, module)
);

CREATE INDEX IF NOT EXISTS symbols_name_trgm_idx ON index.symbols USING GIN (name gin_trgm_ops);
CREATE INDEX IF NOT EXISTS symbols_path_idx ON index.symbols USING GIST (symbol_path);

CREATE OR REPLACE FUNCTION index.symbol_name_to_ltree(input text)
RETURNS ltree
LANGUAGE sql
IMMUTABLE
AS $$
    SELECT COALESCE((
        SELECT array_to_string(array_agg(cleaned ORDER BY ord), '.')
        FROM (
            SELECT ord,
                regexp_replace(token, E'[^A-Za-z0-9_]', '', 'g') AS cleaned
            FROM regexp_split_to_table(
                regexp_replace(input, E'[\\*\\[\\]\\{\\},@\\- \\(\\)]', '', 'g'),
                E'[./:]'
            ) WITH ORDINALITY AS t(token, ord)
        ) s
        WHERE cleaned <> ''
    ), 'unknown')::ltree;
$$;

CREATE OR REPLACE FUNCTION index.set_symbol_path()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.symbol_path := index.symbol_name_to_ltree(NEW.name);
    RETURN NEW;
END;
$$;

DROP TRIGGER IF EXISTS symbols_set_path ON index.symbols;
CREATE TRIGGER symbols_set_path
BEFORE INSERT OR UPDATE OF name ON index.symbols
FOR EACH ROW
EXECUTE FUNCTION index.set_symbol_path();

CREATE TABLE IF NOT EXISTS index.declarations
(
    id SERIAL PRIMARY KEY,
    symbol INTEGER NOT NULL REFERENCES index.symbols(id) ON DELETE CASCADE,
    file_id INTEGER NOT NULL REFERENCES index.files(id) ON DELETE CASCADE,
    symbol_type INTEGER NOT NULL,
    offset_range INT4RANGE NOT NULL,
    UNIQUE (symbol, file_id, offset_range)
);

CREATE TABLE IF NOT EXISTS index.symbol_refs
(
    id SERIAL PRIMARY KEY,
    to_symbol INTEGER NOT NULL REFERENCES index.symbols(id) ON DELETE CASCADE,
    from_file INTEGER NOT NULL REFERENCES index.files(id) ON DELETE CASCADE,
    from_offset_range INT4RANGE NOT NULL,
    UNIQUE (to_symbol, from_file, from_offset_range)
);

CREATE INDEX IF NOT EXISTS symbol_refs_to_symbol_idx ON index.symbol_refs(to_symbol);

CREATE INDEX IF NOT EXISTS declarations_offset_range_idx
    ON index.declarations USING GIST (offset_range);

CREATE INDEX IF NOT EXISTS symbol_refs_from_file_idx
    ON index.symbol_refs(from_file);

CREATE INDEX IF NOT EXISTS symbol_refs_from_offset_range_idx
    ON index.symbol_refs USING GIST (from_offset_range);

CREATE TABLE IF NOT EXISTS index.file_contents
(
    file_id INTEGER PRIMARY KEY REFERENCES index.files(id) ON DELETE CASCADE,
    content BYTEA NOT NULL
);
