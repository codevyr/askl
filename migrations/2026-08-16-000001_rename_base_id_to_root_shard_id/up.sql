-- The engine's vocabulary now matches the design docs: a command's cache
-- entry is partitioned into a ROOT SHARD (keyed on root identity + the
-- command's input hash), zero or more LAYER SHARDS (one per visible
-- ephemeral content layer), and a SELECTION SHARD (the eph-chained delta).
-- What this column has always held is the id of the ROOT SHARD a delta
-- layer's lifetime is coupled to (ON DELETE CASCADE), so name it that.
--
-- Supersedes the comment in 2026-07-26-000001_eph_layer_base_fk, which was
-- already stale: it claims the column is set only on rows with
-- kind = 'supplement', but `kind` has since been coarsened to 'ephemeral'
-- and both selection shards and layer shards carry the reference.
--
-- Metadata-only: renames touch no data.
ALTER TABLE index.layers RENAME COLUMN base_id TO root_shard_id;

-- The index kept its pre-2026-07-28 name (ALTER TABLE ... RENAME TO does
-- not rename indexes), so this catches the container rename up too.
ALTER INDEX index.eph_layers_base_id_idx RENAME TO layers_root_shard_id_idx;
