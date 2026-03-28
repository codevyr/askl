SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES
    (1, 'test_project', '/test_project');

-- directories table has been removed - directories are now symbols

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 'main.c', '/main.c', 'cc', '');

-- Sentinel object for "/" directory
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 1, '/', '/', 'directory', '');

-- File symbol (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (100, '/main.c', 1, 2, NULL);

-- Directory symbol (type=4)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (101, '/', 1, 4, NULL);

-- File instance
INSERT INTO symbol_instances (id, symbol, object_id, offset_range)
VALUES (1001, 100, 1, int4range(0, 1000));

-- Directory self-instance on sentinel object
INSERT INTO symbol_instances (id, symbol, object_id, offset_range)
VALUES (1002, 101, 2, int4range(0, 0));

-- Directory instance on /main.c for containment
INSERT INTO symbol_instances (id, symbol, object_id, offset_range)
VALUES (1003, 101, 1, int4range(0, 1000));

-- Function symbols (type=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (1, 'foo', 1, 1, 1),
    (2, 'foo.bar', 1, 1, 1),
    (3, 'foobar', 1, 1, 1),
    (4, 'tar', 1, 1, 1),
    (5, 'sort.IsSorted', 1, 1, 1),
    (6, 'sort.Sort', 1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range)
VALUES
    (91, 1, 1, int4range(910, 919)),
    (92, 2, 1, int4range(920, 929)),
    (93, 3, 1, int4range(930, 939)),
    (94, 4, 1, int4range(940, 949)),
    (95, 5, 1, int4range(950, 959)),
    (96, 6, 1, int4range(960, 969));

INSERT INTO
    symbol_refs(to_symbol, from_object, from_offset_range)
VALUES
    (2, 1, int4range(911, 912)),
    (2, 1, int4range(912, 913)),
    (3, 1, int4range(921, 922)),
    (4, 1, int4range(922, 923));

-- Data symbols (type=6, global variables)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (7, 'config.Debug', 1, 6, 2),
    (8, 'config.Port', 1, 6, 2);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range)
VALUES
    (97, 7, 1, int4range(970, 979)),
    (98, 8, 1, int4range(980, 989));

-- Reference from function foo to data config.Debug
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (7, 1, int4range(915, 916));
