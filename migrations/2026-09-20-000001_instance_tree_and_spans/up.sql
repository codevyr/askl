-- Give every instance and reference its depth in the containment tree, and
-- every instance its finer spans.
--
-- Until now "which instance owns this symbol / this reference" was recomputed
-- on every read from offset ranges plus symbol_types.level.  Ranges alone
-- cannot answer it: everything a macro generates shares one extent.  For
-- PMU_FORMAT_ATTR(event, ...) libclang reports `event_show` and
-- `format_attr_event` as siblings with `__compiletime_assert_1121` inside
-- `event_show`'s body -- identical extents, different depths.  Depth is the
-- missing coordinate, and the indexer has it.
--
-- A direct child sits at exactly parent + 1, so ownership becomes a filter
-- instead of a correlated NOT EXISTS over range containment -- the construct
-- behind the 2.3-5.5s amdgpu statement, which the planner cannot cost.
--
-- Known limit, accepted: two co-extensive siblings at the same depth both
-- satisfy "contains it and is one level up", so a symbol nested under one is
-- over-attributed to the other as well.  Over-attribution, never loss.
--
-- offset_range keeps its meaning -- the EXTENT on instances, the NAME span on
-- refs -- so nothing is renamed.
--
-- There is deliberately NO backfill.  Depth is exactly what the old data
-- cannot express, so inventing one would bake in a wrong tree.  The columns
-- stay at their defaults until the projects are reindexed.

-- 1. Instances: depth, plus the finer spans.
--    Constant defaults, so these are metadata-only adds with no rewrite.
ALTER TABLE index.symbol_instances
    ADD COLUMN nesting_level   INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN dfs_enter       BIGINT  NOT NULL DEFAULT 0,
    ADD COLUMN dfs_exit        BIGINT  NOT NULL DEFAULT 0,
    ADD COLUMN name_range      INT4RANGE,
    ADD COLUMN signature_range INT4RANGE,
    ADD COLUMN content_range   INT4RANGE;

COMMENT ON COLUMN index.symbol_instances.offset_range IS
    'EXTENT: the authoritative containment span -- everything this instance owns.';
COMMENT ON COLUMN index.symbol_instances.nesting_level IS
    'Depth in this object''s containment tree, counting symbol instances only. A direct child is at exactly parent + 1.';
COMMENT ON COLUMN index.symbol_instances.dfs_enter IS
    'Depth-first entry number within this object. Descendants are exactly the instances whose dfs_enter lies strictly between this row''s dfs_enter and dfs_exit.';
COMMENT ON COLUMN index.symbol_instances.dfs_exit IS
    'Depth-first exit number. Numbered sparsely (stride 1024) so an ephemeral layer can insert between neighbours without renumbering.';

-- 2. Refs: the same depth scale, plus the full span of the construct.
ALTER TABLE index.symbol_refs
    ADD COLUMN nesting_level   INTEGER NOT NULL DEFAULT 0,
    ADD COLUMN dfs_enter       BIGINT  NOT NULL DEFAULT 0,
    ADD COLUMN from_full_range INT4RANGE;

COMMENT ON COLUMN index.symbol_refs.from_offset_range IS
    'NAME span: the referencing token itself.';
COMMENT ON COLUMN index.symbol_refs.from_full_range IS
    'FULL span: the whole referencing construct, e.g. printk(foo) where from_offset_range covers only printk. Display only.';
COMMENT ON COLUMN index.symbol_refs.nesting_level IS
    'Depth of this reference; an instance owns the refs at its own depth + 1.';
COMMENT ON COLUMN index.symbol_refs.dfs_enter IS
    'Position in the same DFS sequence as this object''s instances. A ref is a point, not an interval.';

-- 3. Ownership is a btree range scan over the DFS sequence: no GiST, no
--    recursion, and `unnest` differs from `direct children` by one extra term
--    on nesting_level.  This replaces the correlated NOT EXISTS over range
--    containment that costs 2.3-5.5s on amdgpu and that the planner cannot
--    cost (range predicates estimate at rows=1).
CREATE INDEX symbol_instances_object_dfs_idx
    ON index.symbol_instances (object_id, dfs_enter);

CREATE INDEX symbol_refs_object_dfs_idx
    ON index.symbol_refs (from_object, dfs_enter);
