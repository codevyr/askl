DROP INDEX IF EXISTS index.symbol_refs_object_dfs_idx;
DROP INDEX IF EXISTS index.symbol_instances_object_dfs_idx;
-- from earlier drafts of this migration
DROP INDEX IF EXISTS index.symbol_refs_object_nesting_idx;
-- also clears columns an earlier draft of this migration created
DROP INDEX IF EXISTS index.symbol_refs_parent_idx;
DROP INDEX IF EXISTS index.symbol_instances_parent_idx;

ALTER TABLE index.symbol_refs
    DROP COLUMN IF EXISTS fold_depth,
    DROP COLUMN IF EXISTS folded,
    DROP COLUMN IF EXISTS dfs_enter,
    DROP COLUMN IF EXISTS from_full_range,
    DROP COLUMN IF EXISTS nesting_level,
    DROP COLUMN IF EXISTS parent_instance;

ALTER TABLE index.symbol_instances
    DROP COLUMN IF EXISTS fold_depth,
    DROP COLUMN IF EXISTS folded,
    DROP COLUMN IF EXISTS dfs_exit,
    DROP COLUMN IF EXISTS dfs_enter,
    DROP COLUMN IF EXISTS content_range,
    DROP COLUMN IF EXISTS signature_range,
    DROP COLUMN IF EXISTS name_range,
    DROP COLUMN IF EXISTS nesting_level,
    DROP COLUMN IF EXISTS parent_instance;

-- Restore symbol_types.level with the values it last held.  Higher was
-- broader: field/content 1, type/function/data/macro 2, file 3, module 4,
-- directory 5.
ALTER TABLE index.symbol_types ADD COLUMN IF NOT EXISTS level INTEGER NOT NULL DEFAULT 1;
UPDATE index.symbol_types SET level = CASE name
    WHEN 'directory' THEN 5
    WHEN 'module'    THEN 4
    WHEN 'file'      THEN 3
    WHEN 'type'      THEN 2
    WHEN 'function'  THEN 2
    WHEN 'data'      THEN 2
    WHEN 'macro'     THEN 2
    ELSE 1
END;
