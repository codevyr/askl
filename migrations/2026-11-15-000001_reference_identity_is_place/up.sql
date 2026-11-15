-- A reference is identified by what it names, the token that names it, and
-- where it sits in the containment tree.
--
-- `symbol_refs_layer_uq` has been (to_symbol, from_object, from_offset_range,
-- layer): one reference per target per token.  That is one too few when one
-- token is read inside several declarations:
--
--     #define KHASH_INIT(name, key_t, hash_func)                        \
--         static int kh_get_##name(key_t k)    { return hash_func(k); } \
--         static int kh_put_##name(key_t k)    { return hash_func(k); }
--
--     KHASH_INIT(x, int, my_hash)
--
-- Both generated functions call `my_hash`, on the one `my_hash` the caller
-- typed.  The pair is unstorable under the old index, so the producer kept the
-- reference for whichever function it read first and the other called
-- nothing.  The same holds for a file compiled twice under two names, where
-- two functions sit on the same bytes.
--
-- An instance has been identified by its place since
-- 2026-10-10-000001_instance_identity_is_dfs.  This is the same change for a
-- reference: `dfs_enter` is one step inside the instance it sits in, so two
-- references to one target on one token differ in it exactly when they sit in
-- different instances.
--
-- No partial predicate is needed, unlike for instances.  The new key is the
-- old key plus a column, so every set of rows the old index accepted the new
-- one accepts too, including rows uploaded before references were numbered
-- (dfs_enter = 0).
--
-- `dfs_enter` goes LAST so every prefix the old index offered is still there.

DROP INDEX index.symbol_refs_layer_uq;

CREATE UNIQUE INDEX symbol_refs_layer_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, layer, dfs_enter);
