CREATE TABLE IF NOT EXISTS projects
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    project_name TEXT NOT NULL,
    UNIQUE (project_name)
);

CREATE TABLE IF NOT EXISTS modules
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    module_name TEXT NOT NULL,
    project_id INTEGER NOT NULL,
    FOREIGN KEY (project_id) REFERENCES projects(id)
);

CREATE TABLE IF NOT EXISTS files
(
    id INTEGER NOT NULL PRIMARY KEY AUTOINCREMENT,
    module INTEGER NOT NULL,
    module_path TEXT NOT NULL,
    filesystem_path TEXT NOT NULL,
    filetype TEXT NOT NULL,
    content_hash TEXT NOT NULL,
    UNIQUE (module, filesystem_path),
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
    start_offset INTEGER NOT NULL,
    end_offset INTEGER NOT NULL,
    FOREIGN KEY (symbol) REFERENCES symbols(id),
    FOREIGN KEY (file_id) REFERENCES files(id),
    UNIQUE (symbol, file_id, start_offset, end_offset)
);

CREATE TABLE IF NOT EXISTS symbol_refs
(
    to_symbol INTEGER NOT NULL,
    from_file INTEGER NOT NULL,
    from_offset_start INTEGER NOT NULL,
    from_offset_end INTEGER NOT NULL,
    FOREIGN KEY (to_symbol) REFERENCES symbols(id),
    FOREIGN KEY (from_file) REFERENCES files(id),
    UNIQUE (to_symbol, from_file, from_offset_start, from_offset_end)
);

CREATE INDEX IF NOT EXISTS symbol_refs_to_symbol_idx ON symbol_refs(to_symbol);

CREATE INDEX IF NOT EXISTS symbol_refs_ref_loc_idx ON symbol_refs(
    from_file,
    from_offset_start,
    from_offset_end
);

CREATE TABLE IF NOT EXISTS file_contents
(
    file_id INTEGER PRIMARY KEY,
    content BLOB NOT NULL,
    FOREIGN KEY (file_id) REFERENCES files(id)
);
