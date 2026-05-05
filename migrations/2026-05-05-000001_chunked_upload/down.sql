DROP TABLE IF EXISTS index.project_object_chunks;
DROP TABLE IF EXISTS index.project_symbol_chunks;
ALTER TABLE index.projects DROP COLUMN IF EXISTS object_chunks_total;
ALTER TABLE index.projects DROP COLUMN IF EXISTS symbol_chunks_total;
