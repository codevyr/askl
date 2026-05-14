-- Reserve the ephemeral ID space so persistent rows can never use ephemeral IDs.
-- Ephemeral rows exist only in-memory (per-query CTE overlay) and are never
-- written to the database, but without these constraints a sufficiently large
-- persistent dataset could produce IDs that collide with the ephemeral range.
--
-- These values must stay in sync with the constants in
-- index/src/db_diesel/overlay.rs:
--   EPHEMERAL_SYMBOL_ID_MIN   = i64::MAX - 1_000_000_000 = 9223372035854775807
--   EPHEMERAL_INSTANCE_ID_MIN = i32::MAX - 1_000_000     = 2146483647
--   EPHEMERAL_REF_ID_MIN      = i32::MAX - 1_000_000     = 2146483647

-- CHECK constraints: reject any INSERT/UPDATE that would place a persistent row
-- in the ephemeral ID range, regardless of how the ID was produced.
ALTER TABLE index.symbols
    ADD CONSTRAINT symbols_id_not_ephemeral
    CHECK (id < 9223372035854775807);

ALTER TABLE index.symbol_instances
    ADD CONSTRAINT symbol_instances_id_not_ephemeral
    CHECK (id < 2146483647);

ALTER TABLE index.symbol_refs
    ADD CONSTRAINT symbol_refs_id_not_ephemeral
    CHECK (id < 2146483647);

-- Sequence caps: the SERIAL sequences for instances and refs will produce an
-- error ("nextval: reached maximum value of sequence") before ever allocating
-- an ID in the ephemeral range, providing an early-warning layer on top of the
-- CHECK constraint above.
-- Max allowed persistent value = EPHEMERAL_*_ID_MIN - 1 = 2146483646.
ALTER SEQUENCE index.symbol_instances_id_seq MAXVALUE 2146483646;
ALTER SEQUENCE index.symbol_refs_id_seq      MAXVALUE 2146483646;
