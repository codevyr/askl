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

## Addendum (2026-08-30) — compound names walk the whole GiST index

Added after a `func {field "qaic_bo.dbc"}` report: **17.3 s cold**, of which
17.279 s is a single `probe completed elapsed_ms=17279 rows=1` (askld log);
every other SQL in that query is 1–9 ms. Shapes S10–S12 in
`probe_baseline.sql`.

| # | probe shape | pages read | warm |
|---|---|--:|--:|
| S10 | compound name, engine's lquery `*.qaic_bo.*.dbc.*` | 79,456 | 390 ms |
| S11 | same name, anchored lquery `qaic_bo.dbc` | 642 | 4.1 ms |
| S12 | trigram-seeded candidates + lquery recheck | 41 | 0.4 ms |
| S13 | **shipped fix** — token containment + demoted lquery | **19** | **0.086 ms** |

**Root cause.** `name_filter` (askld/src/verb/generic/selectors.rs) splits on
whether the name contains `.`, `/` or `:`. A *non*-compound name gets
`LeafNameMixin` → `leaf_name = 'x'`, a selective btree equality (S1: 0.3 ms).
A **compound** name gets `CompoundNameMixin::new` — and with
`leaf_anchored=false` that mixin contributes *only* the lquery
(`mixins.rs::CompoundNameMixin::current_expr` pushes `leaf_name` **only**
when `leaf_token` is `Some`, i.e. only in the leaf-anchored constructor).
`build_lquery` always prefixes `*.` to express ordered-subset matching, and
a leading-`*` lquery is unprunable by GiST ltree — so the probe's one
indexable predicate degenerates into a full walk of the 1.5 GB
`symbols_project_path_gist_idx`. 620 MB of reads against `shared_buffers =
128 MB` on a 24 GB database means the walk is served from the OS page cache
at best: ~390 ms warm, 17.3 s cold. `symbol_type = 8` and the layer
predicate are applied as post-index Filters and prune nothing.

This is not specific to the reported name — any compound name costs the same
walk. Probing the index directly with single-label lqueries isolates it as a
property of the access method rather than of this pattern: `*.dbc.*` reads
116,247 pages and `*.vfs_read.*` 120,041, for 6 and 9 rows. (Those two are
index probes, not reachable queries — a single-token name always takes
`LeafNameMixin`, and `match="contains"` does not change that; verified against
the running server. The reachable slow shapes are a compound code-symbol name,
which is unanchored by default, and a compound `file`/`dir` name with
`match="contains"`.) It is only invisible in REPORT.md because no
symbol name in that corpus is compound: every non-`file` name there is a
single token, and the `file(...)` entries dodge it twice over --
`/linux/fs/read_write.c` is absolute so it takes `ExactNameMixin`, and
`qaic.h` / `drm_mm.c` are FILE-typed, where `dot_is_separator=false` folds
the dot into `qaic_h` and compoundness is decided by `/` and `:` alone.

**Why the obvious fix is wrong.** Emitting `leaf_name = 'dbc'` alongside the
unanchored lquery would be *unsound*: unanchored means the last token may sit
at any depth (`qaic_bo.dbc.field` must still match), so the last token is not
necessarily the leaf. S12 is the sound shape — an ordered-subset match on
tokens `a…b` implies `name LIKE '%a%b%'`, so the trigram index can generate a
superset of candidates and the lquery stays the decider. Semantics unchanged,
1,900× fewer pages.

**Fix (2026-08-30, same branch).** Migration
`2026-08-30-000001_symbols_path_tokens_gin` adds a GIN *expression* index over
`string_to_array(symbol_path::text,'.')`, and `CompoundNameMixin` now emits a
token containment against it beside the lquery -- which is simultaneously cast
to `(symbol_path::text)::ltree` so the planner cannot BitmapAnd the GiST walk
back in. **S13** is S10's query with the new access path.

Measured after deploying, S10 and S13 back to back on the same cache state
(the redeploy left the GiST index cold, which is why S10 shows its cold cost
rather than the 390 ms warm figure above):

| | S10, before | S13, after |
|---|--:|--:|
| pages | 79,456 | **19** |
| time | 13,225 ms | **0.086 ms** |

End to end, `func {field "qaic_bo.dbc"}` went from **19.6 s cold** to 23-56 ms,
and the probe that was the whole query — `probe completed elapsed_ms=19636` —
now logs 0-9 ms. Output is byte-for-byte identical: 7 symbols, 9 refs, same
symbols in the same order. The rest of the corpus followed: `"qaic_bo.dbc"`
0.57 s -> 11 ms, `func "qaic_data.c"` 0.94 s -> 10 ms, against unchanged
single-token controls (`type "drm_gem_object"` 95 ms, `"vfs_read"` 12 ms) — so
the gap the regression gate watches is closed. Semantics are unchanged, and the two
pre-existing guards in `index/src/symbols_test.rs` prove it: `"kubelet.run"`
still matches only the depth-3 path, and `"run.kubelet"` still matches nothing
(that second one is what fails if anyone decides the lquery recheck looks
redundant next to the containment). Note the code is *worse* than the status
quo without the index -- 227,400 pages, a parallel seq scan -- so the migration
and the engine change must not be separated; diesel runs migrations at boot
before askld serves, which is what keeps them together.

