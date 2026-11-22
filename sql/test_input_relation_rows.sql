-- Relation rows: facts placed on the declaration of their source.
--
--     /ops.h  (file, in directory /)
--       ops        (type)   a struct holding one field, `slot`;
--       slot       (field)  HOLDS `my_read`, and its declaration also names
--                           `plain` in the ordinary way (say, in its type);
--       my_read, plain (func)
--       DEFINE_X   (macro)  its invocation DECLARES `lock`, and its body
--                           names `plain`;
--       lock       (data)
--       DEFINE_Y   (macro)  its invocation DECLARES `both`, and its body
--                           also names `both`: one pair, two relationships;
--       both       (data)
--       bare_slot  (field)  HOLDS `kept` and `hidden` and names nothing: a
--                           slot whose only relationship is the relation;
--       kept       (func)
--       hidden     (func)   folded: nobody wrote its name;
--       GEN_Z      (macro)  folded; its invocation DECLARES `gen_only`;
--       gen_only   (data)
--
-- A reference belongs to every declaration around it: `plain` is referred to
-- by slot, by ops, by the file and by the directory.  A relation belongs to
-- its source only: `my_read` is held by `slot`, and neither ops, the file
-- nor the directory "refers to" it.  Likewise `lock` is declared by
-- `DEFINE_X` and by nothing around the `#define`.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-relation-rows'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'ops.h', '/ops.h', 'cc', 'ho');

-- function 1, file 2, directory 4, type 5, data 6, macro 7, field 8
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (50, '/',        1, 4, NULL),
       (51, '/ops.h',   1, 2, NULL),
       (1,  'ops',      1, 5, 1),
       (2,  'slot',     1, 8, 1),
       (3,  'my_read',  1, 1, 1),
       (4,  'plain',    1, 1, 1),
       (5,  'DEFINE_X', 1, 7, 1),
       (6,  'lock',     1, 6, 1),
       (7,  'DEFINE_Y', 1, 7, 1),
       (8,  'both',     1, 6, 1),
       (9,  'bare_slot', 1, 8, 1),
       (10, 'kept',     1, 1, 1),
       (11, 'hidden',   1, 1, 1),
       (12, 'GEN_Z',    1, 7, 1),
       (13, 'gen_only', 1, 6, 1);

UPDATE symbols SET folded = TRUE WHERE id IN (11, 12);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (500, 50, 1, int4range(0, 2000), 5),      -- the directory, over the file
       (510, 51, 1, int4range(0, 2000), 6),      -- the file
       (10,  1,  1, int4range(100, 400), 1),     -- ops
       (20,  2,  1, int4range(150, 200), 1),     -- slot, inside ops
       (30,  3,  1, int4range(500, 600), 1),     -- my_read
       (40,  4,  1, int4range(700, 800), 1),     -- plain
       (60,  5,  1, int4range(900, 1000), 1),    -- DEFINE_X, the #define
       (70,  6,  1, int4range(1100, 1150), 1),   -- lock
       (80,  7,  1, int4range(1300, 1400), 1),   -- DEFINE_Y, the #define
       (90,  8,  1, int4range(1500, 1550), 1),   -- both
       (100, 9,  1, int4range(1600, 1650), 1),   -- bare_slot
       (110, 10, 1, int4range(1700, 1800), 1),   -- kept
       (120, 11, 1, int4range(1900, 1950), 1),   -- hidden
       (130, 12, 1, int4range(2000, 2100), 1),   -- GEN_Z, the #define
       (140, 13, 1, int4range(2200, 2250), 1);   -- gen_only

-- On `slot`'s declaration: the slot HOLDS my_read (relation 2) ...
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (3, 1, int4range(160, 170), 2);
-- ... and names `plain` as any reference does.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (4, 1, int4range(172, 180));

-- On the `#define`: the macro DECLARES lock (relation 1) ...
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (6, 1, int4range(910, 920), 1);
-- ... and its body names `plain`.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (4, 1, int4range(930, 940));

-- On the second `#define`: the macro DECLARES `both` (relation 1) ...
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (8, 1, int4range(1310, 1320), 1);
-- ... and its body names `both` too: the pair has an ordinary reference and
-- a relation.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (8, 1, int4range(1330, 1340));

-- On `bare_slot`'s declaration: it HOLDS kept (relation 2), and nothing else
-- is written there.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (10, 1, int4range(1610, 1620), 2),
       (11, 1, int4range(1630, 1640), 2);

-- On the folded `#define`: it DECLARES gen_only (relation 1).
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (13, 1, int4range(2010, 2020), 1);

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
