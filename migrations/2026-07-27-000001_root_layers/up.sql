-- Root layers: every project owns a root layer and every row in the data
-- tables belongs to an explicit layer.  This removes the "persistent means
-- eph_layer IS NULL" sentinel: persistent rows now reference their project's
-- root layer, and visibility everywhere becomes `eph_layer = ANY($set)` where
-- the set always contains the visible roots.
--
-- Layer-id sign convention (mirrors the negative eph row-id sequences):
--   id > 0  — root layer (persistent, project-owned, never garbage-collected)
--   id < 0  — ephemeral layer (cache entry, TTL-collected) and the canary
-- Enforced row-locally on the data tables by CHECK ((id > 0) = (eph_layer > 0)):
-- positive (persistent) rows live in root layers, negative rows in ephemeral
-- layers.  The canary (row id -999999 in layer -999999) satisfies this with no
-- special case.
--
-- The root's hash is 32 random bytes for now — unguessable and collision-free
-- across environments.  When project versioning lands, it becomes the slot for
-- a content/version digest, making the whole layer chain content-addressed
-- (every layer already folds its parent hash).
--
-- Rollback caveat: once this migration commits, an OLD askld binary's
-- `eph_layer IS NULL` visibility matches zero rows — queries silently return
-- empty results (uploads fail loudly on NOT NULL).  Rolling back to an old
-- image therefore requires running down.sql against the database first.
--
-- Performance shape: the backfill rewrites every row of the three data
-- tables.  With the ~20 secondary indexes live (three GiST, one trigram GIN
-- among them) each rewritten row is re-inserted into every index, which put
-- a rehearsal run far beyond 20 minutes.  So the migration drops ALL
-- secondary indexes on the data tables first, backfills against bare heaps +
-- PKs, and rebuilds the full index set afterwards (B-trees build in
-- parallel; the GiST/GIN rebuilds dominate at a few minutes each).

-- 1. Purge cached ephemeral layers.  They are cache (base hashes change
--    meaning under root salting anyway); their data rows go via ON DELETE
--    CASCADE.  After this, eph_layer IS NULL <=> persistent row, and the only
--    surviving layer is the canary.
DELETE FROM index.eph_layers WHERE kind <> 'canary';

-- 2. Positive sequence for root-layer ids.  Continue above the historical
--    identity maximum so purged cache-layer ids are never reused.
CREATE SEQUENCE index.root_layer_id_seq AS BIGINT INCREMENT 1 NO CYCLE;
SELECT setval('index.root_layer_id_seq',
              (SELECT last_value + 1 FROM index.eph_layers_id_seq), false);

-- 3. Flip the identity sequence for ephemeral layers to the negative space,
--    starting below the canary (-999999) so they can never collide with it.
ALTER TABLE index.eph_layers ALTER COLUMN id SET MINVALUE -9223372036854775808;
ALTER TABLE index.eph_layers ALTER COLUMN id SET INCREMENT BY -1;
-- START must move below MAXVALUE before MAXVALUE can shrink (Postgres
-- validates START against the bounds on every ALTER).
ALTER TABLE index.eph_layers ALTER COLUMN id SET START WITH -1000000;
ALTER TABLE index.eph_layers ALTER COLUMN id RESTART WITH -1000000;
ALTER TABLE index.eph_layers ALTER COLUMN id SET MAXVALUE -1000000;

-- 4. One root layer per project (including the canary project — uniform; its
--    root simply has no rows).  populated = TRUE: roots never go through the
--    2-phase create_eph_layer upsert path.
ALTER TABLE index.projects ADD COLUMN root_layer_id BIGINT;

CREATE TEMP TABLE project_roots ON COMMIT DROP AS
SELECT p.id AS project_id,
       nextval('index.root_layer_id_seq') AS root_id,
       decode(replace(gen_random_uuid()::text || gen_random_uuid()::text, '-', ''), 'hex') AS root_hash
FROM index.projects p;

INSERT INTO index.eph_layers (id, parent_id, hash, kind, last_used, populated)
OVERRIDING SYSTEM VALUE
SELECT root_id, NULL, root_hash, 'root', now(), TRUE
FROM project_roots;

UPDATE index.projects p
SET root_layer_id = r.root_id
FROM project_roots r
WHERE p.id = r.project_id;

-- No ON DELETE CASCADE: the FK deliberately blocks deleting a root layer
-- while its project exists — hard protection against a buggy purge cascading
-- the persistent index away.  delete_project removes the project row first,
-- then the root layer.
ALTER TABLE index.projects
    ALTER COLUMN root_layer_id SET NOT NULL,
    ADD CONSTRAINT projects_root_layer_id_uq UNIQUE (root_layer_id),
    ADD CONSTRAINT projects_root_layer_id_fkey
        FOREIGN KEY (root_layer_id) REFERENCES index.eph_layers(id);

-- 5. Drop every secondary index on the data tables before the backfill (see
--    the performance note above).  PKs stay: FK targets need them and their
--    maintenance is the cheap part.  The old partial eph indexes and split
--    persistent/eph UNIQUEs are dropped for good — their replacements are
--    built in step 8.
DROP INDEX index.symbols_eph_layer_idx;
DROP INDEX index.symbols_name_trgm_idx;
DROP INDEX index.symbols_project_leafname_idx;
DROP INDEX index.symbols_project_name_idx;
DROP INDEX index.symbols_project_path_gist_idx;
DROP INDEX index.symbols_project_type_leafname_idx;
DROP INDEX index.symbols_project_type_name_idx;
DROP INDEX index.symbols_project_type_nlevel_name_idx;
DROP INDEX index.symbols_type_idx;

