-- The index a reference is found by, from its target, carries what deciding
-- whose reference it is needs.
--
-- "Who refers to X" enters `symbol_refs` by `to_symbol` through
-- `symbol_refs_layer_uq`, and never touched the table: where the reference is
-- (`from_object`, `dfs_enter`) is in the index, and that was all the question
-- "which declarations is it inside" asked.
--
-- A relation row -- the macro DECLARES it, the slot HOLDS it -- belongs to
-- the one declaration it sits directly in and not to everything around that
-- (`index::db_diesel::attribution`).  Deciding it reads two more columns of
-- the reference, `relation` and `nesting_level`.  With those only in the
-- table, every reference visited costs a row fetch: measured on the kernel
-- index, the probe that walks from `spinlock_t` and tests against a module
-- went from 253 ms to 460 ms (276,004 buffers to 509,098), and a read of
-- 10,001 references from a hot name gains about 60 ms, three times over in
-- the slowest everyday query.
--
-- INCLUDE keeps them out of the key, so uniqueness and every prefix the index
-- offers are what they were, and an index-only scan can return them.  `roles`
-- comes along for the same reason one step ahead: a read that asks "who CALLS
-- X" will filter on it, and this index is not rebuilt lightly.
--
-- Cost: the index is rebuilt.  On the kernel index (23M references) that took
-- 8.4 s and the result is 1.9 GB (the old one, bloated, 2.6 GB).

DROP INDEX index.symbol_refs_layer_uq;

CREATE UNIQUE INDEX symbol_refs_layer_uq
    ON index.symbol_refs (to_symbol, from_object, from_offset_range, layer, dfs_enter)
    INCLUDE (relation, nesting_level, roles);
