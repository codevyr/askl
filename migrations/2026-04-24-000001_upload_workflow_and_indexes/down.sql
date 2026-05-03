-- NOTE: Only usable on an empty database; BIGINT→INT truncates computed IDs.

DROP INDEX IF EXISTS index.symbols_project_name_idx;
DROP INDEX IF EXISTS index.symbol_instances_symbol_idx;
DROP INDEX IF EXISTS index.symbol_instances_object_id_idx;
CREATE INDEX IF NOT EXISTS symbols_project_id_idx ON index.symbols (project_id);
DROP INDEX IF EXISTS index.objects_project_id_idx;

DROP TRIGGER IF EXISTS symbols_set_path ON index.symbols;
DROP TRIGGER IF EXISTS symbols_set_path_on_rename ON index.symbols;
CREATE TRIGGER symbols_set_path
BEFORE INSERT OR UPDATE OF name ON index.symbols
FOR EACH ROW
EXECUTE FUNCTION index.set_symbol_path();

ALTER TABLE index.projects DROP CONSTRAINT IF EXISTS projects_upload_status_check;
ALTER TABLE index.projects DROP COLUMN IF EXISTS upload_status;

-- Restore INT symbol IDs.
ALTER TABLE index.symbol_instances DROP CONSTRAINT IF EXISTS symbol_instances_symbol_fkey;
ALTER TABLE index.symbol_refs DROP CONSTRAINT IF EXISTS symbol_refs_to_symbol_fkey;

ALTER TABLE index.symbol_instances ALTER COLUMN symbol TYPE INTEGER USING symbol::INTEGER;
ALTER TABLE index.symbol_refs ALTER COLUMN to_symbol TYPE INTEGER USING to_symbol::INTEGER;

CREATE SEQUENCE IF NOT EXISTS index.symbols_id_seq AS INTEGER;
ALTER TABLE index.symbols ALTER COLUMN id TYPE INTEGER USING id::INTEGER;
ALTER TABLE index.symbols ALTER COLUMN id SET DEFAULT nextval('index.symbols_id_seq');
ALTER SEQUENCE index.symbols_id_seq OWNED BY index.symbols.id;
-- Position the sequence above any existing rows so the next INSERT doesn't
-- collide with an already-stored id.
SELECT setval(
    'index.symbols_id_seq',
    COALESCE((SELECT MAX(id) FROM index.symbols), 0) + 1,
    false
);

ALTER TABLE index.symbol_instances
    ADD CONSTRAINT symbol_instances_symbol_fkey
    FOREIGN KEY (symbol) REFERENCES index.symbols(id) ON DELETE CASCADE;
ALTER TABLE index.symbol_refs
    ADD CONSTRAINT symbol_refs_to_symbol_fkey
    FOREIGN KEY (to_symbol) REFERENCES index.symbols(id) ON DELETE CASCADE;
