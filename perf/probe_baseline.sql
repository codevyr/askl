-- Probe-shaped EXPLAIN baseline (roadmap PR-P0).
--
-- Validates the SQL shapes of the planned cardinality probes
-- (`probe_instance_ids`, roadmap PR-P5) before any engine change:
--   * a probe is the statement's CurrentQuery (symbols ⋈ symbol_instances
--     ⋈ projects ⋈ objects, cf. build_current_query in
--     index/src/db_diesel/index_impl.rs) with an id-only projection and
--     LIMIT :cap + 1 — never COUNT(*);
--   * role variants add the semi-join subqueries that the refinement loop
--     will bind resolved neighbour ids into (ProbeRole::{RefsChildrenOf,
--     RefsParentsOf, HasChildrenOf, HasParentsOf});
--   * visibility is the Combined empty-chain form: layer = ANY(roots),
--     matching resolve_filter_to_ids' cached_load mode.
--
-- Run against the compose DB:
--   docker compose exec -T db psql -U postgres -d askl \
--     -f - < perf/probe_baseline.sql
--
-- Questions this baseline answers (gates for PR-P5 / PR-F3):
--   Q1  Do capped probes terminate early (btree/seq under LIMIT), or does
--       the access method do its full work up front (GIN bitmap)?
--   Q2  Do the REFS role semi-joins drive from the bound id array through
--       symbol_refs_{to_symbol,from_object}_idx?
--   Q3  Does the HAS containment semi-join (no engine precedent) use
--       symbol_instances_object_offset_gist_idx, or regress to a join scan?
--   Q4  Is the worst-case refinement probe (func ∧ RefsParentsOf(tiny set))
--       milliseconds where today's plan materialises 2.67M rows?

\set cap 1000
\set QUIET on
\pset pager off
\timing on

-- Root layers = every project's root_layer_id (empty eph chain).
SELECT array_agg(root_layer_id) AS roots FROM index.projects;
\gset

-- Resolved-neighbour id sets, inlined as literal arrays so the planner
-- sees the same cardinality it will see from the engine's int8[] binds.
SELECT string_agg(si.id::text, ',') AS amdgpu_ids
  FROM index.symbols s JOIN index.symbol_instances si ON si.symbol = s.id
 WHERE s.leaf_name = 'amdgpu' AND s.symbol_type = 3;
\gset
SELECT string_agg(si.id::text, ',') AS drm_dev_enter_ids
  FROM index.symbols s JOIN index.symbol_instances si ON si.symbol = s.id
 WHERE s.leaf_name = 'drm_dev_enter';
\gset
SELECT string_agg(si.id::text, ',') AS vfs_read_ids
  FROM index.symbols s JOIN index.symbol_instances si ON si.symbol = s.id
 WHERE s.leaf_name = 'vfs_read';
\gset
SELECT string_agg(si.id::text, ',') AS color_adjust_ids
  FROM index.symbols s JOIN index.symbol_instances si ON si.symbol = s.id
 WHERE s.leaf_name = 'color_adjust';
\gset
SELECT string_agg(si.id::text, ',') AS drm_mm_c_ids
  FROM index.symbols s JOIN index.symbol_instances si ON si.symbol = s.id
 WHERE s.leaf_name = 'drm_mm_c' AND s.symbol_type = 2;
\gset

\echo '=== S1: bare probe, selective name anchor ("drm_dev_enter") ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si.id
  FROM index.symbols s
  JOIN index.symbol_instances si ON s.id = si.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si.object_id
 WHERE s.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
   AND s.leaf_name = 'drm_dev_enter'
 LIMIT :cap + 1;

\echo '=== S2: bare probe, capped type predicate (func — 3.4M symbols) ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si.id
  FROM index.symbols s
  JOIN index.symbol_instances si ON s.id = si.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si.object_id
 WHERE s.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
   AND s.symbol_type = 1
 LIMIT :cap + 1;

\echo '=== S3: bare probe, capped trigram glob (g"*color*") ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si.id
  FROM index.symbols s
  JOIN index.symbol_instances si ON s.id = si.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si.object_id
 WHERE s.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
   AND s.leaf_name ILIKE '%color%'
 LIMIT :cap + 1;

\echo '=== S4: RefsParentsOf role — g"*lock*" callers of vfs_read ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
   AND s.leaf_name ILIKE '%lock%'
   AND si0.id IN (
        SELECT pd.id FROM index.symbol_refs sr
        JOIN index.symbol_instances pd ON pd.object_id = sr.from_object
         AND pd.offset_range @> sr.from_offset_range
        JOIN index.symbol_instances si ON si.symbol = sr.to_symbol
        WHERE sr.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
          AND pd.layer = ANY(:'roots')
          AND si.id = ANY(ARRAY[:vfs_read_ids]::int8[]))
 LIMIT :cap + 1;

