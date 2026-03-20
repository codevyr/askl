-- Phase 1.9: Convert modules to symbols
-- Modules become symbols with type=MODULE. Symbol uniqueness changes from per-module to per-project.

-- Step 1: Add project_id column to symbols
ALTER TABLE index.symbols ADD COLUMN project_id INTEGER;

-- Step 2: Backfill project_id from module→project relationship
UPDATE index.symbols s
SET project_id = m.project_id
FROM index.modules m
WHERE s.module = m.id;

-- Step 3: Make project_id NOT NULL and add FK constraint
ALTER TABLE index.symbols ALTER COLUMN project_id SET NOT NULL;
ALTER TABLE index.symbols ADD CONSTRAINT symbols_project_fk
    FOREIGN KEY (project_id) REFERENCES index.projects(id) ON DELETE CASCADE;

-- Step 4: Drop old uniqueness constraint, add new (per project instead of per module)
ALTER TABLE index.symbols DROP CONSTRAINT IF EXISTS symbols_name_module_key;
ALTER TABLE index.symbols ADD CONSTRAINT symbols_name_project_key UNIQUE (name, project_id);

-- Step 5: Drop module column from symbols
ALTER TABLE index.symbols DROP COLUMN module;

-- Step 6: Drop module column from objects
ALTER TABLE index.objects DROP COLUMN module;

-- Step 7: Drop modules table
DROP TABLE IF EXISTS index.modules CASCADE;

-- Add index on project_id for symbols
CREATE INDEX IF NOT EXISTS symbols_project_id_idx ON index.symbols (project_id);
