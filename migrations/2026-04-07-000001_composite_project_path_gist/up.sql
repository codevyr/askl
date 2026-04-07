CREATE INDEX IF NOT EXISTS symbols_project_path_gist_idx
    ON index.symbols USING GIST (project_id, symbol_path);
