CREATE EXTENSION IF NOT EXISTS ltree;

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

UPDATE index.symbols
SET symbol_path = index.symbol_name_to_ltree(name);
