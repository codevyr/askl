CREATE TABLE IF NOT EXISTS files
(
    id INTEGER PRIMARY KEY,
    path TEXT NOT NULL,
    project TEXT NOT NULL,
    filetype TEXT NOT NULL,
    UNIQUE (path, project)
);

CREATE TABLE IF NOT EXISTS symbols
(
    id INTEGER PRIMARY KEY,
    name TEXT NOT NULL,
    module_id INTEGER,
    symbol_scope INTEGER NOT NULL,
    FOREIGN KEY (module_id) REFERENCES files(id),
    UNIQUE (name, module_id, symbol_scope)
);

CREATE TABLE IF NOT EXISTS declarations
(
    symbol INTEGER NOT NULL,
    file_id INTEGER NOT NULL,
    symbol_type INTEGER NOT NULL,
    line_start INTEGER NOT NULL,
    col_start INTEGER NOT NULL,
    line_end INTEGER NOT NULL,
    col_end INTEGER NOT NULL,
    FOREIGN KEY (symbol) REFERENCES symbols(id),
    FOREIGN KEY (file_id) REFERENCES files(id),
    UNIQUE (symbol, file_id, line_start, col_start, line_end, col_end)
);

CREATE TABLE IF NOT EXISTS symbol_refs
(
    from_symbol INTEGER NOT NULL,
    to_symbol INTEGER NOT NULL,
    from_line INTEGER NOT NULL,
    from_col_start INTEGER NOT NULL,
    from_col_end INTEGER NOT NULL,
    FOREIGN KEY (from_symbol) REFERENCES symbols(id),
    FOREIGN KEY (to_symbol) REFERENCES symbols(id),
    UNIQUE (from_symbol, to_symbol, from_line, from_col_start, from_col_end)
);