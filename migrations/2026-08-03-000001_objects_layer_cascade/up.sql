-- Add ON DELETE CASCADE to the objects.layer FK, matching
-- symbols/symbol_instances/symbol_refs.layer.  The 2026-08-02 migration that
-- introduced objects.layer omitted the cascade, so deleting an ephemeral layer
-- that carries content objects (the future content-in-layers feature) would
-- fail with a FK violation instead of removing those objects — silently
-- blocking TTL/GC purges of such layers.  Root layers are never deleted, so
-- this is dormant today; the cascade makes the behaviour correct once
-- ephemeral objects exist.

ALTER TABLE index.objects
    DROP CONSTRAINT objects_layer_fkey,
    ADD CONSTRAINT objects_layer_fkey
        FOREIGN KEY (layer) REFERENCES index.layers(id) ON DELETE CASCADE;