DROP INDEX index.symbol_instances_eph_layer_idx;
DROP INDEX index.symbol_instances_eph_uq;
DROP INDEX index.symbol_instances_persistent_uq;
DROP INDEX index.symbol_instances_instance_type_idx;
DROP INDEX index.symbol_instances_object_id_idx;
DROP INDEX index.symbol_instances_object_offset_gist_idx;
DROP INDEX index.symbol_instances_offset_range_idx;
DROP INDEX index.symbol_instances_symbol_idx;

DROP INDEX index.symbol_refs_eph_layer_idx;
DROP INDEX index.symbol_refs_eph_uq;
DROP INDEX index.symbol_refs_persistent_uq;
DROP INDEX index.symbol_refs_from_object_idx;
DROP INDEX index.symbol_refs_to_symbol_idx;

-- 6. Backfill: tag every persistent row with its project's root layer.
--    Canary rows already carry eph_layer = -999999 and are untouched.
UPDATE index.symbols s
SET eph_layer = p.root_layer_id
FROM index.projects p
WHERE s.project_id = p.id AND s.eph_layer IS NULL;

UPDATE index.symbol_instances si
SET eph_layer = p.root_layer_id
FROM index.objects o
JOIN index.projects p ON p.id = o.project_id
WHERE si.object_id = o.id AND si.eph_layer IS NULL;

UPDATE index.symbol_refs sr
SET eph_layer = p.root_layer_id
FROM index.objects o
JOIN index.projects p ON p.id = o.project_id
WHERE sr.from_object = o.id AND sr.eph_layer IS NULL;

-- 7. Every row now belongs to a layer, and the old sign checks (tautological
--    under NOT NULL) are replaced with the row-local form of "persistent
--    rows live in root layers": row-id sign equals layer-id sign.  Zero
--    storage, no triggers.  One ALTER per table on purpose: ALTER TABLE
--    validates all subcommands of a single statement in one full-table
--    pass, so this costs three scans instead of six inside the lock window
--    (DROP CONSTRAINT is metadata-only).
ALTER TABLE index.symbols
    ALTER COLUMN eph_layer SET NOT NULL,
    DROP CONSTRAINT symbols_eph_id_sign_check,
    ADD CONSTRAINT symbols_layer_sign_check CHECK ((id > 0) = (eph_layer > 0));
ALTER TABLE index.symbol_instances
    ALTER COLUMN eph_layer SET NOT NULL,
    DROP CONSTRAINT symbol_instances_eph_id_sign_check,
    ADD CONSTRAINT symbol_instances_layer_sign_check CHECK ((id > 0) = (eph_layer > 0));
ALTER TABLE index.symbol_refs
    ALTER COLUMN eph_layer SET NOT NULL,
    DROP CONSTRAINT symbol_refs_eph_id_sign_check,
    ADD CONSTRAINT symbol_refs_layer_sign_check CHECK ((id > 0) = (eph_layer > 0));

-- 8. Rebuild the index set.  Unchanged indexes come back with their original
--    definitions; the eph_layer partials return as full B-trees (every row
--    matches now, and the populated column finally gets real planner
--    statistics); the split persistent/eph UNIQUEs merge into one total
--    UNIQUE including eph_layer (a project's persistent rows share one root
--    id, so per-project persistent uniqueness is preserved exactly).
SET LOCAL maintenance_work_mem = '1GB';
SET LOCAL max_parallel_maintenance_workers = 4;

CREATE INDEX symbols_eph_layer_idx ON index.symbols (eph_layer);
CREATE INDEX symbols_name_trgm_idx ON index.symbols USING gin (name gin_trgm_ops);
CREATE INDEX symbols_project_leafname_idx ON index.symbols (project_id, leaf_name);
CREATE INDEX symbols_project_name_idx ON index.symbols (project_id, name);
CREATE INDEX symbols_project_path_gist_idx ON index.symbols USING gist (project_id, symbol_path);
CREATE INDEX symbols_project_type_leafname_idx ON index.symbols (project_id, symbol_type, leaf_name);
CREATE INDEX symbols_project_type_name_idx ON index.symbols (project_id, symbol_type, name);
CREATE INDEX symbols_project_type_nlevel_name_idx
    ON index.symbols (project_id, symbol_type, nlevel(symbol_path), name);
CREATE INDEX symbols_type_idx ON index.symbols (symbol_type);

CREATE INDEX symbol_instances_eph_layer_idx ON index.symbol_instances (eph_layer);
CREATE UNIQUE INDEX symbol_instances_layer_uq
    ON index.symbol_instances (symbol, object_id, offset_range, eph_layer);
CREATE INDEX symbol_instances_instance_type_idx ON index.symbol_instances (instance_type);
CREATE INDEX symbol_instances_object_id_idx ON index.symbol_instances (object_id);
CREATE INDEX symbol_instances_object_offset_gist_idx
    ON index.symbol_instances USING gist (object_id, offset_range);
CREATE INDEX symbol_instances_offset_range_idx
    ON index.symbol_instances USING gist (offset_range);
CREATE INDEX symbol_instances_symbol_idx ON index.symbol_instances (symbol);

CREATE INDEX symbol_refs_eph_layer_idx ON index.symbol_refs (eph_layer);
CREATE UNIQUE INDEX symbol_refs_layer_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, eph_layer);
CREATE INDEX symbol_refs_from_object_idx ON index.symbol_refs (from_object);
CREATE INDEX symbol_refs_to_symbol_idx ON index.symbol_refs (to_symbol);

-- 9. Fresh statistics: eph_layer was all-NULL (no usable stats, wild row
--    estimates); it is now a fully populated, highly skewed column.
ANALYZE index.symbols;
ANALYZE index.symbol_instances;
ANALYZE index.symbol_refs;
