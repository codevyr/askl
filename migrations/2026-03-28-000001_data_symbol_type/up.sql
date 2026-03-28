INSERT INTO index.symbol_types (id, name, level, dot_is_separator) VALUES
    (6, 'data', 1, true)
ON CONFLICT (id) DO NOTHING;
