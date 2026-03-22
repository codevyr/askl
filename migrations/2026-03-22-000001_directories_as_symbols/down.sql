-- Rollback: Recreate directories table and objects.directory_id column

-- Step 1: Recreate directories table
CREATE TABLE IF NOT EXISTS index.directories (
    id SERIAL PRIMARY KEY,
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE,
    parent_id INTEGER REFERENCES index.directories(id) ON DELETE CASCADE,
    path TEXT NOT NULL,
    UNIQUE (project_id, path)
);

-- Step 2: Recreate directory indexes
CREATE INDEX IF NOT EXISTS directories_project_parent_idx
    ON index.directories (project_id, parent_id);

CREATE INDEX IF NOT EXISTS directories_project_path_idx
    ON index.directories (project_id, path);

-- Step 3: Add directory_id column back to objects (nullable since we can't restore data)
ALTER TABLE index.objects ADD COLUMN directory_id INTEGER REFERENCES index.directories(id) ON DELETE CASCADE;

-- Step 4: Recreate objects_directory_idx
CREATE INDEX IF NOT EXISTS objects_directory_idx
    ON index.objects (directory_id);

-- Note: Data in directories table cannot be restored automatically.
-- A full re-index is required after rolling back this migration.
