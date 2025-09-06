-- This file should undo anything in `up.sql`
DROP TRIGGER IF EXISTS trg_symbol_refs_bi_set_from_file;
DROP TRIGGER IF EXISTS trg_symbol_refs_bu_from_decl_set_from_file;
DROP TRIGGER IF EXISTS trg_declaration_au_from_file_touch_refs;

DROP INDEX IF EXISTS idx_symbol_refs_from_decl;
DROP INDEX IF EXISTS idx_symbol_refs_to_symbol;

ALTER TABLE symbol_refs DROP COLUMN from_file;