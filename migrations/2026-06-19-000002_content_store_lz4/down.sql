-- Revert the default TOAST compression algorithm to pglz for future
-- INSERT/UPDATE traffic.  Existing rows keep whatever algorithm they were
-- written with; a follow-up VACUUM FULL index.content_store is needed to
-- re-compress in place.
ALTER TABLE index.content_store ALTER COLUMN content_text SET COMPRESSION pglz;
ALTER TABLE index.content_store ALTER COLUMN content      SET COMPRESSION pglz;
