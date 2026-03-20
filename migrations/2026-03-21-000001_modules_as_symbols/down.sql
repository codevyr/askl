-- Revert Phase 1.9: This is a destructive migration that cannot be fully reversed
-- because module information is lost. This down migration recreates the schema
-- but cannot restore the data.

-- Step 1: Recreate modules table
CREATE TABLE IF NOT EXISTS index.modules
(
    id SERIAL PRIMARY KEY,
    module_name TEXT NOT NULL,
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE
);

-- Step 2: Add module column back to objects
ALTER TABLE index.objects ADD COLUMN module INTEGER REFERENCES index.modules(id) ON DELETE CASCADE;

-- Step 3: Add module column back to symbols
ALTER TABLE index.symbols ADD COLUMN module INTEGER;

-- Step 4: Drop the new uniqueness constraint
ALTER TABLE index.symbols DROP CONSTRAINT IF EXISTS symbols_name_project_key;

-- Step 5: Drop project_id index and constraint from symbols
DROP INDEX IF EXISTS index.symbols_project_id_idx;
ALTER TABLE index.symbols DROP CONSTRAINT IF EXISTS symbols_project_fk;
ALTER TABLE index.symbols DROP COLUMN project_id;

-- Step 6: Add back old uniqueness constraint (will be empty since module is null)
-- Note: Cannot restore data, so this will need manual intervention
ALTER TABLE index.symbols ADD CONSTRAINT symbols_name_module_key UNIQUE (name, module);

-- Note: Module data is lost and cannot be restored. You will need to re-index projects.
