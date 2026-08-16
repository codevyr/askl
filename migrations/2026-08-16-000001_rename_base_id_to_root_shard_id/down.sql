ALTER INDEX index.layers_root_shard_id_idx RENAME TO eph_layers_base_id_idx;
ALTER TABLE index.layers RENAME COLUMN root_shard_id TO base_id;