\echo '=== S5: RefsChildrenOf role — g"*lock*" callees of vfs_read ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
   AND s.leaf_name ILIKE '%lock%'
   AND si0.id IN (
        SELECT si.id FROM index.symbol_refs sr
        JOIN index.symbol_instances si ON si.symbol = sr.to_symbol
        JOIN index.symbol_instances pd ON pd.object_id = sr.from_object
         AND pd.offset_range @> sr.from_offset_range
        WHERE sr.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
          AND pd.layer = ANY(:'roots')
          AND pd.id = ANY(ARRAY[:vfs_read_ids]::int8[]))
 LIMIT :cap + 1;

\echo '=== S6: HasChildrenOf role — g"*color*" contained in drm_mm_c ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
   AND s.leaf_name ILIKE '%color%'
   AND si0.id IN (
        SELECT child.id
          FROM index.symbol_instances parent
          JOIN index.symbol_instances child ON child.object_id = parent.object_id
           AND parent.offset_range @> child.offset_range
           AND child.id <> parent.id
          JOIN index.symbols cs ON cs.id = child.symbol
          JOIN index.symbols ps ON ps.id = parent.symbol
          JOIN index.symbol_types ct ON ct.id = cs.symbol_type
          JOIN index.symbol_types pt ON pt.id = ps.symbol_type
         WHERE parent.layer = ANY(:'roots') AND child.layer = ANY(:'roots')
           AND pt.level >= ct.level
           AND parent.id = ANY(ARRAY[:drm_mm_c_ids]::int8[]))
 LIMIT :cap + 1;

\echo '=== S7: HasParentsOf role — containers of color_adjust ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
   AND si0.id IN (
        SELECT parent.id
          FROM index.symbol_instances child
          JOIN index.symbol_instances parent ON parent.object_id = child.object_id
           AND parent.offset_range @> child.offset_range
           AND parent.id <> child.id
          JOIN index.symbols cs ON cs.id = child.symbol
          JOIN index.symbols ps ON ps.id = parent.symbol
          JOIN index.symbol_types ct ON ct.id = cs.symbol_type
          JOIN index.symbol_types pt ON pt.id = ps.symbol_type
         WHERE parent.layer = ANY(:'roots') AND child.layer = ANY(:'roots')
           AND pt.level >= ct.level
           AND child.id = ANY(ARRAY[:color_adjust_ids]::int8[]))
 LIMIT :cap + 1;

\echo '=== S6c: HasChildrenOf via MATERIALIZED CTE (planner-hint fix) ==='
-- S6/S7 show the same planner weakness the engine already works around
-- with CteHasChildren (index_impl.rs): the containment subquery is tiny
-- and fast, but as an IN-subquery the planner refuses to drive the probe
-- side from it (S7 hash-joins a full 14.8M-row scan against 6 ids; S6
-- re-probes the materialised set 43k times).  Materialising the role ids
-- first forces the probe to run id-driven through the pkey.
EXPLAIN (ANALYZE, BUFFERS)
WITH role_ids AS MATERIALIZED (
    SELECT child.id
      FROM index.symbol_instances parent
      JOIN index.symbol_instances child ON child.object_id = parent.object_id
       AND parent.offset_range @> child.offset_range
       AND child.id <> parent.id
      JOIN index.symbols cs ON cs.id = child.symbol
      JOIN index.symbols ps ON ps.id = parent.symbol
      JOIN index.symbol_types ct ON ct.id = cs.symbol_type
      JOIN index.symbol_types pt ON pt.id = ps.symbol_type
     WHERE parent.layer = ANY(:'roots') AND child.layer = ANY(:'roots')
       AND pt.level >= ct.level
       AND parent.id = ANY(ARRAY[:drm_mm_c_ids]::int8[]))
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
  JOIN role_ids r ON r.id = si0.id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
   AND s.leaf_name ILIKE '%color%'
 LIMIT :cap + 1;

\echo '=== S7c: HasParentsOf via MATERIALIZED CTE (planner-hint fix) ==='
EXPLAIN (ANALYZE, BUFFERS)
WITH role_ids AS MATERIALIZED (
    SELECT parent.id
      FROM index.symbol_instances child
      JOIN index.symbol_instances parent ON parent.object_id = child.object_id
       AND parent.offset_range @> child.offset_range
       AND parent.id <> child.id
      JOIN index.symbols cs ON cs.id = child.symbol
      JOIN index.symbols ps ON ps.id = parent.symbol
      JOIN index.symbol_types ct ON ct.id = cs.symbol_type
      JOIN index.symbol_types pt ON pt.id = ps.symbol_type
     WHERE parent.layer = ANY(:'roots') AND child.layer = ANY(:'roots')
       AND pt.level >= ct.level
       AND child.id = ANY(ARRAY[:color_adjust_ids]::int8[]))
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
  JOIN role_ids r ON r.id = si0.id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
 LIMIT :cap + 1;

