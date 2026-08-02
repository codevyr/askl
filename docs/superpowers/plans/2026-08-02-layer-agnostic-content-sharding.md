# Layer-Agnostic Content + Executor-Owned Sharding Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Make content (`objects`) join the layer-uniform model the rest of the schema already uses, move the base/supplement (root vs ephemeral) *sharding* decision out of the verb and into the executor, and delete `LayerSpec::persistent_only`.

**Architecture:** Today `search`/`loc` call `LayerSpec::persistent_only`, which declares "my result depends only on the root" (root-salted base key + a no-op, empty supplement). That is the executor's job and an invariant the verb can't know. We fix it in three moves: (R1) give `objects` a `layer` FK so content is layer-scopable like `symbols`/`symbol_instances`/`symbol_refs` already are; (R2) make the content scan visibility-parameterized (`o.layer = ANY($vis)` + the eph-branch guard), expressed as one `content_scan(vis)` closure; (R3) have the executor invoke that one closure as base (`root_only`) + supplement (`eph_touching`) — the write-side analog of the existing read-side `cached_load_partitioned` — and delete `persistent_only`.

**Tech Stack:** Rust, diesel + diesel-async (Postgres), diesel embedded migrations (`migrations/`, run at startup), bb8 pool. Build/test only via `( cd askl && devenv shell -- cargo … )` (pre-commit rustfmt hook).

## Global Constraints

- **Pure refactor — ZERO behavior change.** Every object is in its project's root layer today (`objects` has no eph rows), so the eph-content shard is always empty. Every task's acceptance is "existing behavior is byte-identical." Verified by: the existing `cargo test -p askld` / `-p index` suites stay green, AND live query results on the deployed `:3002` are byte-identical before/after (reuse the W4 result-set comparison: extract the `# Results` lines, `sort -u`, `diff` = 0).
- **Layer-id sign convention (do not break):** root/persistent layer ids are **positive**, ephemeral layer ids **negative**. Data tables enforce it row-locally with `CHECK ((id > 0) = (layer > 0))`. `objects` gets the same treatment.
- **`content_store` stays layer-less** — it is content-addressed (`content_hash` PK), shared across objects/projects. The *object* (the file entry) carries the layer; visibility is enforced through the object.
- **`layer{}` verb is out of scope** — its supplement is genuine eph-referencing *ops* (distinct rows per layer; real `supplement_extra`), not "the same query at eph visibility." Leave its `LayerSpec` path untouched. Only `search`/`loc` unify onto `content_scan(vis)`.
- **One commit.** The user squashes feature branches to a single commit and force-pushes manually. Run `cargo test` at each task boundary; make ONE commit at the end (Task 6). Do not commit mid-plan.
- **Migration + reindex are deploy-side.** The implementer writes/tests the migration on a scratch DB; the user runs it on deploy. Do NOT run migrations against the deployed DB.
- **Out of scope (the future feature, not this plan):** actually creating ephemeral content objects, and unifying the "two `search:foo` symbols across base+supplement shards" that only arises once eph content exists.

---

## File Structure

| File | Responsibility | Change |
|---|---|---|
| `migrations/2026-08-02-000001_objects_layer/{up,down}.sql` | DB schema for `objects.layer` | **Create** |
| `index/src/schema_diesel.rs` | diesel table! for `objects` | Add `layer -> BigInt` |
| `index/src/models_diesel.rs` | `Object` struct (`:33`) | Add `pub layer: i64` |
| `askld/src/index_store/mod.rs` | `NewObject` (`:173`) | Add `layer: i64` |
| `askld/src/index_store/upload.rs` | `build_objects` (~589), `insert_objects` (~645) | Set/insert `layer` |
| `index/src/db_diesel/selection.rs` | `has_eph_leak` | Extend to `object.layer` **iff** objects surface in checked results |
| `index/src/db_diesel/index_impl.rs` | `build_search_sql` (~2865), `search_content_matches_on` (~2950), new `with_layer_sharded_scan` (near `with_partitioned_layers` `:2343`) | Visibility-param scan + executor sharding |
| `askld/src/verb/generic/search.rs` | `SearchSelector::layer_spec` (~178-302) | One `content_scan(vis)`; drop `persistent_only` |
| `askld/src/verb/generic/loc.rs` | `LocSelector::layer_spec` (~140-310) | One `content_scan(vis)`; drop `persistent_only` |
| `askld/src/verb/mod.rs` | `LayerSpec` (`:73-122`) | Delete `persistent_only`; add scan-based constructor if useful |
| `askld/src/command.rs` | test at `:870` (`empty_part`) | Rebuild the test spec without `persistent_only` |

