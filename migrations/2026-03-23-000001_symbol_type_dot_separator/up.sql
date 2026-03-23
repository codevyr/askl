-- Add dot_is_separator column to symbol_types.
-- When true, dots in symbol names split into separate ltree labels (e.g., kueuectl.main → kueuectl.main as two labels).
-- When false, dots are replaced with underscores within labels (e.g., configuration_types.go → configuration_types_go as one label).
ALTER TABLE index.symbol_types ADD COLUMN dot_is_separator BOOLEAN NOT NULL DEFAULT true;

-- Functions and modules: dot separates module from name (e.g., kueuectl.main)
UPDATE index.symbol_types SET dot_is_separator = true WHERE name IN ('function', 'module');
-- Files and directories: dot is part of the name (e.g., configuration_types.go)
UPDATE index.symbol_types SET dot_is_separator = false WHERE name IN ('file', 'directory');

-- Recreate symbol_name_to_ltree to accept dot_is_separator flag.
-- When dot_is_separator is true: dots split into ltree labels (original behavior).
-- When dot_is_separator is false: dots are replaced with underscores, keeping them in the same label.
CREATE OR REPLACE FUNCTION index.symbol_name_to_ltree(input text, dot_is_sep boolean DEFAULT true)
RETURNS ltree
LANGUAGE sql
IMMUTABLE
AS $$
    WITH stripped AS (
        SELECT regexp_replace(input, E'[\\*\\[\\]\\{\\},@\\- \\(\\)]', '', 'g') AS val
    )
    SELECT COALESCE((
        SELECT array_to_string(array_agg(cleaned ORDER BY ord), '.')
        FROM (
            SELECT ord,
                regexp_replace(token, E'[^A-Za-z0-9_]', '', 'g') AS cleaned
            FROM stripped,
            regexp_split_to_table(
                CASE
                    WHEN dot_is_sep THEN stripped.val
                    ELSE regexp_replace(stripped.val, E'\\.', '_', 'g')
                END,
                CASE
                    WHEN dot_is_sep THEN E'[./:]'
                    ELSE E'[/:]'
                END
            ) WITH ORDINALITY AS t(token, ord)
        ) s
        WHERE cleaned <> ''
    ), 'unknown')::ltree;
$$;

-- Update trigger to use dot_is_separator from symbol_types.
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

-- Recompute all symbol_paths using the per-type flag
UPDATE index.symbols s
SET symbol_path = index.symbol_name_to_ltree(s.name, st.dot_is_separator)
FROM index.symbol_types st
WHERE s.symbol_type = st.id;