\echo '=== S8: worst-case refinement probe — func ∧ RefsParentsOf(drm_dev_enter) ==='
-- The probe that replaces today's 2.67M-row unscoped sweep in
-- `mod("amdgpu") { func { "drm_dev_enter" } }` (perf/REPORT.md line 1).
EXPLAIN (ANALYZE, BUFFERS)
SELECT si0.id
  FROM index.symbols s
  JOIN index.symbol_instances si0 ON s.id = si0.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si0.object_id
 WHERE s.layer = ANY(:'roots') AND si0.layer = ANY(:'roots')
   AND s.symbol_type = 1
   AND si0.id IN (
        SELECT pd.id FROM index.symbol_refs sr
        JOIN index.symbol_instances pd ON pd.object_id = sr.from_object
         AND pd.offset_range @> sr.from_offset_range
        JOIN index.symbol_instances si ON si.symbol = sr.to_symbol
        WHERE sr.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
          AND pd.layer = ANY(:'roots')
          AND si.id = ANY(ARRAY[:drm_dev_enter_ids]::int8[]))
 LIMIT :cap + 1;

\echo '=== S9: wave-0 probe for mod("amdgpu") — the ~800-instance Resolved case ==='
EXPLAIN (ANALYZE, BUFFERS)
SELECT si.id
  FROM index.symbols s
  JOIN index.symbol_instances si ON s.id = si.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si.object_id
 WHERE s.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
   AND s.leaf_name = 'amdgpu' AND s.symbol_type = 3
 LIMIT :cap + 1;

\echo '=== S10: compound-name probe — field "qaic_bo.dbc" (the 17 s report) ==='
-- Verbatim shape the engine emits for a name containing '.', '/' or ':':
-- CompoundNameMixin with leaf_anchored=false contributes ONLY the lquery
-- (index/src/db_diesel/mixins.rs, CompoundNameMixin::current_expr), so the
-- probe's sole indexable predicate is a leading-`*` lquery.  GiST ltree
-- cannot prune on a leading `*`, so this walks the 1.5 GB
-- symbols_project_path_gist_idx end to end to return one row.
EXPLAIN (ANALYZE, BUFFERS)
SELECT si.id
  FROM index.symbols s
  JOIN index.symbol_instances si ON s.id = si.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si.object_id
 WHERE s.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
   AND s.symbol_type = 8
   AND s.symbol_path ~ '*.qaic_bo.*.dbc.*'::lquery
 LIMIT :cap + 1;

\echo '=== S11: same name, ANCHORED lquery — what pruning is worth ==='
-- Not the engine's semantics (it must match qaic_bo..dbc at any depth), only
-- the control that isolates the leading-`*` as the cause: identical index,
-- identical row, two orders of magnitude fewer pages.
EXPLAIN (ANALYZE, BUFFERS)
SELECT s.id FROM index.symbols s
 WHERE s.symbol_path ~ 'qaic_bo.dbc'::lquery AND s.symbol_type = 8
 LIMIT :cap + 1;

\echo '=== S12: trigram-seeded candidate + lquery recheck — the fix shape ==='
-- Preserves the ordered-subset semantics exactly (the lquery still decides;
-- the LIKE is only a superset pre-filter, since an ordered-subset match of
-- tokens a..b implies name LIKE '%a%b%').  Seeds from
-- symbols_name_trgm_idx instead of the GiST walk.
EXPLAIN (ANALYZE, BUFFERS)
WITH cand AS MATERIALIZED (
  SELECT s.id, s.symbol_path FROM index.symbols s
   WHERE s.name LIKE '%qaic_bo%dbc%' AND s.symbol_type = 8
     AND s.layer = ANY(:'roots'))
SELECT id FROM cand WHERE symbol_path ~ '*.qaic_bo.*.dbc.*'::lquery
 LIMIT :cap + 1;

\echo '=== S13: compound-name probe AFTER the fix (needs symbols_path_tokens_gin) ==='
-- The predicate CompoundNameMixin emits once 2026-08-30-000001 is applied:
-- a token containment as the access path, with the lquery cast off the
-- GiST-indexed column so it can only be a recheck.  This is S10's query with
-- S10's semantics; only the access path differs.  Expect ~19 pages, sub-ms.
EXPLAIN (ANALYZE, BUFFERS)
SELECT si.id
  FROM index.symbols s
  JOIN index.symbol_instances si ON s.id = si.symbol
  JOIN index.projects p ON s.project_id = p.id
  JOIN index.objects o ON o.id = si.object_id
 WHERE s.layer = ANY(:'roots') AND si.layer = ANY(:'roots')
   AND s.symbol_type = 8
   AND string_to_array(s.symbol_path::text, '.') @> ARRAY['qaic_bo','dbc']::text[]
   AND (s.symbol_path::text)::ltree ~ '*.qaic_bo.*.dbc.*'::lquery
 LIMIT :cap + 1;
