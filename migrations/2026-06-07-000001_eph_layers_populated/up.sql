-- Two-phase commit for ephemeral layers.
--
-- Without this flag, a winner-of-hash-race crash between INSERT INTO
-- eph_layers and the populate batch would leave the layer row visible
-- with no symbol/instance/ref rows.  Subsequent requests hitting the
-- same hash would see an empty layer and return wrong results.
--
-- With the flag:
--   1. create_eph_layer inserts with populated = FALSE.
--   2. Populate batch runs (eph_symbols / eph_instances / eph_refs).
--   3. with_eph_layer flips populated = TRUE just before COMMIT.
--   4. Readers add `AND populated = TRUE` to any eph_layers lookup
--      that would otherwise feed a hash-race winner the loser's
--      half-built state.
--
-- The canary layer is the documented exception: it ships populated
-- from the previous migration.
ALTER TABLE index.eph_layers
    ADD COLUMN populated BOOLEAN NOT NULL DEFAULT FALSE;

-- Backfill: every layer that already exists is populated.  This is
-- safe because the only pre-existing layers come from successful
-- requests (the migration ran before any 2PC-aware code) or from the
-- canary insert in the previous migration.
UPDATE index.eph_layers SET populated = TRUE;
