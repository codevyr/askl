SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

INSERT INTO directories (id, project_id, parent_id, path)
VALUES (1, 1, NULL, '/');

INSERT INTO objects (id, project_id, directory_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 1, 'main.go', '/main.go', 'go', 'hash1');

-- Module symbol (type=3, level=3)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (1, 'testmodule', 1, 3, NULL);

-- Function symbols (type=1, level=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (2, 'testmodule.foo', 1, 1, 1),
    (3, 'testmodule.bar', 1, 1, 1),
    (4, 'testmodule.baz', 1, 1, 1);

-- Module instance covers entire file [0, 1000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (10, 1, 1, int4range(0, 1000));

-- Function instances within file
INSERT INTO symbol_instances (id, symbol, object_id, offset_range) VALUES
    (20, 2, 1, int4range(100, 200)),   -- foo
    (30, 3, 1, int4range(200, 300)),   -- bar
    (40, 4, 1, int4range(300, 400));   -- baz

-- References: foo -> bar, bar -> baz (reference-based calls)
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (3, 1, int4range(150, 160)),  -- foo calls bar
    (4, 1, int4range(250, 260));  -- bar calls baz

-- Containment relationships are derived from:
-- module [0,1000) contains foo [100,200), bar [200,300), baz [300,400)
-- because module.type.level (3) > function.type.level (1)
