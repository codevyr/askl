-- One name defined in two programs, only one of which makes a given call.
--
--     /a.c   main (instance 10) calls print_version
--     /b.c   main (instance 20) calls helper
--     /x.c   print_version (30), helper (40)
--
-- `main` is ONE symbol with two instances (C has one symbol per name).  The
-- reference to `print_version` is written inside instance 10 and nowhere
-- else, so the `main` of /b.c does not refer to it.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-one-name-two-programs'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'a.c', '/a.c', 'cc', 'ha'),
       (2, 1, 'b.c', '/b.c', 'cc', 'hb'),
       (3, 1, 'x.c', '/x.c', 'cc', 'hx');

-- Files (type 2) and functions (type 1).
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (101, '/a.c', 1, 2, NULL),
       (102, '/b.c', 1, 2, NULL),
       (103, '/x.c', 1, 2, NULL),
       (1, 'main',          1, 1, 1),
       (2, 'print_version', 1, 1, 1),
       (3, 'helper',        1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (1001, 101, 1, int4range(0, 1000), 6),
       (1002, 102, 2, int4range(0, 1000), 6),
       (1003, 103, 3, int4range(0, 1000), 6),
       (10, 1, 1, int4range(100, 200), 1),   -- main in /a.c
       (20, 1, 2, int4range(100, 200), 1),   -- main in /b.c
       (30, 2, 3, int4range(100, 200), 1),   -- print_version
       (40, 3, 3, int4range(300, 400), 1);   -- helper

INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (2, 1, int4range(110, 115)),   -- main of /a.c -> print_version
       (3, 2, int4range(110, 115));   -- main of /b.c -> helper

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
