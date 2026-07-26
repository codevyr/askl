DROP INDEX IF EXISTS index.eph_layers_base_id_idx;
ALTER TABLE index.eph_layers DROP COLUMN IF EXISTS base_id;
