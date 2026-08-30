# What the `stage_read` budget gate costs (C1-P0), and what it costs now

> **Status.** The table below is the **pre-change baseline**, taken when *any*
> neighbour cleared the budget. The rule has since been narrowed to exclude
> echo edges (see "The echo rule"), and row 4 has been **re-measured after
> deploy** — it moved from *timeout at both limits* to **0.32-0.36 s** at
> `limit=10`. See "Row 4, after". Rows 1-3 are bare queries with no dependents,
> so the gate never applied to them and this change cannot have moved them.

`ResultBudget` pushes a `LIMIT cap × 8` into row-producing leaves so a bare
selector stops far short of materialising rows the renderer would discard.
`stage_read` (`askld/src/statement/mod.rs`) then clears it to `UNLIMITED` for
any statement whose rows another statement consumes, because a truncated,
order-arbitrary row set would be consumed by `constrain_by_*` as if it were the
whole answer.  *When these numbers were taken* that meant any statement with a
non-pre-seed dependency or dependent at all; it now excludes edges to an echo.

That safety is real. This is what it costs, measured against the deployed
compose stack (`localhost:3002`, server statement timeout 120s) before any
engine change. Each pair is the SAME query at two `limit` values, interleaved
and repeated so DB page-cache warming cannot explain the difference.

| query | shape | `limit=0` | `limit=10` |
|---|---|--:|--:|
| `project("linux") search("mmap_lock")` | bare (budget applies) | 159-255 ms | **36-66 ms** |
| `project("linux") g"*alloc*"` | bare (budget applies) | 79-83 s | **0.05-0.33 s** |
| `project("rdma-core") g"*alloc*"` | bare (budget applies) | 4.9-44 s | **0.06-1.7 s** |
| `project("rdma-core") g"*alloc*" { }` | composed (**gate clears it**) | timeout (120 s) | timeout (120 s) |

## Findings

1. **The budget is worth up to three orders of magnitude** on a wide leaf:
   `g"*alloc*"` over linux answers in 0.05 s bounded and 80 s unbounded, for
   24 892 symbols the caller never asked for.
2. **The gate turns that off for composed statements, completely.** Adding an
   empty scope — one dependent, nothing else changed — makes `limit=10` and
   `limit=0` indistinguishable: both hit the 120 s timeout. The caller's cap
   buys *nothing* the moment a statement acquires a neighbour, which in a
   nested query is nearly every statement.
3. So the gate is not a marginal cost to trade off. On wide leaves it is the
   difference between an answer and a timeout, and it fires exactly when a
   query stops being trivial.

## Method notes (read before re-measuring)

- **Interleave and repeat.** The first attempt measured `limit=10` at 4.2 s and
  `limit=0` at 0.26 s — the exact opposite of the truth. The bounded run was
  simply first, paying cold DB page-cache costs for a plan the unbounded run
  then reused. Alternating the two and repeating reversed the ordering.
- **Check for `# Error` before reading a latency.** An early composed
  measurement recorded a clean-looking 12 s that was really
  `could not resize shared memory segment … No space left on device` — the
  compose DB container's `/dev/shm` is too small for wide parallel scans, worth
  raising independently of this work.
- The composed timeouts above are genuine query timeouts, not shm failures.
  The `/dev/shm` shortfall noted above is now fixed in the compose repo
  (`shm_size: 1gb` on the `db` service). It needs a container **recreate**, not
  a restart, to take effect.

## The echo rule (what changed)

The gate was "any Parent/Child/User dependency or dependent clears the budget".
That is coarser than the hazard. The hazard is `constrain_*` retaining a
statement's nodes against a *neighbour's rows*: a neighbour that stopped at a
LIMIT would prune rows it never fetched. A neighbour that only **echoes** —
a weak unit whose whole subtree is weak, i.e. a bare `{}` — has no selection of
its own to lose. It derives from us and, being weak, may never narrow a
resolved neighbour. It cannot be hurt by a bound and cannot hide one.

So the law is now: **truncated rows may only travel where they cannot narrow an
independent selection.** `edge_consumes_rows` in `askld/src/statement/mod.rs`
decides it per edge; `Statement::notify` fences it, asking
`weak_notifier_blocks` rather than restating the weakness rule.
`PreSeedLabel` still always clears the budget — label resolution needs an
exact, order-stable id set for layer hashing, and that requirement does not go
away.

That is what row 4 of the table above was blocked on: in
`project("rdma-core") g"*alloc*" { }` **both** scopes are `Unscoped`, so
`find_symbol`'s own rule never objected — the coarse gate alone threw the bound
away.

## Why the gate is not retired

`find_symbol`'s scope rule does **not** subsume it. When a conditioned child
sits under an unconditioned wrapper, both scope builders return `Skip`
(deferred neighbourhood), nothing inside `find_symbol` objects to bounding the
child, and the wrapper still takes the child's rows through
`constrain_by_child`. `a_composed_child_keeps_its_rows_complete_under_a_budget`
is that case; disabling the gate makes it trip the fence and change its answer.