---

## Task 1: R1 — `objects.layer` migration (DB)

**Files:**
- Create: `migrations/2026-08-02-000001_objects_layer/up.sql`
- Create: `migrations/2026-08-02-000001_objects_layer/down.sql`

**Interfaces:**
- Produces: an `index.objects.layer BIGINT NOT NULL` FK → `index.eph_layers(id)`, backfilled to each object's project root, with `CHECK ((id > 0) = (layer > 0))`.

**Model on:** `migrations/2026-07-27-000001_root_layers/up.sql` (added `layer` to the three data tables). Note that migration's performance lesson: the backfill rewrites every row, so it drops secondary indexes first and rebuilds them after. `objects` is smaller and lighter-indexed than `symbols`, but follow the same shape if the rehearsal is slow.

- [ ] **Step 1: Enumerate current `objects` secondary indexes** (needed for the drop/rebuild). On a scratch DB with the schema migrated to HEAD:

```bash
( cd /home/mplaneta/research/projects/ask && docker compose -f compose/compose.yaml exec -T db \
  psql -U postgres -d askl -c "\d index.objects" )
```
Record the non-PK indexes (expect `objects_filesystem_path_trgm`, a `(project_id, filesystem_path)` UNIQUE, possibly a `content_hash` index). These reappear in Step 3's rebuild block.

- [ ] **Step 2: Write `up.sql`**

```sql
-- Give every object an explicit layer, mirroring symbols/instances/refs
-- (2026-07-27 root_layers).  Content objects were implicitly root-only;
-- now they are layer-scopable so the executor can shard the content scan.
-- content_store stays layer-less (content-addressed, shared); the object
-- carries the layer.

-- 1. Nullable add (metadata-only, no rewrite).
ALTER TABLE index.objects ADD COLUMN layer BIGINT;

-- 2. Drop secondary indexes so the backfill rewrites against a bare heap
--    (see 2026-07-27 root_layers perf note).  PK stays.  <<REPLACE with the
--    exact list from Step 1>>
DROP INDEX index.objects_filesystem_path_trgm_idx;
-- DROP INDEX index.objects_project_filesystem_path_uq;   -- if present
-- DROP INDEX index.objects_content_hash_idx;             -- if present

-- 3. Backfill: every object belongs to its project's root layer.
UPDATE index.objects o
SET layer = p.root_layer_id
FROM index.projects p
WHERE o.project_id = p.id;

-- 4. NOT NULL + FK + sign check (one ALTER = one validating scan).
ALTER TABLE index.objects
    ALTER COLUMN layer SET NOT NULL,
    ADD CONSTRAINT objects_layer_fkey
        FOREIGN KEY (layer) REFERENCES index.eph_layers(id),
    ADD CONSTRAINT objects_layer_sign_check CHECK ((id > 0) = (layer > 0));

-- 5. Rebuild the secondary indexes + a layer index for scan scoping.
SET LOCAL maintenance_work_mem = '1GB';
SET LOCAL max_parallel_maintenance_workers = 4;
CREATE INDEX objects_layer_idx ON index.objects (layer);
-- <<REBUILD the exact indexes dropped in step 2 with their original defs>>
CREATE INDEX objects_filesystem_path_trgm_idx
    ON index.objects USING gin (filesystem_path gin_trgm_ops);

ANALYZE index.objects;
```

