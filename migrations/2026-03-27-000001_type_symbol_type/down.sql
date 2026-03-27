DELETE FROM index.symbol_types WHERE id = 5;
ALTER TABLE index.symbol_types ADD CONSTRAINT symbol_types_level_key UNIQUE (level);
