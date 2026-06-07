-- Ephemeral layers: content-addressed DAG of per-request symbol/instance/ref
-- overlays on the persistent index.  Layers are written once, queried by
-- including their id in eph_ids, and garbage-collected on TTL.  Each layer's
-- rows live in the normal symbols/instances/refs tables, distinguished by
-- their `eph_layer` column (NULL for persistent rows).

-- Widen instance/ref id columns to BIGINT so ephemeral sequences can run in
-- the full BIGINT negative space without cycling.
ALTER TABLE index.symbol_instances ALTER COLUMN id TYPE BIGINT;
ALTER TABLE index.symbol_refs      ALTER COLUMN id TYPE BIGINT;

-- Catalogue of layers.  Hash uniqueness is the cache key; parent_id chains
-- layers into a DAG so a layer can be derived from prior layers.
--
-- Note: `parent_id` is set only on first insert.  When a second request
-- with a different parent chain hits the cache (ON CONFLICT (hash) DO
-- UPDATE), the row keeps the original creator's parent_id; only
-- `last_used` is touched.  Do not read `parent_id` to infer the current
-- request's ancestry.
CREATE TABLE index.eph_layers (
    id         BIGINT PRIMARY KEY GENERATED ALWAYS AS IDENTITY,
    parent_id  BIGINT REFERENCES index.eph_layers(id) ON DELETE CASCADE,
    hash       BYTEA NOT NULL,
    kind       TEXT  NOT NULL,
    last_used  TIMESTAMPTZ NOT NULL DEFAULT now()
);
CREATE UNIQUE INDEX eph_layers_hash_idx ON index.eph_layers (hash);

-- Negative-value sequences for ephemeral IDs; never collide with positive
-- persistent IDs.  BIGINT, no MINVALUE, no CYCLE — long-running deployments
-- get the full 2^63 negative space.
CREATE SEQUENCE index.eph_symbol_id_seq   START -1 INCREMENT -1 NO MINVALUE NO CYCLE;
CREATE SEQUENCE index.eph_instance_id_seq AS BIGINT START -1 INCREMENT -1 NO MINVALUE NO CYCLE;
CREATE SEQUENCE index.eph_ref_id_seq      AS BIGINT START -1 INCREMENT -1 NO MINVALUE NO CYCLE;

-- Tag rows on the canonical tables with the layer they belong to.  Persistent
-- rows have eph_layer IS NULL; ephemeral rows reference an eph_layers id.
ALTER TABLE index.symbols          ADD COLUMN eph_layer BIGINT REFERENCES index.eph_layers(id) ON DELETE CASCADE;
ALTER TABLE index.symbol_instances ADD COLUMN eph_layer BIGINT REFERENCES index.eph_layers(id) ON DELETE CASCADE;
ALTER TABLE index.symbol_refs      ADD COLUMN eph_layer BIGINT REFERENCES index.eph_layers(id) ON DELETE CASCADE;

-- Persistent rows MUST use the positive-id sequences and negative ids MUST
-- have a non-NULL eph_layer.  Without this, an orphan ephemeral row (eg.
-- from a previous buggy code path or manual UPDATE) silently re-surfaces in
-- query results as if it were persistent.  The canary is the documented
-- exception: id -999999 with eph_layer -999999 is fine, and id 0 is also
-- excluded because some tables use SERIAL starting at 1 (no row at id=0).
ALTER TABLE index.symbols
    ADD CONSTRAINT symbols_eph_id_sign_check
    CHECK (id > 0 OR eph_layer IS NOT NULL);
ALTER TABLE index.symbol_instances
    ADD CONSTRAINT symbol_instances_eph_id_sign_check
    CHECK (id > 0 OR eph_layer IS NOT NULL);
ALTER TABLE index.symbol_refs
    ADD CONSTRAINT symbol_refs_eph_id_sign_check
    CHECK (id > 0 OR eph_layer IS NOT NULL);

-- Partial indexes: only ephemeral rows are indexed by eph_layer.  Persistent
-- queries don't touch these.
CREATE INDEX symbols_eph_layer_idx           ON index.symbols          (eph_layer) WHERE eph_layer IS NOT NULL;
CREATE INDEX symbol_instances_eph_layer_idx  ON index.symbol_instances (eph_layer) WHERE eph_layer IS NOT NULL;
CREATE INDEX symbol_refs_eph_layer_idx       ON index.symbol_refs      (eph_layer) WHERE eph_layer IS NOT NULL;

-- Split the existing blanket UNIQUE constraints so ephemeral rows can coexist
-- with persistent ones at the same (symbol, object_id, offset_range) /
-- (to_symbol, from_object, from_offset_range).  Persistent uniqueness stays
-- as before; ephemeral uniqueness includes eph_layer in the key.
ALTER TABLE index.symbol_instances
    DROP CONSTRAINT IF EXISTS symbol_instances_symbol_object_id_offset_range_key;
CREATE UNIQUE INDEX symbol_instances_persistent_uq
    ON index.symbol_instances (symbol, object_id, offset_range)
    WHERE eph_layer IS NULL;
CREATE UNIQUE INDEX symbol_instances_eph_uq
    ON index.symbol_instances (symbol, object_id, offset_range, eph_layer)
    WHERE eph_layer IS NOT NULL;

ALTER TABLE index.symbol_refs
    DROP CONSTRAINT IF EXISTS symbol_refs_to_symbol_from_object_from_offset_range_key;
CREATE UNIQUE INDEX symbol_refs_persistent_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range)
    WHERE eph_layer IS NULL;
CREATE UNIQUE INDEX symbol_refs_eph_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, eph_layer)
    WHERE eph_layer IS NOT NULL;

-- Canary layer: a well-known layer (id -999999) with dummy rows that should
-- never appear in query results.  If they do, the eph_layer filter was
-- bypassed — a data-isolation violation.  Self-contained: own project,
-- object, symbol, instance, all with id -999999.
INSERT INTO index.eph_layers (id, parent_id, hash, kind, last_used)
OVERRIDING SYSTEM VALUE
VALUES (-999999, NULL, '\x0063616e617279', 'canary', '2000-01-01')
ON CONFLICT DO NOTHING;

INSERT INTO index.projects (id, project_name, root_path)
VALUES (-999999, '__canary__', '/__canary__')
ON CONFLICT DO NOTHING;

INSERT INTO index.objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (-999999, -999999, '', '/__canary__', 'canary', '')
ON CONFLICT DO NOTHING;

INSERT INTO index.symbols (id, name, symbol_path, project_id, symbol_type, leaf_name, eph_layer)
VALUES (-999999, '__canary__', 'canary', -999999, 1, '__canary__', -999999)
ON CONFLICT DO NOTHING;

INSERT INTO index.symbol_instances (id, symbol, object_id, offset_range, instance_type, eph_layer)
VALUES (-999999, -999999, -999999, int4range(0, 1), 1, -999999)
ON CONFLICT DO NOTHING;