- [ ] **Step 3: Write `down.sql`**

```sql
DROP INDEX IF EXISTS index.objects_layer_idx;
ALTER TABLE index.objects
    DROP CONSTRAINT IF EXISTS objects_layer_sign_check,
    DROP CONSTRAINT IF EXISTS objects_layer_fkey,
    DROP COLUMN IF EXISTS layer;
-- (secondary indexes on other columns are unaffected)
```

- [ ] **Step 4: Verify migration up+down on a scratch DB**

Run against a throwaway DB (NOT the deployed one). Confirm `up` applies, `\d index.objects` shows `layer bigint not null` + the FK/check, every row's `layer` equals its project `root_layer_id`, and `down` cleanly removes it:

```bash
# up runs at startup / via diesel; then:
( cd /home/mplaneta/research/projects/ask && docker compose -f compose/compose.yaml exec -T db psql -U postgres -d <scratch> -c \
  "SELECT count(*) AS mismatched FROM index.objects o JOIN index.projects p ON p.id=o.project_id WHERE o.layer <> p.root_layer_id;" )
# expect: mismatched = 0
```

## Task 2: R1 — `objects.layer` in schema, model, and indexer

**Files:**
- Modify: `index/src/schema_diesel.rs` (objects `table!`)
- Modify: `index/src/models_diesel.rs:33` (`Object`)
- Modify: `askld/src/index_store/mod.rs:173` (`NewObject`)
- Modify: `askld/src/index_store/upload.rs` (`build_objects` ~589, `insert_objects` ~645)

**Interfaces:**
- Consumes: Task 1's `objects.layer` column.
- Produces: `Object { …, layer: i64 }`, `NewObject { …, layer: i64 }`; the indexer writes `layer = root_layer_id`.

- [ ] **Step 1: Add `layer` to the diesel schema.** In `schema_diesel.rs`, the `index.objects (id)` block — add `layer -> BigInt,` (place after `content_hash`, matching the ordering `symbol_instances` uses for its `layer`).

- [ ] **Step 2: Add `layer` to the `Object` model** (`models_diesel.rs:33`): add `pub layer: i64,` after `content_hash` (mirrors `SymbolInstance`/`Symbol` which already carry `pub layer: i64`).

- [ ] **Step 3: Add `layer` to `NewObject`** (`index_store/mod.rs:173`): add `layer: i64,`.

- [ ] **Step 4: Thread `root_layer_id` into `build_objects`** (`upload.rs` ~589). `build_symbols`/`build_symbol_instances` already take and set `root_layer_id` — follow them exactly: add the `root_layer_id: i64` parameter, set `layer: root_layer_id` on each `NewObject`, and pass it at the call site in `upload_object_chunk` (fetch `root_layer_id` the same way `upload_symbol_chunk` does at `:111`).

- [ ] **Step 5: Include `layer` in `insert_objects`** (`upload.rs` ~645): add `layer` to the INSERT column list and the upsert `SET` (if the `ON CONFLICT (project_id, filesystem_path) DO UPDATE` should refresh it — it should, so a re-upload keeps the object in the root layer).

- [ ] **Step 6: Test — objects get the root layer.** Add/extend an upload test asserting a freshly-built object carries `layer == root_layer_id`. If `build_objects` has no unit test, add one mirroring the `build_symbols` test; otherwise assert in the existing upload integration test.

```rust
// e.g. in upload.rs #[cfg(test)]
let objs = build_objects(project_id, root_layer_id, &sample_objects);
assert!(objs.iter().all(|o| o.layer == root_layer_id));
```

- [ ] **Step 7: Run tests**

