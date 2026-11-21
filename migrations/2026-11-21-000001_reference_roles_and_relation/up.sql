-- A reference says what it does to its target.
--
-- Until now a row of `symbol_refs` said one thing: something at this place
-- names that symbol.  Whether it calls it, stores into it, passes it along,
-- or is not a use at all but a fact the indexer derived (this macro declared
-- that mutex; this slot holds that handler) was known to the indexer and
-- thrown away, and every reader guessed it from the two symbol types.
--
-- `roles` is a SET of bits (1 call, 2 write, 4 take: the proto's RefRole).
-- Every bit is a positive fact and zero states nothing, so zero is the right
-- value for every existing row and not a placeholder: a project indexed
-- before this, and every reference an ephemeral layer places, keeps reading
-- "refers to".
--
-- `relation` is zero for a reference.  Otherwise the row is a fact between
-- two symbols that no token spells (1 declares, 2 holds: the proto's
-- RefRelation), placed on the declaration of its source.
--
-- Constant defaults, so both are catalog changes with no table rewrite.
ALTER TABLE index.symbol_refs
    ADD COLUMN roles SMALLINT NOT NULL DEFAULT 0,
    ADD COLUMN relation SMALLINT NOT NULL DEFAULT 0;

COMMENT ON COLUMN index.symbol_refs.roles IS
    'What the token does to its target, as a set of bits: 1 call, 2 write, 4 take. Zero states nothing. A role is what the text does, not what runs.';
COMMENT ON COLUMN index.symbol_refs.relation IS
    'Zero for a reference. Otherwise a fact between two symbols that no token spells, placed on the declaration of its source: 1 declares, 2 holds.';
