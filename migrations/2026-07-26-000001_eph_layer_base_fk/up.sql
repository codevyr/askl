-- Couple a supplement layer's lifetime to its base layer.
--
-- A partitioned cache entry is a pair: a parentless base (keyed on verb
-- inputs only) and a chained supplement (keyed on parent chain + base hash
-- + supplement inputs).  Before this migration nothing tied their
-- lifetimes: the base's last_used is always the older of the pair (it is
-- upserted first), so a TTL purge cutoff could delete the base while its
-- supplement stayed cached, and BaseLayerRef.layer_id handed to supplement
-- populates could dangle after a purge+recreate of the base.
--
-- base_id is set only on supplement rows (kind = 'supplement'); ON DELETE
-- CASCADE makes the base's death take its supplements with it, so a cached
-- supplement always refers to a live base of the same incarnation.
ALTER TABLE index.eph_layers
    ADD COLUMN base_id BIGINT REFERENCES index.eph_layers(id) ON DELETE CASCADE;

CREATE INDEX eph_layers_base_id_idx
    ON index.eph_layers (base_id)
    WHERE base_id IS NOT NULL;
