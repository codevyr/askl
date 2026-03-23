-- Restore original symbol_name_to_ltree (single-argument, dots always split)
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

-- Drop the two-argument overload
DROP FUNCTION IF EXISTS index.symbol_name_to_ltree(text, boolean);

-- Restore original trigger
CREATE OR REPLACE FUNCTION index.set_symbol_path()
RETURNS trigger
LANGUAGE plpgsql
AS $$
BEGIN
    NEW.symbol_path := index.symbol_name_to_ltree(NEW.name);
    RETURN NEW;
END;
$$;

-- Remove the column
ALTER TABLE index.symbol_types DROP COLUMN IF EXISTS dot_is_separator;

-- Recompute all symbol_paths
UPDATE index.symbols SET symbol_path = index.symbol_name_to_ltree(name);
