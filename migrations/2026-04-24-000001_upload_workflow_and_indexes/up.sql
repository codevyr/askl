-- Change symbols.id from SERIAL INT to explicit BIGINT.
-- Symbol IDs are now computed by the upload server as:
--   project_id::bigint << 32 | local_id
-- This eliminates the upload_symbol_map lookup table entirely.

-- Drop FK constraints from the referencing columns before altering types.
ALTER TABLE index.symbol_instances DROP CONSTRAINT IF EXISTS symbol_instances_symbol_fkey;
ALTER TABLE index.symbol_refs DROP CONSTRAINT IF EXISTS symbol_refs_to_symbol_fkey;

ALTER TABLE index.symbol_instances ALTER COLUMN symbol TYPE BIGINT USING symbol::BIGINT;
ALTER TABLE index.symbol_refs ALTER COLUMN to_symbol TYPE BIGINT USING to_symbol::BIGINT;

ALTER TABLE index.symbols ALTER COLUMN id TYPE BIGINT USING id::BIGINT;
ALTER TABLE index.symbols ALTER COLUMN id DROP DEFAULT;
DROP SEQUENCE IF EXISTS index.symbols_id_seq;

ALTER TABLE index.symbol_instances
    ADD CONSTRAINT symbol_instances_symbol_fkey
    FOREIGN KEY (symbol) REFERENCES index.symbols(id) ON DELETE CASCADE;
ALTER TABLE index.symbol_refs
    ADD CONSTRAINT symbol_refs_to_symbol_fkey
    FOREIGN KEY (to_symbol) REFERENCES index.symbols(id) ON DELETE CASCADE;

-- Upload lifecycle: projects gain an upload_status column.
ALTER TABLE index.projects
    ADD COLUMN upload_status TEXT NOT NULL DEFAULT 'complete';

ALTER TABLE index.projects
    ADD CONSTRAINT projects_upload_status_check
    CHECK (upload_status IN ('uploading', 'complete', 'failed', 'deleting'));

-- Trigger: skip symbol_path recomputation on INSERT when the caller pre-fills it.
-- Bulk uploads pre-compute symbol_path in Rust; the trigger only fires when
-- symbol_path is NULL (direct SQL inserts) or when name is renamed.
DROP TRIGGER IF EXISTS symbols_set_path ON index.symbols;
CREATE TRIGGER symbols_set_path
BEFORE INSERT ON index.symbols
FOR EACH ROW
WHEN (NEW.symbol_path IS NULL)
EXECUTE FUNCTION index.set_symbol_path();

CREATE TRIGGER symbols_set_path_on_rename
BEFORE UPDATE OF name ON index.symbols
FOR EACH ROW
EXECUTE FUNCTION index.set_symbol_path();

-- Performance indexes.
CREATE INDEX objects_project_id_idx ON index.objects (project_id);

-- symbols_project_id_idx is subsumed by composite indexes added earlier.
DROP INDEX IF EXISTS index.symbols_project_id_idx;

-- B-tree on symbol_instances.object_id for fast FK checks during delete_project.
CREATE INDEX symbol_instances_object_id_idx ON index.symbol_instances (object_id);

-- B-tree on symbol_instances.symbol for fast FK checks and join scans.
-- Without this index, delete_project and child/parent queries do full table scans.
CREATE INDEX symbol_instances_symbol_idx ON index.symbol_instances (symbol);

-- (project_id, name) index for exact-name symbol lookups (ExactNameMixin).
CREATE INDEX symbols_project_name_idx ON index.symbols (project_id, name);

-- NOTE: object_contents.object_id is already the PRIMARY KEY of that table,
-- so no additional UNIQUE constraint is needed.
