SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

-- Object 1: /main.go file content
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'main.go', '/main.go', 'go', 'hash1');

-- Sentinel object for "/" directory
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 1, '/', '/', 'directory', '');

-- Directory symbol (type=4) - / directory
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (50, '/', 1, 4, NULL);

-- File symbol (type=2) - /main.go file
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (51, '/main.go', 1, 2, NULL);

-- Module symbol (type=3, level=3)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (1, 'testmodule', 1, 3, NULL);

-- Function symbols (type=1, level=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (2, 'testmodule.foo', 1, 1, 1),
    (3, 'testmodule.bar', 1, 1, 1),
    (4, 'testmodule.baz', 1, 1, 1);

-- Directory self-instance on sentinel object [0, 0)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (500, 50, 2, int4range(0, 0), 4);

-- Directory instance on /main.go for containment queries
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (501, 50, 1, int4range(0, 1000), 5);

-- File instance covers entire file [0, 1000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (510, 51, 1, int4range(0, 1000), 6);

-- Module instance covers entire file [0, 1000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (10, 1, 1, int4range(0, 1000), 5);

-- Function instances within file
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (20, 2, 1, int4range(100, 200), 1),   -- foo
    (30, 3, 1, int4range(200, 300), 1),   -- bar
    (40, 4, 1, int4range(300, 400), 1);   -- baz

-- References: foo -> bar, bar -> baz (reference-based calls)
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (3, 1, int4range(150, 160)),  -- foo calls bar
    (4, 1, int4range(250, 260));  -- bar calls baz

-- Containment model (via range overlap + strict type level >):
--
-- Objects:
--   Object 1 (/main.go): directory, file, module, foo, bar, baz instances
--
-- Containment hierarchy:
--   "/" (directory, level 4) > "testmodule" (module, level 3) > foo, bar, baz (functions, level 1)
--   "/" (directory, level 4) > "/main.go" (file, level 2) > foo, bar, baz (functions, level 1)
--   "/main.go" (file, level 2) > foo, bar, baz (functions, level 1)
--
-- Directory→directory hierarchy uses symbol_refs (none in this test data, only one dir).
