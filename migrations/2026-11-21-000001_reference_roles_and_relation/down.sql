ALTER TABLE index.symbol_refs
    DROP COLUMN IF EXISTS relation,
    DROP COLUMN IF EXISTS roles;
