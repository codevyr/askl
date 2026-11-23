-- A relation says where it happened.
--
-- A relation row (`relation` 1 declares, 2 holds) is placed on the
-- declaration of its source: over the field in the header, on the macro's
-- name in its `#define`.  That says WHO is related.  Whoever opens the edge
-- `file_operations.read ⇢ bt_bmc_read` is shown the header line of the field,
-- which is the same line for every function the slot holds, and not
-- `.read = bt_bmc_read` in bt-bmc.c, which is where it was made so.
--
-- The WITNESS is that place: one place in the code that made the relation
-- (the first in file order), and how many there are.  Three nullable columns
-- on the reference, NULL on every reference and on a relation with no place
-- to name, so NULL is the right value for every existing row.
--
-- The witness names its object by the id the UPLOAD gave it, not by
-- `objects.id`.  A chunk of an upload knows the stored id of its own objects
-- only: chunks are sent concurrently and committed in any order, and the
-- field's header and the file that assigns it are in different chunks more
-- often than not.  So an object keeps its upload id (`objects.local_id`),
-- and a witness is resolved where it is read, through
-- `(project_id, local_id)`, for the page of occurrences being shown.
--
-- Whether every witness names an object the upload did deliver can only be
-- known when the last chunk is in.  Each chunk records the objects its
-- witnesses name and it does not itself carry
-- (`project_witness_objects`); finalize refuses the upload if one of them
-- never arrived, and empties the list.
--
-- Nullable columns with no default are catalog changes: no table is
-- rewritten.  The index on `objects` is built over a table of a few hundred
-- thousand rows, none of which it holds yet (it is partial).
ALTER TABLE index.symbol_refs
    ADD COLUMN witness_object_local_id BIGINT,
    ADD COLUMN witness_range INT4RANGE,
    ADD COLUMN witness_sites INTEGER;

COMMENT ON COLUMN index.symbol_refs.witness_object_local_id IS
    'On a relation row: the object of one place in the code that made the relation, as objects.local_id of the same project. NULL on a reference, and on a relation with no place to name.';
COMMENT ON COLUMN index.symbol_refs.witness_range IS
    'Byte range of that place in the witness object. NULL exactly when witness_object_local_id is.';
COMMENT ON COLUMN index.symbol_refs.witness_sites IS
    'How many places made the relation; the witness is the first in file order. At least 1. NULL exactly when witness_object_local_id is.';

ALTER TABLE index.objects
    ADD COLUMN local_id BIGINT;

COMMENT ON COLUMN index.objects.local_id IS
    'The id the upload gave this object (the proto''s Object.local_id), unique within its project. NULL for an object uploaded before witnesses existed and for one no upload made.';

CREATE INDEX objects_project_local_id_idx
    ON index.objects (project_id, local_id)
    WHERE local_id IS NOT NULL;

CREATE TABLE index.project_witness_objects (
    project_id INTEGER NOT NULL REFERENCES index.projects(id) ON DELETE CASCADE,
    object_local_id BIGINT NOT NULL
);

COMMENT ON TABLE index.project_witness_objects IS
    'Objects that the witnesses of an upload in progress name from another chunk. Checked against index.objects and emptied when the upload is finalized; no row outlives its upload.';

CREATE INDEX project_witness_objects_project_idx
    ON index.project_witness_objects (project_id);
