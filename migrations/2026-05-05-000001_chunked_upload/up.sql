-- Chunked, resumable upload protocol.
-- Symbols and objects are now uploaded in small sequential chunks; each chunk
-- is recorded here so that interrupted uploads can resume without restarting.

ALTER TABLE index.projects ADD COLUMN symbol_chunks_total INTEGER;
ALTER TABLE index.projects ADD COLUMN object_chunks_total INTEGER;

CREATE TABLE index.project_symbol_chunks (
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    PRIMARY KEY (project_id, seq)
);

CREATE TABLE index.project_object_chunks (
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE,
    seq        INTEGER NOT NULL,
    PRIMARY KEY (project_id, seq)
);