**Regression watch.** `perf/queries.txt` now carries `field "qaic_bo.dbc"`,
`"qaic_bo.dbc"` and `func "qaic_data.c"` next to the single-token control
`type "drm_gem_object"`. Compare the *gap*: on a healthy engine the compound
and non-compound names should land in the same order of magnitude. Cold
numbers need a page-cache drop between runs — the harness clears askld's
caches, not the OS's, so a warm 390 ms and a cold 17 s are the same defect.

## 2026-10-17 — the 11-second empty probe, and two gaps this baseline had

`mod("amdgpu") { macro { func } }` on the kernel index (5.3M symbols) took
11.4 s, and askld's own log put all of it in ONE probe:

```
probe completed elapsed_ms=21     rows=0     capped=false
probe completed elapsed_ms=87     rows=1001  capped=true
probe completed elapsed_ms=146    rows=1001  capped=true
probe completed elapsed_ms=11153  rows=0     capped=false     <- the whole query
```

Four roles in one concurrent wave; three fast, one eleven seconds, and the slow
one returned nothing. Captured mid-flight from `pg_stat_activity`, it was the
REFS-parents semi-join: `symbols.id IN (SELECT pd.symbol … WHERE si.id =
ANY($7))`.

**Two causes, each harmless alone.** A parameterised array hides its length —
measured on this DB, `id = ANY(ARRAY[…15 literals…])` estimates `rows=15` but
`id = ANY($1)` estimates `rows=10`, a hardcoded default. A role holds up to
~1000 ids (roles come from *resolved* neighbours, and Resolved means
`rows <= cap`), so the inner side looked 100x smaller than it was and the
planner drove the semi-join from the OUTER side: every symbol in the project,
since the only other predicates are `layer` and `project_name`. And `LIMIT
cap + 1` only rescues that plan when rows exist — the capped probes stopped at
once, which is why they were the fast ones; the empty one had to exhaust 9M
rows to prove there was nothing.

**Fixed in askl `0d2b713`** by emitting REFS roles as `MATERIALIZED` CTEs, as
the HAS roles already were (S6c/S7c above record that conversion). Note what
the CTE actually does, because it is not what I first wrote: S21's plan still
estimates the CTE body at 2,260,095 rows against 45,766 actual — 49x over. The
win is not a better estimate. `MATERIALIZED` is an optimisation fence: the role
is computed as its own node and the outer query hash-joins the real rows,
instead of the planner inlining the subquery and choosing to drive 9M rows
against it.

### Why this baseline did not catch it

S4/S5 measured REFS roles with the callers/callees of ONE function — a handful
of ids — and nothing here ever measured an empty answer. Both gaps are closed
by S14–S21, which are also the first shapes written against the engine's
*current* SQL: S1–S13 use the pre-containment `offset_range @>` model and a
`symbol_types.level` test, both gone since 211c12c. They stay as the record.

| # | role | form | cold |
|---|---|---|--:|
| S14 | EMPTY role (931 module ids — nothing refs a module), outer = all linux | IN-subquery | **16,226 ms** |
| S15 | same | MATERIALIZED CTE | 169 ms |
| S20 | NON-EMPTY cap-sized role (1000 amdgpu macro instances) | IN-subquery | **7,656 ms** |
| S21 | same | MATERIALIZED CTE | 303 ms |
| S14e/S16e | "forced empty" via `leaf_name = 'zzz'` | IN-subquery | <0.1 ms |

That last row is a caution, measured: `leaf_name = 'zzz'` is a selective indexed
predicate and short-circuits before the role matters. It does **not** exercise
the no-early-exit case. The pathological empty is an UNSELECTIVE outer side
disjoint from the role — S14 — and it costs more than the non-empty S20, which
is the whole point.

### Regression watch

`perf/probe_guard.py` asserts this from askld's own `probe completed` lines, per
probe, against the deployed server: no probe over 2000 ms, and the slowest
`rows=0` probe no more than 5x the slowest non-empty one. It refuses a false
green — a warm SQL cache serves every probe in ~0 ms, so if everything it sees
is under 5 ms it exits 2 and tells you to `docker compose restart askld`. On a
cold askld after `0d2b713`: 27 probes, worst 272 ms, the probe that was 11,153 ms
now 157 ms. `queries.txt` carries the two amdgpu queries for `run.py`.

The guard is end-to-end on purpose. S4 was correct SQL measured with the wrong
input size; a hand-copied shape can drift (S1–S13 did). The engine's own log
line cannot.
