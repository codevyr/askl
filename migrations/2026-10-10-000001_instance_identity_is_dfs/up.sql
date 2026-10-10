-- An instance is identified by its position in the containment tree, not by
-- its symbol and byte range.
--
-- `symbol_instances_layer_uq` has been (symbol, object_id, offset_range,
-- layer) since the layers work.  For a function that is fine.  For a macro
-- expansion it is wrong, because the same bytes can be expanded more than once
-- and mean something different each time.
--
-- A header with no include guard, included twice under different macro
-- definitions, expands ONE source line into two different functions:
--
--     #define DECL(name) int XCAT(name, SUFFIX)(int v) { return helper(v); }
--     DECL(alpha)                 -- in multi.h, included twice
--
--     #define SUFFIX _f           -- gives int alpha_f(int v) {...}
--     #define SUFFIX _d           -- gives int alpha_d(int v) {...}
--
-- Both generated functions survive today because their NAMES differ.  The two
-- `DECL` expansions do not: same symbol, same extent, so the pair is
-- unstorable, and the producer deleted one to satisfy this index -- after
-- numbering, so the references stamped against the dropped expansion were left
-- pointing into the hole it used to occupy.  Measured on ucx: 1105 of 2041
-- objects had instances removed and 552 references dangled, which is a
-- rejected upload, since validation is per object.
--
-- dfs_enter is already required to be unique within an object --
-- `validate_object_tree` enforces it at upload.  This makes the schema agree.
--
-- PARTIAL, on `dfs_exit <> 0`, and that is not a loophole.  `dfs_enter` and
-- `dfs_exit` were added with `NOT NULL DEFAULT 0` and NO backfill
-- (2026-09-20-000001_instance_tree_and_spans), so every row uploaded before
-- that migration sits at dfs_enter = 0 and a total unique index could not be
-- built at all -- any object with two such rows collides.  `dfs_exit = 0` is
-- already the established "never numbered" marker, read by the derivation in
-- `derive_instance_tree_for_tests`; a numbered row always has
-- dfs_exit >= dfs_enter + DFS_STRIDE, so the predicate separates the two
-- classes exactly.  Unnumbered rows answer no containment query correctly
-- anyway -- that is what the reindex is for -- and the exemption empties
-- itself as each project is re-uploaded.
--
-- Every index on offset_range stays.  Extents remain load-bearing: they are
-- what a scoped search's corpus is bounded by, and what the UI highlights.
-- They stop being identity.

DROP INDEX index.symbol_instances_layer_uq;

CREATE UNIQUE INDEX symbol_instances_layer_uq
    ON index.symbol_instances (object_id, dfs_enter, layer)
    WHERE dfs_exit <> 0;
