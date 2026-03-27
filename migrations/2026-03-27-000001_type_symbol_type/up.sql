-- Drop UNIQUE constraint on level to allow TYPE and FUNCTION to share level=1.
-- The containment query uses >= (not =), so same-level symbols can contain each
-- other (this is how nested functions already work).
ALTER TABLE index.symbol_types DROP CONSTRAINT IF EXISTS symbol_types_level_key;

INSERT INTO index.symbol_types (id, name, level, dot_is_separator) VALUES
    (5, 'type', 1, true)
ON CONFLICT (id) DO NOTHING;
