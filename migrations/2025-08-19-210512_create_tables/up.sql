-- Your SQL goes here
CREATE TABLE IF NOT EXISTS modules
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    module_name TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS files
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    module INTEGER NOT NULL,
    module_path TEXT NOT NULL,
    filesystem_path TEXT NOT NULL,
    filetype TEXT NOT NULL,
    UNIQUE (filesystem_path),
    FOREIGN KEY (module) REFERENCES modules(id)
);

CREATE TABLE IF NOT EXISTS symbols
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    name TEXT NOT NULL,
    module INTEGER NOT NULL, -- This references is excessive because it is already in declarations
    symbol_scope INTEGER NOT NULL,
    FOREIGN KEY (module) REFERENCES modules(id),
    UNIQUE (name, module)
);

CREATE TABLE IF NOT EXISTS declarations
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
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
    from_decl INTEGER NOT NULL,
    to_symbol INTEGER NOT NULL,
    from_line INTEGER NOT NULL,
    from_col_start INTEGER NOT NULL,
    from_col_end INTEGER NOT NULL,
    FOREIGN KEY (from_decl) REFERENCES declarations(id),
    FOREIGN KEY (to_symbol) REFERENCES symbols(id),
    UNIQUE (from_decl, to_symbol, from_line, from_col_start, from_col_end)
);