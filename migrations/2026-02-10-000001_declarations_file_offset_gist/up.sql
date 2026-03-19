CREATE EXTENSION IF NOT EXISTS btree_gist;

CREATE INDEX IF NOT EXISTS symbol_instances_object_offset_gist_idx
    ON index.symbol_instances USING GIST (object_id, offset_range);
