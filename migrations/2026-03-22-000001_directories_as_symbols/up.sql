-- Phase 3+4: Files and directories as symbols
-- The directories table is no longer needed - directories become symbols with instances.
-- Files also become symbols (created by Go indexer).
-- Directory symbols are created server-side during upload.

-- Step 1: Drop indexes that depend on directory_id
DROP INDEX IF EXISTS index.objects_directory_idx;

-- Step 2: Drop directory_id foreign key and column from objects
ALTER TABLE index.objects DROP CONSTRAINT IF EXISTS objects_directory_id_fkey;
ALTER TABLE index.objects DROP COLUMN IF EXISTS directory_id;

-- Step 3: Drop directory-related indexes
DROP INDEX IF EXISTS index.directories_project_parent_idx;
DROP INDEX IF EXISTS index.directories_project_path_idx;

-- Step 4: Drop directories table
DROP TABLE IF EXISTS index.directories CASCADE;