Run: `( cd askl && devenv shell -- cargo test -p index -p askld )`
Expected: PASS (migration applied to the test DB; new field compiles; upload test green).

## Task 3: R1 — leak detection for `object.layer`

**Files:**
- Modify (maybe): `index/src/db_diesel/selection.rs` (`has_eph_leak`)

**Interfaces:**
- Consumes: `objects.layer`.

`has_eph_leak` (`selection.rs` ~340) verifies every result row's `layer ∈ visible_ids`. It currently checks `symbol.layer`, `symbol_instance.layer`, `symbol_ref.layer`.

- [ ] **Step 1: Determine whether objects surface in the checked `Selection`.** Read the node/reference structs `has_eph_leak` iterates. If they carry an object with a `layer`, an ephemeral object could leak; if objects are only referenced by id (no layer in the result set), this is a **no-op** today.

- [ ] **Step 2a (if objects carry a layer):** add a parallel `is_eph_leak(object.layer, visible_ids)` check, mirroring the existing three, plus a unit test that an object whose `layer ∉ visible_ids` triggers the leak error.

- [ ] **Step 2b (if not):** add a one-line comment at `has_eph_leak` noting objects aren't in the checked set (visibility is enforced at the content-scan `o.layer = ANY($vis)` in R2), so no object-layer leak check is needed here. No code change.

- [ ] **Step 3: Run tests** — `( cd askl && devenv shell -- cargo test -p index )`. Expected: PASS.

## Task 4: R2 — visibility-parameterized, layer-scoped content scan

**Files:**
- Modify: `index/src/db_diesel/index_impl.rs` — `build_search_sql` (~2865), `search_content_matches_on` (~2950)
- Modify: `askld/src/verb/generic/search.rs` — the base_populate closure (~228-286)
- Modify: `askld/src/verb/generic/loc.rs` — the base_populate closure (~144-301)

**Interfaces:**
- Produces: `search_content_matches_on(conn, query, case_sensitive, whole_word, composite_filter, limit, project_id, visible_layers: &[i64], eph_branch: bool)` — same results as today when `visible_layers = [root_layer_id]`, `eph_branch = false`. `build_search_sql(whole_word, case_sensitive, eph_branch)` renders `AND o.layer = ANY($vis)` and, when `eph_branch`, `AND o.layer < 0` (the disjointness guard — positive layers are roots and already belong to the persistent branch; see `EphVisibility::guard`).
- Produces (verb side): a single `content_scan(txn, root, visible_layers, eph_branch)` used to build the base_populate (root_only). Supplement still no-op in this task (that flips in Task 5).

- [ ] **Step 1: Add the layer bind to `build_search_sql`.** Add an `eph_branch: bool` param. In the `format!`, append to the WHERE: `" AND o.layer = ANY(${vis_slot})"` and, when `eph_branch`, `" AND o.layer < 0"`. Add a bind slot for the layer-id array. Keep PostgreSQL param-numbering exact (the file warns unreferenced params are rejected) — extend the bind chain in `search_content_matches_on` to match.

- [ ] **Step 2: Bind `visible_layers` in `search_content_matches_on`.** Add params `visible_layers: &[i64]` and `eph_branch: bool`; bind the array into the new slot; pass `eph_branch` to `build_search_sql`.

- [ ] **Step 3: Extract `content_scan` in `search.rs`.** Factor the base_populate body (the `search_content_matches_on` call + the eph symbol/instance materialization, `search.rs:228-284`) into a helper closure parameterized by `(visible_layers, eph_branch)`. Wire `base_populate` to call it with `visible_layers = eph.root_ids()`, `eph_branch = false`. Still return `LayerSpec::persistent_only(...)` for now.

- [ ] **Step 4: Same for `loc.rs`.** loc's populate (`loc.rs:144-301`) resolves a path→object→symbol. Scope its object lookup by `o.layer = ANY($vis)` the same way. **If loc's query shape doesn't fit the search helper, give loc its own vis-parameterized query — do not force search's substring shape.** Still `persistent_only` for now.

