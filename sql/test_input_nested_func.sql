SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

-- Object 1: /main.go file content
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'main.go', '/main.go', 'go', 'hash1');

-- Sentinel object for "/" directory
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 1, '/', '/', 'directory', '');

-- Directory symbol (type=4)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (50, '/', 1, 4, NULL);

-- File symbol (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (51, '/main.go', 1, 2, NULL);

-- Module symbol (type=3, level=3)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (1, 'testmodule', 1, 3, NULL);

-- Function symbols (type=1, level=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (2, 'testmodule.foo', 1, 1, 1),
    (5, 'testmodule.foo:<anon150>', 1, 1, 2),
    (6, 'testmodule.foo:<anon350>', 1, 1, 2),
    (3, 'testmodule.bar', 1, 1, 1),
    (4, 'testmodule.baz', 1, 1, 1);

-- Directory self-instance on sentinel object [0, 0)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (500, 50, 2, int4range(0, 0));

-- Directory instance on /main.go for containment queries
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (501, 50, 1, int4range(0, 1000));

-- File instance covers entire file [0, 1000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (510, 51, 1, int4range(0, 1000));

-- Module instance covers entire file [0, 1000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (10, 1, 1, int4range(0, 1000));

-- Function instances within file
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (20, 2, 1, int4range(100, 500)),   -- foo [100, 500)
    (25, 5, 1, int4range(150, 300)),   -- foo:<anon150> nested inside foo
    (26, 6, 1, int4range(350, 490)),   -- foo:<anon350> nested inside foo
    (30, 3, 1, int4range(500, 700)),   -- bar [500, 700)
    (40, 4, 1, int4range(700, 900));   -- baz [700, 900)

-- References: foo -> bar, bar -> baz, anon150 -> baz
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (3, 1, int4range(160, 170)),  -- anon150 body calls bar
    (4, 1, int4range(550, 560));  -- bar calls baz
