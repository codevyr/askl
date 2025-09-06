-- Your SQL goes here

ALTER TABLE symbol_refs ADD COLUMN from_file INTEGER;

UPDATE symbol_refs AS sr
SET from_file = (
  SELECT f.id
  FROM declarations AS d
  JOIN files        AS f ON f.id = d.file_id
  WHERE d.id = sr.from_decl
  LIMIT 1
);

CREATE TRIGGER trg_symbol_refs_bi_set_from_file
AFTER INSERT ON symbol_refs
BEGIN
  UPDATE symbol_refs
  SET from_file = (
    SELECT f.id
    FROM declarations AS d
    JOIN files        AS f ON f.id = d.file_id
    WHERE d.id = NEW.from_decl
    LIMIT 1
  )
  WHERE rowid = NEW.rowid;
END;

-- When from_decl changes on symbol_refs: recompute from_file
CREATE TRIGGER trg_symbol_refs_bu_from_decl_set_from_file
AFTER UPDATE OF from_decl ON symbol_refs
BEGIN
  UPDATE symbol_refs
  SET from_file = (
    SELECT f.id
    FROM declarations AS d
    JOIN files        AS f ON f.id = d.file_id
    WHERE d.id = NEW.from_decl
    LIMIT 1
  )
  WHERE rowid = NEW.rowid;
END;

CREATE TRIGGER trg_declaration_au_from_file_touch_refs
AFTER UPDATE OF from_file ON declarations
BEGIN
  UPDATE symbol_refs
  SET from_file = NEW.from_file
  WHERE from_decl = NEW.id;
END;

CREATE INDEX IF NOT EXISTS idx_symbol_refs_from_decl ON symbol_refs(from_decl);
CREATE INDEX IF NOT EXISTS idx_symbol_refs_to_symbol ON symbol_refs(to_symbol);