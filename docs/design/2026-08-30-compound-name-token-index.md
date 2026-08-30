# Compound-name lookups: a token-array index for the unanchored lquery

**Status:** design, approved 2026-08-30. Not implemented.
**Scope:** one migration + one filter mixin. No semantic change.

## Problem

`func {field "qaic_bo.dbc"}` takes **17.3 s** on local staging. The askld log
attributes all of it to a single SQL:

```
probe completed elapsed_ms=17279 rows=1 capped=false   <- the whole 17 s
probe completed elapsed_ms=8    rows=0
probe completed elapsed_ms=11   rows=9
select_* / find_*                elapsed_ms=1..9
```

That probe resolves `field "qaic_bo.dbc"`, and its only indexable predicate is

```sql
symbols.symbol_path ~ '*.qaic_bo.*.dbc.*'::lquery
```

`EXPLAIN (ANALYZE, BUFFERS)` shows an `Index Scan using
symbols_project_path_gist_idx` reading **79,434 pages (~620 MB) to return 1
row**. `symbol_type = 8` and the layer predicate are applied as post-index
Filters and prune nothing.

Two independent facts make this the whole story:

1. **A leading-`*` lquery cannot prune a GiST ltree index.** There is no
   prefix to descend on, so the scan degenerates into a full walk of the
   1.5 GB `symbols_project_path_gist_idx`.
2. **`shared_buffers` is 128 MB against a 24 GB database.** A 620 MB walk is
   served from the OS page cache at best — 390 ms warm, 17.3 s cold. The 45x
   warm/cold spread is the page cache, not the plan.

Planner statistics were fresh at the time of the report (analyzed 06:07, query
at 08:45), so this is not a recurrence of the stale-statistics incident that
`2026-08-23-000001_planner_stats_maintenance` addresses.

### Why the query shape reaches that predicate

`name_filter_leaf` (`askld/src/verb/generic/selectors.rs:489`) branches on
whether the name contains `.`, `/` or `:`:

- **simple name** -> `LeafNameMixin` -> `leaf_name = 'x'`, a selective btree
  equality. This is the fast path, and it is the only path the perf corpus
  exercised.
- **compound name** -> `CompoundNameMixin::new` (unanchored, because
  `leaf_anchored` defaults to `!dot_is_separator(type)` = false for code
  symbols) -> **only** the lquery. `current_expr` contributes `leaf_name`
  solely when `leaf_token` is `Some`, which only the leaf-anchored constructor
  sets.

