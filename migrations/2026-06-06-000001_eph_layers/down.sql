-- Drop canary rows.
DELETE FROM index.symbol_instances WHERE id = -999999;
DELETE FROM index.symbols          WHERE id = -999999;
DELETE FROM index.objects          WHERE id = -999999;
DELETE FROM index.projects         WHERE id = -999999;
DELETE FROM index.eph_layers       WHERE id = -999999;

-- Restore blanket UNIQUE constraints on symbol_instances / symbol_refs;
-- removing the split partial indexes will fail if ephemeral duplicates exist,
-- so this script is best run on an otherwise-empty ephemeral space.
DROP INDEX IF EXISTS index.symbol_instances_persistent_uq;
DROP INDEX IF EXISTS index.symbol_instances_eph_uq;
DROP INDEX IF EXISTS index.symbol_refs_persistent_uq;
DROP INDEX IF EXISTS index.symbol_refs_eph_uq;

ALTER TABLE index.symbol_instances
    ADD CONSTRAINT symbol_instances_symbol_object_id_offset_range_key
    UNIQUE (symbol, object_id, offset_range);
ALTER TABLE index.symbol_refs
    ADD CONSTRAINT symbol_refs_to_symbol_from_object_from_offset_range_key
    UNIQUE (to_symbol, from_object, from_offset_range);

-- Drop the negative-id sanity constraints.
ALTER TABLE index.symbol_refs      DROP CONSTRAINT IF EXISTS symbol_refs_eph_id_sign_check;
ALTER TABLE index.symbol_instances DROP CONSTRAINT IF EXISTS symbol_instances_eph_id_sign_check;
ALTER TABLE index.symbols          DROP CONSTRAINT IF EXISTS symbols_eph_id_sign_check;

-- Drop eph_layer columns and the eph_layers table.
ALTER TABLE index.symbol_refs      DROP COLUMN IF EXISTS eph_layer;
ALTER TABLE index.symbol_instances DROP COLUMN IF EXISTS eph_layer;
ALTER TABLE index.symbols          DROP COLUMN IF EXISTS eph_layer;

DROP SEQUENCE IF EXISTS index.eph_ref_id_seq;
DROP SEQUENCE IF EXISTS index.eph_instance_id_seq;
DROP SEQUENCE IF EXISTS index.eph_symbol_id_seq;

DROP TABLE IF EXISTS index.eph_layers;

-- Narrow id columns back to INTEGER.  Will fail if any row has an id outside
-- the i32 range; this is intentional — operators must clean those up first.
ALTER TABLE index.symbol_refs      ALTER COLUMN id TYPE INTEGER;
ALTER TABLE index.symbol_instances ALTER COLUMN id TYPE INTEGER;
