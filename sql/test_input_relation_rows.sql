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
       (6,  'lock',     1, 6, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (500, 50, 1, int4range(0, 2000), 5),      -- the directory, over the file
       (510, 51, 1, int4range(0, 2000), 6),      -- the file
       (10,  1,  1, int4range(100, 400), 1),     -- ops
       (20,  2,  1, int4range(150, 200), 1),     -- slot, inside ops
       (30,  3,  1, int4range(500, 600), 1),     -- my_read
       (40,  4,  1, int4range(700, 800), 1),     -- plain
       (60,  5,  1, int4range(900, 1000), 1),    -- DEFINE_X, the #define
       (70,  6,  1, int4range(1100, 1150), 1);   -- lock

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

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