`build_lquery` always prefixes `*.`, which is what expresses the documented
ordered-subset semantics ("`cli.Run` matches symbols containing both `cli` and
`Run` tokens in order", `docs/syntax.md`).

**Reachable slow shapes** (both have >= 2 tokens):

- a compound code-symbol name, which is unanchored by default —
  `field "qaic_bo.dbc"`;
- a compound `file`/`dir` name with `match="contains"` —
  `dir("pkg/kueue", match="contains")`.

A single-token name always takes `LeafNameMixin`, and `match="contains"` does
not change that (verified against the running server). Single-label lqueries
such as `*.dbc.*` are therefore index probes, not reachable queries.

## Design

### 1. Migration — a GIN expression index

```sql
CREATE INDEX symbols_path_tokens_gin
  ON index.symbols USING gin (string_to_array(symbol_path::text, '.'));
```

`string_to_array`, the ltree->text cast and `text` are all `IMMUTABLE`, so an
**expression index suffices — no generated column, no table rewrite, no change
to the ingest path.**

Measured on staging (5.9M symbols): **480 MB**, 58 s to build `CONCURRENTLY`.

Built **non-`CONCURRENTLY`**, following the convention set by
`2026-07-29-000001_leafname_trgm`: diesel runs migrations in a transaction,
where `CONCURRENTLY` is not allowed. The build takes a ShareLock on
`index.symbols` and briefly blocks indexer writes at deploy. Comparable to the
existing 269 MB `symbols_leafname_trgm_idx` build.

### 2. Engine — one call site

`ltree_filter_sql` has exactly one caller: `CompoundNameMixin::current_expr`
(`index/src/db_diesel/mixins.rs:1015`). `CompoundNameMixin` gains the token
list alongside the lquery it already builds, and emits two predicates:

```sql
string_to_array(symbols.symbol_path::text,'.') @> ARRAY['qaic_bo','dbc']  -- access path
AND (symbols.symbol_path::text)::ltree ~ '*.qaic_bo.*.dbc.*'::lquery      -- decider
```

`hash_into` must fold in the tokens as well, so the filter hash keeps
identifying the predicate that is actually issued.

### 3. The `::text::ltree` wrap is load-bearing

**Adding the GIN predicate alone does not fix the problem.** Measured: the
planner `BitmapAnd`s the new index with the GiST one and still walks 116,529
pages (614 ms warm).

The cause is that GiST-ltree has no selectivity estimator — it reports
`rows=591` for *every* lquery regardless of the pattern (visible as the
constant `cost=0.00..84.8x rows=59x` across all shapes measured). Against that
fixed estimate a second bitmap always looks cheap, so the planner keeps
choosing it.

**Invariant: a predicate the planner cannot cost must not be offered as an
access path.** Wrapping the column in `::text::ltree` makes the expression
stop matching the indexed column, demoting the lquery to a recheck. Verified:
10 pages, 0.12 ms. (`symbol_path || ''::ltree` behaves identically; the cast
form is clearer about intent.)

### 4. The GiST index stays

`mixins.rs:1607` issues `'x'::ltree @> symbols.symbol_path`, a
constant-anchored containment that GiST prunes properly. This change is
therefore **+480 MB, not a swap**; `symbols_project_path_gist_idx` keeps its
1.5 GB.

## Measurements

Pages are the stable metric; timings are warm (page cache populated)
unless marked cold.

| shape | today | with fix |
|---|--:|--:|
| `field "qaic_bo.dbc"` probe (full 4-way join) | 79,456 pg / 390 ms warm, **17.3 s cold** | 19 pg / **0.4 ms** |
| compound, rare tokens (`*.dbc.*` probe) | 116,524 pg | 10 pg / 0.12 ms |
| compound, two *common* tokens (`*.linux.*.c.*`) | 2.8 ms | **28 ms — regression** |

The last row is the honest cost. When both tokens are very common the GIN
posting list holds ~192k entries, and today's plan happens to win: the planner
picks a seq scan that satisfies `LIMIT 1001` almost immediately. The trade is
a 28 ms worst case in exchange for a 17.3 s one, with the bad case bounded by
posting-list size rather than by index size.

## Soundness

**Structural.** `symbol_path ~ '*.a.*.b.*'` matches P iff `a` and `b` occur as
labels of P in that order. That implies `{a,b} subset labels(P)`, which is
exactly `string_to_array(P::text,'.') @> ARRAY['a','b']`. The pre-filter is a
superset; the lquery remains the decider, including for order and repetition.

**Empirical.** 12 high-frequency label pairs, 1.4M matched rows, **zero rows
dropped** by the pre-filter. A second run over 300 real (first, last) label
pairs from paths of depth 2-4 also dropped nothing, though that one is weak
evidence on its own — each pair matched a single row.

**Why the filter is on `symbol_path::text` and not on `name`.** Path labels are
not literal substrings of `name`: `index.symbol_name_to_ltree` strips
`* [ ] { } , @ - ( ) space`, then removes anything outside `[A-Za-z0-9_]` per
label, so `foo-bar.baz` is indexed as `foobar.baz`. **270,090 of 5.9M names
(4.6%)** contain such characters, and a `name LIKE '%a%b%'` pre-filter would
silently drop every one of them. `symbol_path::text` *is* the normalized
dot-joined form, so the labels are literally present, in order.

## Alternatives considered

**Add `leaf_name = <last token>` to the unanchored case.** Rejected: unsound.
Unanchored means the last token may sit at any depth — `qaic_bo.dbc.field`
must still match `"qaic_bo.dbc"`. **5.8% of paths (340,779) are deeper than
two levels**, so the last token is genuinely not the leaf for those.

**Trigram GIN on `symbol_path::text`** (`LIKE '%a%b%'`). Rejected: trigrams
need 3+ characters and **~9% of tokens are 1-2 characters**, where the
pre-filter degenerates to nothing. Same migration cost as the token array,
strictly weaker.

**Leaf-anchor compound code names by default.** Not chosen here, but recorded
because it remains attractive: it needs no migration, reaches 11 ms via the
existing `new_leaf_anchored` constructor, and collapses the documented
file/dir vs code asymmetry into a single law. Rejected for this change because
it alters query semantics that `docs/syntax.md` specifies. It stays available
as a follow-up if the asymmetry is judged worth removing on its own merits.

**Drop the GiST index.** Rejected: still required by the `'x'::ltree @>
symbols.symbol_path` containment at `mixins.rs:1607`.

**Restructure the probe into a MATERIALIZED CTE** (the `ProbeCtes` /
`has_role_cte_body` pattern). Works — measured 0.16 ms — but the `::text::ltree`
wrap achieves the same effect at one call site, without threading CTE
construction through every query that carries a compound name.

## Testing

1. **Differential soundness test** against a seeded index: for a set of label
   pairs, the result set with the pre-filter must equal the result set with
   the bare lquery. This is the test that would catch a future change to
   `symbol_name_to_ltree` normalization breaking the implication.
2. **Rust unit tests** on `CompoundNameMixin`: emitted SQL contains both
   predicates and the wrapped lquery form; `hash_into` distinguishes different
   token sets.
3. **Perf regression corpus** — `perf/queries.txt` already carries
   `field "qaic_bo.dbc"`, `"qaic_bo.dbc"` and `func "qaic_data.c"` next to the
   single-token control `type "drm_gem_object"`. The gate is the *gap*: both
   groups should land in the same order of magnitude.
4. **EXPLAIN baseline** — `perf/probe_baseline.sql` shapes S10-S12 re-run
   after the change; S10 must stop reading ~79k pages.

## Risks

- **Deploy cost.** A non-concurrent GIN build takes a ShareLock on
  `index.symbols` and blocks indexer writes for roughly a minute.
- **Common-token regression.** 2.8 ms -> 28 ms as measured above. Bounded, and
  worth watching if a future corpus adds such a query.
- **Silent-divergence hazard.** The pre-filter's soundness depends on labels
  being literal in `symbol_path::text`. Any future change to normalization
  must keep that true; test 1 above is the guard.
- **Disk.** +480 MB.

## Reproduction

All numbers above come from the compose staging DB (5.9M symbols / 14.8M
symbol_instances / 23.3M symbol_refs, `shared_buffers` 128 MB) via
`perf/probe_baseline.sql` and ad-hoc `EXPLAIN (ANALYZE, BUFFERS)`. The
prototype index was built and dropped; staging carries no leftover state.
