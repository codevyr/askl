DROP INDEX IF EXISTS index.symbol_refs_object_dfs_idx;
DROP INDEX IF EXISTS index.symbol_instances_object_dfs_idx;
-- from earlier drafts of this migration
DROP INDEX IF EXISTS index.symbol_refs_object_nesting_idx;
-- also clears columns an earlier draft of this migration created
DROP INDEX IF EXISTS index.symbol_refs_parent_idx;
DROP INDEX IF EXISTS index.symbol_instances_parent_idx;

ALTER TABLE index.symbol_refs
    DROP COLUMN IF EXISTS dfs_enter,
    DROP COLUMN IF EXISTS from_full_range,
    DROP COLUMN IF EXISTS nesting_level,
    DROP COLUMN IF EXISTS parent_instance;

ALTER TABLE index.symbol_instances
    DROP COLUMN IF EXISTS dfs_exit,
    DROP COLUMN IF EXISTS dfs_enter,
    DROP COLUMN IF EXISTS content_range,
    DROP COLUMN IF EXISTS signature_range,
    DROP COLUMN IF EXISTS name_range,
    DROP COLUMN IF EXISTS nesting_level,
    DROP COLUMN IF EXISTS parent_instance;
