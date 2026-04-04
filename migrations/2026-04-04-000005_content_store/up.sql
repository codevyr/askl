CREATE TABLE IF NOT EXISTS index.content_store (
    content_hash TEXT PRIMARY KEY,
    content BYTEA NOT NULL
);
