-- What an index states.
--
-- A reference can state what it does to its target (`symbol_refs.roles`) and
-- a row can be a relation no token spells (`symbol_refs.relation`).  Nothing
-- obliges an index to say either: one written before those columns existed,
-- or by an indexer that does not know them, states nothing, and every one of
-- its references reads "refers to".
--
-- A query that asks by role (`calls { "kfree" }`) answers nothing on such an
-- index.  That is correct and looks exactly like "nothing calls kfree", so
-- the answer has to say which it is, and to say it without reading 23
-- million references: the fact is recorded here, once, where the index is
-- stored.
--
-- These are OBSERVATIONS of what was uploaded, not capabilities an indexer
-- declares: ingest sets a column when a row it stores has the property, and
-- never clears it.
--
--   states_roles   some ordinary reference of this project states a role;
--   has_declares   some row of it is a `declares` relation;
--   has_holds      some row of it is a `holds` relation.
--
-- Per project, because a project is one upload into one root layer.  An
-- ephemeral layer places references of its own on top (search hits, labels);
-- they state nothing, and are not what "the index" of a project means.
--
-- Constant defaults: a catalogue change, no table rewrite.  FALSE is the
-- right value for every existing project and not a placeholder: none of them
-- states anything.
ALTER TABLE index.projects
    ADD COLUMN states_roles BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN has_declares BOOLEAN NOT NULL DEFAULT FALSE,
    ADD COLUMN has_holds    BOOLEAN NOT NULL DEFAULT FALSE;

COMMENT ON COLUMN index.projects.states_roles IS
    'Observed at ingest: some ordinary reference of this project states a role (symbol_refs.roles). False: no role word can match here.';
COMMENT ON COLUMN index.projects.has_declares IS
    'Observed at ingest: some row of this project is a declares relation (symbol_refs.relation = 1).';
COMMENT ON COLUMN index.projects.has_holds IS
    'Observed at ingest: some row of this project is a holds relation (symbol_refs.relation = 2).';
