-- One field name in many structs, and one variable that refers to one of them.
--
--     S1 .. S8          eight structs in /types.h, each with a field `lockf`
--                       (eight symbols `S<i>.lockf`, one instance each).
--     guard             a variable that refers to `S1.lockf`.
--     spare             a variable that refers to nothing.
--     busy              a function that refers to `S1.lockf` 41 times.
--
-- `data { "lockf" }` has two rows: `guard` and `S1.lockf`.  The name alone has
-- eight, read in full under a cap of eight.  Everything that CONTAINS those
-- eight (a struct and the file, each) is sixteen rows, and what refers to them
-- anywhere is more than a probe may read at that cap (`busy`), so the name is
-- read on its own and its containment side read overflows.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-name-in-many-structs'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'types.h', '/types.h', 'c', 'h1');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (51, '/types.h', 1, 2, NULL),      -- file
       (30, 'guard', 1, 6, NULL),         -- data
       (31, 'spare', 1, 6, NULL),         -- data
       (40, 'busy', 1, 1, NULL);          -- function
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT i, 'S' || i, 1, 5, NULL FROM generate_series(1, 8) AS i;             -- types
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 10 + i, 'S' || i || '.lockf', 1, 8, NULL FROM generate_series(1, 8) AS i;  -- fields

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (510, 51, 1, int4range(0, 20000), 6),      -- the file
       (300, 30, 1, int4range(9000, 9100), 1),    -- guard
       (301, 31, 1, int4range(9200, 9300), 1),    -- spare
       (400, 40, 1, int4range(10000, 11000), 1);  -- busy
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 100 + i, i, 1, int4range(i * 1000, i * 1000 + 500), 1 FROM generate_series(1, 8) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 200 + i, 10 + i, 1, int4range(i * 1000 + 100, i * 1000 + 150), 1
FROM generate_series(1, 8) AS i;

-- guard refers to S1.lockf once; busy refers to it 41 times.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (11, 1, int4range(9010, 9015));
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 11, 1, int4range(10000 + i * 10, 10005 + i * 10) FROM generate_series(1, 41) AS i;

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
