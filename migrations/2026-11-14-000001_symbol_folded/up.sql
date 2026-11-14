-- Folding moves from the instance to the symbol.
--
-- `folded` answers one question: is this listed by default?  It was stored on
-- every instance and reference, as a flag plus a count of folded ancestors,
-- and the default traversal compared the counts.  That made hiding a property
-- of where something SITS: a generated symbol was hidden only when it sat
-- inside another generated symbol, so `mod { func }` on the kernel listed
-- 455,417 `__compiletime_assert_N` and hid 59,880.  It also leaked into
-- containment, which had to keep references out of generated declarations so
-- the counts would come out right.
--
-- Whether a symbol is worth listing is a fact about the symbol: did anyone
-- write it down in normal code, anywhere in the project.  The indexer decides
-- that once (`Symbol.folded` in the proto) and the default traversal reads it
-- from the symbol row each query already joins.  `unnest` and naming a symbol
-- ignore it, as before.
--
-- DEFAULT FALSE is the right value for every existing row, not a placeholder:
-- a project indexed before this has no folded symbols and lists everything
-- until it is reindexed, and a query-time (ephemeral) symbol is never folded.
-- Constant default, so this is a catalog change with no table rewrite.
--
-- The per-instance and per-reference columns are left in place, unread; a
-- later migration drops them.
ALTER TABLE index.symbols
    ADD COLUMN folded BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN index.symbols.folded IS
    'Not listed by default: nobody wrote this symbol down in normal code (outside every #define body). Plays no part in containment; unnest or naming the symbol shows it regardless.';
