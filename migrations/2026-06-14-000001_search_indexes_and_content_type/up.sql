-- Indexes and helpers for the new search("...") verb.
--
-- New content surface:
--   * content_text  - generated text view of content_store.content (NULL on non-UTF-8)
--   * content_tsv   - generated tsvector view for fast whole-word pre-filter
--   * GIN on content_text using gin_trgm_ops for substring / case-sensitive paths
--   * GIN on content_tsv for the whole-word case-insensitive path
--
-- New SQL helpers (no regex anywhere — single source of truth):
--   * safe_convert_from(bytea)         - bytea -> text or NULL on bad UTF-8
--   * is_word_char(text)               - ASCII word-char predicate for boundary checks
--   * find_substr_byte_ranges(...)     - SETOF (start_byte, end_byte, start_char) for every
--                                        occurrence of needle in haystack, capped at max_n
--
-- New eph_layers column:
--   * truncated boolean                - search() writes true when result hit the limit cap;
--                                        cache hits read this and reconstruct the warning
--                                        in the verb's own span.
--
-- New symbol_types row:
--   * (9, 'content', 1, true)          - shared by loc() and search(); they create symbols
--                                        anchored to a byte range in source content rather
--                                        than to a real language-level symbol.

-- pg_trgm and ltree are already enabled by the initial migration.

CREATE OR REPLACE FUNCTION index.safe_convert_from(c bytea) RETURNS text
LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
AS $$
DECLARE
    s text;
BEGIN
    BEGIN
        s := convert_from(c, 'UTF8');
    EXCEPTION WHEN OTHERS THEN
        RETURN NULL;
    END;
    RETURN s;
END;
$$;

-- to_tsvector raises e.g. "string is too long for tsvector" for inputs
-- over PostgreSQL's hard 1 MiB tsvector limit.  Generated columns surface
-- such errors at row insert/update time, which would block uploads of
-- large source blobs (generated code, lockfiles, vendor bundles).  Wrap
-- in an exception handler so oversized or otherwise unindexable rows
-- yield NULL -- they're excluded from the tsvector path, the same way
-- non-UTF-8 binary content is.  The pg_trgm GIN on content_text is
-- unaffected and still handles those rows for substring searches.
CREATE OR REPLACE FUNCTION index.safe_to_tsvector_simple(t text) RETURNS tsvector
LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
AS $$
DECLARE
    tsv tsvector;
BEGIN
    IF t IS NULL THEN
        RETURN NULL;
    END IF;
    BEGIN
        tsv := to_tsvector('simple', lower(t));
    EXCEPTION WHEN OTHERS THEN
        RETURN NULL;
    END;
    RETURN tsv;
END;
$$;

CREATE OR REPLACE FUNCTION index.is_word_char(c text) RETURNS boolean
LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
AS $$
DECLARE
    code int;
BEGIN
    IF c = '' OR c IS NULL THEN RETURN false; END IF;
    code := ascii(c);
    RETURN (code BETWEEN 48 AND 57)    -- 0-9
        OR (code BETWEEN 65 AND 90)    -- A-Z
        OR (code BETWEEN 97 AND 122)   -- a-z
        OR  code = 95;                  -- _
END;
$$;

-- Find every occurrence of needle in haystack as (start_byte, end_byte, start_char),
-- capped at max_n.  Maintains synchronised char/byte cursors so total cost per call
-- is O(length(haystack)) regardless of match count.
--
-- Positions:
--   start_byte / end_byte - 0-based byte offsets inside haystack
--   start_char            - 1-based character position inside haystack
--
-- The function has NO flags: case folding (lower) and word-boundary checks are
-- composed in the calling SQL.  This keeps the helper trivially correct and lets
-- each search variant be a flat SELECT with no internal branching.
CREATE OR REPLACE FUNCTION index.find_substr_byte_ranges(
    haystack text,
    needle text,
    max_n int
) RETURNS TABLE(start_byte int, end_byte int, start_char int)
LANGUAGE plpgsql IMMUTABLE PARALLEL SAFE
AS $$
DECLARE
    cur_char int := 1;
    cur_byte int := 0;
    nlen int := length(needle);
    found int;
    gap_bytes int;
    emitted int := 0;
BEGIN
    IF haystack IS NULL OR nlen = 0 THEN RETURN; END IF;
    LOOP
        EXIT WHEN cur_char > length(haystack) OR emitted >= max_n;
        found := position(needle in substring(haystack from cur_char));
        EXIT WHEN found = 0;
        found := cur_char + found - 1;
        gap_bytes := octet_length(substring(haystack from cur_char for (found - cur_char)));
        cur_byte := cur_byte + gap_bytes;
        start_byte := cur_byte;
        cur_byte := cur_byte + octet_length(substring(haystack from found for nlen));
        end_byte := cur_byte;
        start_char := found;
        RETURN NEXT;
        cur_char := found + nlen;
        emitted := emitted + 1;
    END LOOP;
END;
$$;

-- content_text: bytea -> text view of content_store.content; NULL when not valid UTF-8.
ALTER TABLE index.content_store
    ADD COLUMN content_text text
    GENERATED ALWAYS AS (index.safe_convert_from(content)) STORED;

-- content_tsv: lowercased simple-tokenizer tsvector for the fast whole-word path.
-- Wrapped in safe_to_tsvector_simple so rows that exceed PostgreSQL's hard
-- 1 MiB tsvector limit yield NULL rather than failing the INSERT.
ALTER TABLE index.content_store
    ADD COLUMN content_tsv tsvector
    GENERATED ALWAYS AS (
        index.safe_to_tsvector_simple(index.safe_convert_from(content))
    ) STORED;

-- GIN on content_tsv covers `whole_word=true, case=insensitive` via @@ phraseto_tsquery.
CREATE INDEX IF NOT EXISTS content_store_tsv_gin
    ON index.content_store
    USING GIN (content_tsv);

-- GIN on content_text via gin_trgm_ops covers the other three variants via LIKE/ILIKE.
CREATE INDEX IF NOT EXISTS content_store_text_trgm
    ON index.content_store
    USING GIN (content_text gin_trgm_ops);

-- search() sets truncated=true when the result hit the limit cap; cache hits read it
-- so the verb can re-emit the warning with its own span.
ALTER TABLE index.eph_layers
    ADD COLUMN truncated BOOLEAN NOT NULL DEFAULT false;

-- New symbol type used by both search() and (after the loc() retrofit) loc().
-- level=1 (leaf), dot_is_separator=true: same characteristics as function/data/macro/etc.
INSERT INTO index.symbol_types (id, name, level, dot_is_separator) VALUES
    (9, 'content', 1, true)
ON CONFLICT (id) DO NOTHING;
