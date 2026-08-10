# Probe-shaped EXPLAIN baseline (cost-based execution, PR-P0)

EXPLAIN (ANALYZE, BUFFERS) of the planned cardinality-probe SQL shapes
(`probe_instance_ids`, roadmap PR-P5) against the compose DB, **before any
engine change**. Shapes in `probe_baseline.sql`; reproduce with:

```
docker compose exec -T db psql -U postgres -d askl -f - < perf/probe_baseline.sql
```

DB scale at measurement: 5.9M symbols / 14.8M symbol_instances / 23.3M
symbol_refs / 116.8k objects / 6 projects. Probe cap = 1000 (`LIMIT 1001`,
id-only projection). Visibility = Combined empty-chain (`layer = ANY(roots)`).
Cold = first run on a quiet DB; warm = second run, page cache populated.

| # | probe shape | cold | warm |
|---|---|--:|--:|
| S1 | bare, selective name (`"drm_dev_enter"`) | 1.2 ms | 0.3 ms |
| S2 | bare, capped type (`func`, 3.4M symbols) | 36 ms | 17 ms |
| S3 | bare, capped trigram glob (`g"*color*"`) | 77 ms | 55 ms |
| S4 | RefsParentsOf — `g"*lock*"` ∧ callers-of(vfs_read) | 0.9 ms | 1.2 ms |
| S5 | RefsChildrenOf — `g"*lock*"` ∧ callees-of(vfs_read) | 29 ms | 6.8 ms |
| S6 | HasChildrenOf, IN-subquery form | 2795 ms | 1794 ms |
| S7 | HasParentsOf, IN-subquery form | 10559 ms | 9212 ms |
| S6c | HasChildrenOf, **MATERIALIZED CTE** form | — | 6.9 ms |
| S7c | HasParentsOf, **MATERIALIZED CTE** form | — | 0.5 ms |
| S8 | worst-case refinement: `func` ∧ callers-of(drm_dev_enter) | 70 ms | 45 ms |
| S9 | wave-0 `mod("amdgpu")` (~800 instances, Resolved) | 3.3 ms | 3.6 ms |

## Findings (gates for PR-P5 / PR-F3)

**Q1 — capped probes terminate early: YES for btree/seq, front-loaded for
GIN.** S2's plan is `Limit rows=1001` over a nested loop that consumed only
4,917 of 13.3M instance rows before stopping — a capped probe over an
arbitrarily large type predicate costs ~tens of ms, independent of the
predicate's true cardinality. Trigram GIN (S3) does its bitmap work up
front (~18k candidate symbols for `%color%`) before the LIMIT can bite, so
glob probes cost the full bitmap scan — 55–77 ms here — but not the heap
materialisation. Acceptable; no per-probe statement_timeout needed at this
scale.

**Q2 — REFS role semi-joins: YES, plain IN-subquery is fine.** S4/S5 drive
from the bound id array through `symbol_refs_{to_symbol,from_object}_idx`
as nested loops; single-digit ms. `probe_instance_ids` can emit REFS roles
as IN-subqueries, matching `resolve_filter_to_ids`'s existing fragments.

**Q3 — HAS containment roles: IN-subquery form is BROKEN; MATERIALIZED CTE
is the required shape.** The containment subquery itself is fast and tiny
in both directions (203 rows in 1.4 ms via
`symbol_instances_object_offset_gist_idx`), but as an IN-subquery the
planner refuses to let it drive: S7 hash-semi-joins a **full 14.8M-row
probe-side scan** against 6 container ids (10.6 s, 3.6 GB read); S6
re-scans the materialised role set 43k times from the trigram side
(2.8 s). Forcing the role ids into a `MATERIALIZED` CTE — the same
planner-hint pattern as the existing `CteHasChildren` fast path — flips
both to id-driven pkey plans: **1794 ms → 6.9 ms and 9212 ms → 0.5 ms.**
PR-P5 must implement `ProbeRole::{HasChildrenOf,HasParentsOf}` as
materialized CTEs, never as IN-subqueries. Role-CTE size is bounded by the
containment neighbourhood of a Resolved (≤ cap) neighbour set — 203 rows
here; worth a debug-log if it ever exceeds ~100k.

**Q4 — the worst-case refinement probe is ~45 ms.** S8 is the probe that
replaces the unscoped sweep in `mod("amdgpu") { func { "drm_dev_enter" } }`
(REPORT.md line 1: 53,711 ms, one SQL returning 2.67M rows / 1.8 GB). As a
role-constrained probe it returns the callers of drm_dev_enter that are
functions in 45–70 ms — three orders of magnitude under the current plan,
and S9 confirms the `mod("amdgpu")` side resolves in wave 0 (~800 ids,
3.6 ms, comfortably under cap 1000).

## Design consequences recorded for the roadmap

- Probe primitive stays `LIMIT cap+1` id-fetch (validated: early
  termination real, id projection keeps capped probes ≤ ~8 KB out).
- REFS roles: IN-subquery fragments (share with `resolve_filter_to_ids`).
- HAS roles: MATERIALIZED-CTE fragments (extend the `CteHasChildren`
  pattern). This supersedes the plan's open risk "HAS-role probe SQL is
  novel" — validated with the CTE shape only.
- GIN-backed anchors make wave-0 probes cost a full bitmap scan; fine at
  current scale, revisit per-probe timeouts only if content/trigram
  predicates regress on bigger indexes.
