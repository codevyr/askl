-- C projects can have multiple LOCAL symbols with the same name in different
-- files (e.g., static int count in a.c and b.c). The per-project unique
-- constraint on (name, project_id) is too strict. Drop it and rely on the
-- indexer's internal deduplication instead.
ALTER TABLE index.symbols DROP CONSTRAINT IF EXISTS symbols_name_project_key;
