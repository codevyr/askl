-- Shift all existing levels up by 1 so Field can be inserted at level 1
-- Guard: only shift if field type doesn't exist yet (idempotent)
UPDATE index.symbol_types SET level = level + 1
WHERE NOT EXISTS (SELECT 1 FROM index.symbol_types WHERE id = 8);

-- Insert new Field type at level 1 (below all other leaf types)
INSERT INTO index.symbol_types (id, name, level, dot_is_separator)
VALUES (8, 'field', 1, true)
ON CONFLICT (id) DO NOTHING;
