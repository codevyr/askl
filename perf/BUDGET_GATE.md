# What the `stage_read` budget gate costs (C1-P0)

`ResultBudget` pushes a `LIMIT cap × 8` into row-producing leaves so a bare
selector stops far short of materialising rows the renderer would discard.
`stage_read` (`askld/src/statement/mod.rs`) then clears it to `UNLIMITED` for
any statement with a non-pre-seed dependency or dependent, because a truncated,
order-arbitrary row set would be consumed by `constrain_by_*` as if it were the
whole answer.

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

## What this gates

C1 proceeds: composition must flow *predicates* (or a probe's exact, uncapped
id set) instead of materialised rows, after which the gate can be dropped for
everything except `DependencyRole::PreSeedLabel` — label resolution needs an
exact, order-stable id set for layer hashing, and that requirement does not go
away.

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
