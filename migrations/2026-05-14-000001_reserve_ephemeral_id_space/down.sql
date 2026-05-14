ALTER TABLE index.symbols          DROP CONSTRAINT IF EXISTS symbols_id_not_ephemeral;
ALTER TABLE index.symbol_instances DROP CONSTRAINT IF EXISTS symbol_instances_id_not_ephemeral;
ALTER TABLE index.symbol_refs      DROP CONSTRAINT IF EXISTS symbol_refs_id_not_ephemeral;
ALTER SEQUENCE index.symbol_instances_id_seq MAXVALUE 2147483647;
ALTER SEQUENCE index.symbol_refs_id_seq      MAXVALUE 2147483647;