Retiring the gate outright therefore still needs what C1 always said it needed:
composition must flow *predicates* (or a probe's exact, uncapped id set)
instead of materialised rows. Until then the gate stays, narrowed.

Note also that almost no test in the suite sets a budget at all, so before the
tests named above the gate had **no coverage** — deleting it entirely left 648
tests green.

## Row 4, after

Deployed build, `ASKL_SQL_CACHE_BYTES=0` (no RAM-cache confound),
`ASKL_QUERY_TIMEOUT=120`. Five consecutive runs at `limit=10`, 3 s apart, then
two at `limit=0`:

| `project("rdma-core") g"*alloc*" { }` | before | after |
|---|---|---|
| `limit=0` | timeout (120 s) | timeout (120 s) |
| `limit=10` | timeout (120 s) | **0.32-0.36 s** |

Every `limit=10` run returned `10 of 16 symbols · 7 contains` and carried
`results were bounded to 80 rows by the server result budget`. That warning is
also the proof the change is live: it can only appear if the budget survived
the `{ }` dependent.

The composed case now separates the way bare rows 2 and 3 always did, and
finding 2 above — "the caller's cap buys *nothing* the moment a statement
acquires a neighbour" — no longer holds when that neighbour is an echo.

## Method note: an unbounded run poisons the next bounded one

Worth more than the numbers. Interleaving `limit=0` and `limit=10` in one
session is confounded: the unbounded run reads hundreds of MB and **evicts the
page cache the bounded run depends on**. Same query, same limit, measured in
two contexts:

| `project("rdma-core") g"*alloc*"` at `limit=10` | time |
|---|--:|
| after other bounded runs | 0.12 s |
| immediately after a `limit=0` run that hit the 120 s timeout | 45 s, then 103 s |

Three orders of magnitude. The obvious suspicion — queries surviving the client
timeout server-side — was **wrong**: `pg_stat_activity` was completely idle
straight afterwards, so askld does cancel them. It is the cache.

Consequences for anyone re-running this:
- **Never read a bounded latency that followed an unbounded scan.** Discard it.
- Row 4 at `limit=0` measured 40 s against a warm cache and timed out at 120 s
  against a cold one. Both are honest; neither is "the" number.
- Discard the first run of a session (the original note above, still true):
  row 1 at `limit=10` measured 4.06 s cold, 0.12-0.13 s thereafter.

## Re-measuring

Against the deployed stack, not an ad-hoc `askld serve`.

```
curl -s -m 200 -X POST \
  'localhost:3002/query?format=markdown&projection=names&limit=10' \
  --data-binary 'project("rdma-core") g"*alloc*" { }'
```

Check the body for `# Error` before believing any latency, and leave the DB
idle between runs — see the method note above.

---

## Follow-up: why 800 rows bit so early (the parents family's shape)

The measurement above says a bare wide leaf answers in 0.05 s bounded. It does
not explain why `"i915_ggtt"` — **one symbol** — hit the same 800-row bound.

`i915_ggtt` has 137 reference sites across 33 files. The parents query returned
one row per *(site × enclosing declaration × instance of the target symbol)*:

- **×3.71** — every declaration whose range contains the site: the calling
  function, its file, its directory, its module (41 sites have 3, 95 have 4,
  1 has 5). This is meaning — it is why files, dirs and modules appear as
  callers.
- **×5** — a `symbol_ref` names a *symbol*, and the query joined it to all five
  instances of `i915_ggtt`. This was duplication: `collect_ref_edges` dedups on
  `(from_instance, to_symbol, occurrence)`, so four of five rows died in Rust
  and the survivor was whichever the planner returned first.

137 × 3.71 × 5 = **2540 rows**, cut at 800 — about site 43 of 137 — then
discarded. The budget's `cap × 8` is tuned for `current` rows (instances per
result symbol, ~5 here); the family carried ~18.5 rows per reference.

Removing the ×5 (the target side is reconstructed from `current`, whose
instances are the query's own `source_ids`) gives, on the same neighbourhood
with the real visibility predicates, warm, three runs each:

| shape | rows | complete? | width | time |
|---|--:|:--:|--:|--:|
| old (fan-out, LIMIT 800) | 800 of 2540 | **no** | 267 | 7.7-8.2 ms |
| new (no fan-out) | 508 | yes | 86 | 3.7-6.0 ms |

`DISTINCT ON` was measured too (21.8 ms): it collapses the duplication but
sorts all 2540 rows, which defeats the `LIMIT`'s early exit.

The children query has the same fan-out on the callee side and is **not**
fixed: there the candidate instances are instances of the *callee*, which are
not in `current`, so the choice cannot be made at selection time — it belongs
at edge-collection time, where `all_nodes` is known.
