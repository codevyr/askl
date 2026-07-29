-- Revert root layers: restore the "persistent means eph_layer IS NULL"
-- sentinel.  Lossless for persistent data.
--
-- Ordering is critical: data rows must be detached from root layers BEFORE
-- the root layer rows are deleted — the data-table FKs are ON DELETE CASCADE,
-- so deleting a still-referenced root would cascade the persistent index away.
--
-- Mirrors up.sql's performance shape: the NULL-out rewrites every row, so
-- all secondary indexes are dropped first and the original index set is
-- rebuilt afterwards.

-- 1. Purge cached ephemeral layers (cache; their base hashes are root-salted
--    and meaningless after revert).  Cascades their data rows.
DELETE FROM index.eph_layers WHERE kind NOT IN ('canary', 'root');

-- 2. Restore the old sign checks, drop the new ones.
ALTER TABLE index.symbols          DROP CONSTRAINT symbols_layer_sign_check;
ALTER TABLE index.symbol_instances DROP CONSTRAINT symbol_instances_layer_sign_check;
ALTER TABLE index.symbol_refs      DROP CONSTRAINT symbol_refs_layer_sign_check;

-- 3. Drop secondary indexes ahead of the full-table rewrite (see up.sql).
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
DROP INDEX index.symbol_instances_layer_uq;
DROP INDEX index.symbol_instances_instance_type_idx;
DROP INDEX index.symbol_instances_object_id_idx;
DROP INDEX index.symbol_instances_object_offset_gist_idx;
DROP INDEX index.symbol_instances_offset_range_idx;
DROP INDEX index.symbol_instances_symbol_idx;

DROP INDEX index.symbol_refs_eph_layer_idx;
DROP INDEX index.symbol_refs_layer_uq;
DROP INDEX index.symbol_refs_from_object_idx;
DROP INDEX index.symbol_refs_to_symbol_idx;

-- 4. Detach persistent rows from their roots.
ALTER TABLE index.symbols          ALTER COLUMN eph_layer DROP NOT NULL;
ALTER TABLE index.symbol_instances ALTER COLUMN eph_layer DROP NOT NULL;
ALTER TABLE index.symbol_refs      ALTER COLUMN eph_layer DROP NOT NULL;

UPDATE index.symbols          SET eph_layer = NULL WHERE eph_layer > 0;
UPDATE index.symbol_instances SET eph_layer = NULL WHERE eph_layer > 0;
UPDATE index.symbol_refs      SET eph_layer = NULL WHERE eph_layer > 0;

ALTER TABLE index.symbols
    ADD CONSTRAINT symbols_eph_id_sign_check
    CHECK (id > 0 OR eph_layer IS NOT NULL);
ALTER TABLE index.symbol_instances
    ADD CONSTRAINT symbol_instances_eph_id_sign_check
    CHECK (id > 0 OR eph_layer IS NOT NULL);
ALTER TABLE index.symbol_refs
    ADD CONSTRAINT symbol_refs_eph_id_sign_check
    CHECK (id > 0 OR eph_layer IS NOT NULL);

-- 5. Drop the project->root link, then the (now unreferenced) root layers.
ALTER TABLE index.projects
    DROP CONSTRAINT projects_root_layer_id_fkey,
    DROP CONSTRAINT projects_root_layer_id_uq,
    DROP COLUMN root_layer_id;

DELETE FROM index.eph_layers WHERE kind = 'root';

-- 6. Restore the positive-ascending identity sequence and drop the root
--    sequence.  Continue above the historical maximum positive id.
ALTER TABLE index.eph_layers ALTER COLUMN id SET MAXVALUE 9223372036854775807;
ALTER TABLE index.eph_layers ALTER COLUMN id SET INCREMENT BY 1;
-- START must move above MINVALUE before MINVALUE can rise (Postgres
-- validates START against the bounds on every ALTER).
ALTER TABLE index.eph_layers ALTER COLUMN id SET START WITH 1;
ALTER TABLE index.eph_layers ALTER COLUMN id RESTART WITH 1;
ALTER TABLE index.eph_layers ALTER COLUMN id SET MINVALUE 1;
SELECT setval(pg_get_serial_sequence('index.eph_layers', 'id'),
              (SELECT last_value + 1 FROM index.root_layer_id_seq), false);
DROP SEQUENCE index.root_layer_id_seq;

-- 7. Rebuild the original index set (partial eph indexes, split UNIQUEs).
SET LOCAL maintenance_work_mem = '1GB';
SET LOCAL max_parallel_maintenance_workers = 4;

CREATE INDEX symbols_eph_layer_idx
    ON index.symbols (eph_layer) WHERE eph_layer IS NOT NULL;
CREATE INDEX symbols_name_trgm_idx ON index.symbols USING gin (name gin_trgm_ops);
CREATE INDEX symbols_project_leafname_idx ON index.symbols (project_id, leaf_name);
CREATE INDEX symbols_project_name_idx ON index.symbols (project_id, name);
CREATE INDEX symbols_project_path_gist_idx ON index.symbols USING gist (project_id, symbol_path);
CREATE INDEX symbols_project_type_leafname_idx ON index.symbols (project_id, symbol_type, leaf_name);
CREATE INDEX symbols_project_type_name_idx ON index.symbols (project_id, symbol_type, name);
CREATE INDEX symbols_project_type_nlevel_name_idx
    ON index.symbols (project_id, symbol_type, nlevel(symbol_path), name);
CREATE INDEX symbols_type_idx ON index.symbols (symbol_type);

CREATE INDEX symbol_instances_eph_layer_idx
    ON index.symbol_instances (eph_layer) WHERE eph_layer IS NOT NULL;
CREATE UNIQUE INDEX symbol_instances_persistent_uq
    ON index.symbol_instances (symbol, object_id, offset_range)
    WHERE eph_layer IS NULL;
CREATE UNIQUE INDEX symbol_instances_eph_uq
    ON index.symbol_instances (symbol, object_id, offset_range, eph_layer)
    WHERE eph_layer IS NOT NULL;
CREATE INDEX symbol_instances_instance_type_idx ON index.symbol_instances (instance_type);
CREATE INDEX symbol_instances_object_id_idx ON index.symbol_instances (object_id);
CREATE INDEX symbol_instances_object_offset_gist_idx
    ON index.symbol_instances USING gist (object_id, offset_range);
CREATE INDEX symbol_instances_offset_range_idx
    ON index.symbol_instances USING gist (offset_range);
CREATE INDEX symbol_instances_symbol_idx ON index.symbol_instances (symbol);

CREATE INDEX symbol_refs_eph_layer_idx
    ON index.symbol_refs (eph_layer) WHERE eph_layer IS NOT NULL;
CREATE UNIQUE INDEX symbol_refs_persistent_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range)
    WHERE eph_layer IS NULL;
CREATE UNIQUE INDEX symbol_refs_eph_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, eph_layer)
    WHERE eph_layer IS NOT NULL;
CREATE INDEX symbol_refs_from_object_idx ON index.symbol_refs (from_object);
CREATE INDEX symbol_refs_to_symbol_idx ON index.symbol_refs (to_symbol);

ANALYZE index.symbols;
ANALYZE index.symbol_instances;
ANALYZE index.symbol_refs;
