-- The layer table holds ALL layers now — roots (persistent, project-owned)
-- and ephemeral cache layers (negative ids, TTL-collected).  "Ephemeral" is
-- a kind-level property, not a table-level one, so the container names drop
-- the eph_ prefix.  Metadata-only: renames touch no data.
ALTER TABLE index.eph_layers RENAME TO layers;
ALTER TABLE index.symbols RENAME COLUMN eph_layer TO layer;
ALTER TABLE index.symbol_instances RENAME COLUMN eph_layer TO layer;
ALTER TABLE index.symbol_refs RENAME COLUMN eph_layer TO layer;
