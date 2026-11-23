-- Relations that say where they happened, and rows that have nowhere to say.
--
--     /ops.h    ops       (type)   a struct with one field, `slot`;
--               slot      (field)  HOLDS five functions, each a row on the
--                                  field's declaration;
--               DEFINE_X  (macro)  DECLARES `lock`, a row on its `#define`.
--     /drv.c    my_read, shared_read, bare, lock, and `caller`, which CALLS
--               `my_read` (a reference: its own span is where it happened).
--     /other.c  shared_read again, and `late`.
--
-- A relation row's own place is the declaration of its SOURCE, the same line
-- of /ops.h for everything the slot holds.  Its WITNESS is one place that
-- made the relation: `.slot = my_read` in /drv.c.
--
--     slot ⇢ my_read      witness /drv.c  [1000, 1015), 1 site
--     slot ⇢ shared_read  witness /drv.c  [1020, 1039), the first of 3 sites
--     slot ⇢ late         witness /other.c [300, 312), 1 site
--     slot ⇢ bare         no witness: a relation with no place to name
--     slot ⇢ ghost        a witness naming an object the project does not
--                         have (what a reader sees while the chunk with that
--                         object is still on its way)
--     DEFINE_X ↦ lock     witness /drv.c  [890, 904), 1 site
--
-- The witness names its object by `objects.local_id`, the id the upload gave
-- it, which is not `objects.id`: here 11, 12, 13 against 1, 2, 3.  (/dup.c
-- repeats 12.)

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-witness'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id,
                      states_roles, has_declares, has_holds)
VALUES (1, 'test_project', '/test_project', 1000001, TRUE, TRUE, TRUE);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash, local_id)
VALUES (1, 1, 'ops.h',   '/ops.h',   'cc', 'ho', 11),
       (2, 1, 'drv.c',   '/drv.c',   'cc', 'hd', 12),
       (3, 1, 'other.c', '/other.c', 'cc', 'hx', 13),
       -- An upload does not send two objects under one id, and nothing in
       -- the table forbids it.  Then the witness is the first of them, and
       -- an occurrence is still one row.
       (4, 1, 'dup.c',   '/dup.c',   'cc', 'hz', 12);

-- function 1, type 5, data 6, macro 7, field 8
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1,  'caller',      1, 1, 1),
       (2,  'ops',         1, 5, 1),
       (3,  'slot',        1, 8, 1),
       (4,  'my_read',     1, 1, 1),
       (5,  'shared_read', 1, 1, 1),
       (6,  'bare',        1, 1, 1),
       (7,  'late',        1, 1, 1),
       (8,  'ghost',       1, 1, 1),
       (9,  'DEFINE_X',    1, 7, 1),
       (10, 'lock',        1, 6, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (90,  9,  1, int4range(10, 60), 1),      -- DEFINE_X, in /ops.h
       (20,  2,  1, int4range(700, 900), 1),    -- ops
       (30,  3,  1, int4range(710, 730), 1),    -- slot, inside ops
       (40,  4,  2, int4range(100, 200), 1),    -- my_read, in /drv.c
       (50,  5,  2, int4range(300, 400), 1),    -- shared_read
       (60,  6,  2, int4range(500, 600), 1),    -- bare
       (10,  1,  2, int4range(700, 800), 1),    -- caller
       (100, 10, 2, int4range(890, 904), 1),    -- lock
       (80,  8,  2, int4range(1100, 1200), 1),  -- ghost
       (51,  5,  3, int4range(100, 200), 1),    -- shared_read, in /other.c
       (70,  7,  3, int4range(210, 290), 1);    -- late

-- On `slot`'s declaration: what the slot holds (relation 2).
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation,
                         witness_object_local_id, witness_range, witness_sites)
VALUES (4, 1, int4range(712, 716), 2, 12,   int4range(1000, 1015), 1),
       (5, 1, int4range(712, 716), 2, 12,   int4range(1020, 1039), 3),
       (7, 1, int4range(712, 716), 2, 13,   int4range(300, 312),   1),
       (6, 1, int4range(712, 716), 2, NULL, NULL,                  NULL),
       (8, 1, int4range(712, 716), 2, 99,   int4range(5, 9),       1);

-- On `DEFINE_X`'s `#define`: what its invocation declared (relation 1).
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation,
                         witness_object_local_id, witness_range, witness_sites)
VALUES (10, 1, int4range(18, 26), 1, 12, int4range(890, 904), 1);

-- Inside `caller`: a call of my_read.  A reference has no witness.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (4, 2, int4range(710, 717), 1);

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
