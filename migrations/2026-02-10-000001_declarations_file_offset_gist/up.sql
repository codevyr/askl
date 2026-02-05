CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE INDEX IF NOT EXISTS declarations_file_offset_gist_idx
    ON index.declarations USING GIST (file_id, offset_range);
