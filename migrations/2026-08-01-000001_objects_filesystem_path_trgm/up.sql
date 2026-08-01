-- Trigram index for askl_read's path resolver (Index::resolve_source_file):
-- segment-boundary path matching compiles to `filesystem_path LIKE '%/suffix'`,
-- whose leading '%' a plain btree cannot serve.  Mirrors the symbols name /
-- leaf_name trigram indexes.
--
-- Built non-CONCURRENTLY on purpose: diesel runs migrations inside a
-- transaction (CONCURRENTLY is not allowed there), matching the existing
-- symbols_leafname_trgm_idx build.  The GIN build takes a ShareLock on
-- index.objects, briefly blocking indexer writes at deploy.
CREATE INDEX objects_filesystem_path_trgm_idx ON index.objects USING gin (filesystem_path gin_trgm_ops);
