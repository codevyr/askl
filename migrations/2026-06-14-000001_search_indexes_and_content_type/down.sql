-- Reverse of up.sql.

DELETE FROM index.symbol_types WHERE id = 9;

ALTER TABLE index.eph_layers DROP COLUMN IF EXISTS truncated;

DROP INDEX IF EXISTS index.content_store_text_trgm;
DROP INDEX IF EXISTS index.content_store_tsv_gin;

ALTER TABLE index.content_store DROP COLUMN IF EXISTS content_tsv;
ALTER TABLE index.content_store DROP COLUMN IF EXISTS content_text;

DROP FUNCTION IF EXISTS index.find_substr_byte_ranges(text, text, int);
DROP FUNCTION IF EXISTS index.is_word_char(text);
DROP FUNCTION IF EXISTS index.safe_to_tsvector_simple(text);
DROP FUNCTION IF EXISTS index.safe_convert_from(bytea);