- [ ] **Step 5: Byte-identical check.** Rebuild; against the running index (local test DB or, after the user redeploys, `:3002`), confirm `search("drm_mm_init")`, `search("color") file("drm_mm.c")`, and a `loc(...)` return **identical** result sets to pre-R2 (extract `# Results`, `sort -u`, `diff` = 0). Reuse the W4 comparison harness.

- [ ] **Step 6: Run tests** — `( cd askl && devenv shell -- cargo test -p index -p askld )`. Expected: PASS (all existing search/loc tests unchanged, since `root_only` over all-root objects = today's scan).

## Task 5: R3 — executor owns the sharding; delete `persistent_only`

**Files:**
- Modify: `index/src/db_diesel/index_impl.rs` — add `with_layer_sharded_scan` near `with_partitioned_layers` (`:2343`)
- Modify: `askld/src/verb/generic/search.rs`, `askld/src/verb/generic/loc.rs` — provide the single scan; stop calling `persistent_only`
- Modify: `askld/src/verb/mod.rs` — delete `LayerSpec::persistent_only` (`:107-121`)
- Modify: `askld/src/command.rs:868-870` — rebuild the test spec without `persistent_only`

**Interfaces:**
- Consumes: Task 4's `content_scan(txn, root, visible_layers, eph_branch)`.
- Produces: `with_layer_sharded_scan(eph, base_hash, base_kind, scan)` where `scan: Fn(&mut EphTransaction, &RootLayer, visible_layers: &[i64], eph_branch: bool) -> EphScopedFut<bool>`. Internally it calls the existing `with_partitioned_layers` with `supplement_extra = &[]`, `base_populate = |txn,root| scan(txn, root, eph.root_ids(), false)` and `supplement_populate = |txn,root,_base_ref| scan(txn, root, eph.visible_ids(), true)`. **The base/supplement→visibility mapping lives HERE (the executor), not in the verb.**

- [ ] **Step 1: Add `with_layer_sharded_scan`.** Wrap `with_partitioned_layers`. The base run passes the root-only layer set + `eph_branch=false`; the supplement run passes the full visible set + `eph_branch=true`. **Impl risk to resolve here:** the closure lifetimes — `scan`'s returned `EphScopedFut<'b>` must not borrow a `vis` that drops at the closure boundary. Pass the layer-id `Vec<i64>` (owned, cloned from `eph` which is `'s`) and the `bool` by value into each populate closure so the future owns what it needs; build the SQL synchronously before the first await. (This mirrors how `with_partitioned_layers` already shares `base_populate`/`supplement_populate` by reference across the per-root fan-out.)

- [ ] **Step 2: Point `search.rs` at it.** Replace `Ok(Some(LayerSpec::persistent_only(hash, EphLayerKind::Search, base_populate)))` with a call path that hands `content_scan` to `with_layer_sharded_scan`. Depending on how `layer_spec`/`compute_selected` are wired, either (a) `LayerSpec` gains a `sharded_scan` constructor that stores the single scan and `compute_selected` routes it to `with_layer_sharded_scan`, or (b) `layer_spec` keeps producing base+supplement closures but both are derived from the one `content_scan` (base=root_only/false, supplement=eph_touching/true) with `supplement_extra = vec![]`. Prefer (a) so the executor owns the mapping; (b) is the fallback if the `LayerSpec` plumbing to `with_partitioned_layers` is hard to reshape. Either way, `persistent_only` is gone.

- [ ] **Step 3: Same for `loc.rs`** (`EphLayerKind::Loc`).

- [ ] **Step 4: Confirm the empty-supplement path is byte-identical to today.** Today (no eph objects) the supplement run scans `o.layer < 0` → zero rows → `search.rs:250` early-returns, no symbol/instances → empty supplement layer, exactly as the old no-op supplement produced. Add a debug log/assert during dev that the eph-branch match count is 0 on the current index, then remove it.

- [ ] **Step 5: Delete `LayerSpec::persistent_only`** (`verb/mod.rs:107-121`). Grep to confirm no remaining callers: `grep -rn persistent_only index/src askld/src` → only comments/tests.

- [ ] **Step 6: Fix the W4 test** (`command.rs:868-870`). `empty_part()` uses `LayerSpec::persistent_only`. Rebuild it directly (`LayerSpec { base_hash:[0;32], base_kind: EphLayerKind::Search, base_populate: noop_base(), supplement_populate: noop_supplement(), supplement_extra: Vec::new() }`) so `composite_of_empty_supplements_stays_empty` still exercises "all parts empty → composite empty."

- [ ] **Step 7: Run the full suite** — `( cd askl && devenv shell -- cargo test -p index -p askld )`. Expected: PASS (incl. `command::tests::composite_*`).

## Task 6: Verify end-to-end + single commit

- [ ] **Step 1: Grep-confirm the smell is gone.** `grep -rn "persistent_only\|reads persistent data only" index/src askld/src` → no constructor, no verb-level "reads persistent data only" claims (any remaining `persistent` refers only to the root layer / persistent index — legitimate).

- [ ] **Step 2: Live parity (after the user redeploys the branch).** On `:3002`, byte-identical result sets before/after for: a single `search`, the two-search union `search("A") search("B")` (W4), a chained `search("kmalloc") { }`, and a `loc(...)`. `diff` of `sort -u`'d `# Results` = 0 for each.

- [ ] **Step 3: Formatting + full build** — `( cd askl && devenv shell -- cargo fmt --all && cargo build -p askld )`.

- [ ] **Step 4: Single commit** (user squashes/force-pushes; make one commit for the whole architecture repair):

```bash
( cd /home/mplaneta/research/projects/ask/askl && devenv shell -- git add -A && git commit -m "$(cat <<'EOF'
Make content layer-uniform; executor owns layer-sharding

objects gain a layer FK (like symbols/instances/refs), so the content scan
is layer-scopable. search/loc express one visibility-parameterized
content_scan(vis); the executor (with_layer_sharded_scan) invokes it as
base (root_only) + supplement (eph_touching) — the write analog of
cached_load_partitioned. LayerSpec::persistent_only is deleted: verbs no
longer declare their layer-dependence. Pure refactor — the eph-content
shard is empty until content can live in ephemeral layers, so results are
byte-identical.

Co-Authored-By: Claude Opus 4.8 (1M context) <noreply@anthropic.com>
EOF
)" )
```

---

## Self-Review

- **Spec coverage:** R1 (Tasks 1-3: migration + schema/model/indexer + leak), R2 (Task 4: vis-parameterized scan), R3 (Task 5: executor sharding + delete `persistent_only`), verify+commit (Task 6). All roadmap items covered.
- **Decisions honored:** `layer{}` untouched (Task 5 only edits search/loc); `objects.layer` prerequisite first (Task 1); two-symbols/content-creation explicitly out of scope (Global Constraints).
- **Type consistency:** `content_scan(txn, root, visible_layers: &[i64], eph_branch: bool)` used identically in Tasks 4 and 5; `with_layer_sharded_scan(eph, base_hash, base_kind, scan)` maps base→(root_ids,false), supplement→(visible_ids,true).
- **Known impl risks (flagged, not hidden):** (a) Task 1 migration cost on a large `objects` table → drop/rebuild indexes like root_layers; (b) Task 4 exact PG bind-slot numbering; (c) Task 5 closure-lifetime plumbing for the single scan; (d) Task 4 loc may need its own query shape. Each has a concrete resolution in-task.
- **No behavior change is the acceptance test** — every task verifies against the existing suite + byte-identical live results.
