-- Composite index for tree-browser direct-child queries.
-- The /tree endpoint filters by (project_id, symbol_type) and then range-scans
-- by name prefix (starts_with). Without symbol_type in the index key, every
-- starts_with scan over a large prefix (e.g. '/linux/') touches all symbols in
-- the project -- millions of rows for the Linux kernel -- before filtering to
-- the handful of Directory entries that are direct children.
-- With this index the planner can use (project_id=N, symbol_type=4, name >= prefix)
-- for a range scan that only touches Directory (or File) symbols.
CREATE INDEX symbols_project_type_name_idx
    ON index.symbols (project_id, symbol_type, name);

-- Functional index for direct-child queries in the /tree endpoint.
--
-- The main join in load_tree_children_multi finds direct children of a set of
-- directory paths.  Without this index, starts_with(name, prefix) triggers a
-- range scan that touches ALL descendants of the prefix (e.g. all of /linux/
-- for the Linux kernel) and then filters with a per-row depth expression.
-- That is O(all_descendants) per prefix.
--
-- With nlevel(symbol_path) as the third index key the planner can emit a
-- tight range scan:
--   (project_id=N, symbol_type=4, nlevel=D, name >= prefix AND name < prefix_next)
-- which touches only the O(direct_children) rows at the correct depth.
CREATE INDEX symbols_project_type_nlevel_name_idx
    ON index.symbols (project_id, symbol_type, nlevel(symbol_path), name);
