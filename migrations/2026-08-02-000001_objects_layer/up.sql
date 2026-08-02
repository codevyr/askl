-- Add objects.layer: every object now carries an explicit layer reference,
-- mirroring symbols/symbol_instances/symbol_refs (2026-07-27 root_layers).
-- Content objects were implicitly root-only; now they are layer-scopable
-- so the executor can shard the content scan.
-- content_store stays layer-less (content-addressed, shared); the object
-- carries the layer.

-- 1. Nullable add (metadata-only, no rewrite).
ALTER TABLE index.objects ADD COLUMN layer BIGINT;

-- 2. Backfill: persistent objects (positive id) get their project's root layer.
UPDATE index.objects o
SET layer = p.root_layer_id
FROM index.projects p
WHERE o.project_id = p.id AND o.id > 0;

-- The canary object (id -999999) is self-contained in the canary layer
-- (-999999), matching the canary symbol/instance (see 2026-06-06 eph_layers).
-- The blanket root backfill would give it a positive layer and violate
-- the (id>0)=(layer>0) sign check.
UPDATE index.objects SET layer = -999999 WHERE id = -999999;

-- 3. NOT NULL + FK + sign check (one ALTER = one validating scan).
ALTER TABLE index.objects
    ALTER COLUMN layer SET NOT NULL,
    ADD CONSTRAINT objects_layer_fkey
        FOREIGN KEY (layer) REFERENCES index.layers(id),
    ADD CONSTRAINT objects_layer_sign_check CHECK ((id > 0) = (layer > 0));

-- 4. Layer index for scan scoping.
CREATE INDEX objects_layer_idx ON index.objects (layer);
