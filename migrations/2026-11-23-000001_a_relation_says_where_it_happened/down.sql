DROP TABLE IF EXISTS index.project_witness_objects;

DROP INDEX IF EXISTS index.objects_project_local_id_idx;

ALTER TABLE index.objects
    DROP COLUMN IF EXISTS local_id;

ALTER TABLE index.symbol_refs
    DROP COLUMN IF EXISTS witness_sites,
    DROP COLUMN IF EXISTS witness_range,
    DROP COLUMN IF EXISTS witness_object_local_id;
