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
