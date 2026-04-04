SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

-- directories table has been removed - directories are now symbols

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 'main.c', '/main.c', 'cc', ''),
    (2, 1, 'bar.c', '/bar.c', 'cc', '');

-- Sentinel object for "/" directory
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (3, 1, '/', '/', 'directory', '');

-- File symbols (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (100, '/main.c', 1, 2, NULL),
    (102, '/bar.c', 1, 2, NULL);

-- Directory symbol (type=4) for /
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (101, '/', 1, 4, NULL);

-- File symbol instances covering entire files
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (1001, 100, 1, int4range(0, 10000), 6),
    (1003, 102, 2, int4range(0, 10000), 6);

-- Directory self-instance on sentinel object
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (1005, 101, 3, int4range(0, 0), 4);

-- Directory instances on direct child files (for containment)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (1002, 101, 1, int4range(0, 10000), 5),
    (1004, 101, 2, int4range(0, 10000), 5);

-- Function symbols (type=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (1,  'a',    1, 1, 1),
    (2,  'b',    1, 1, 1),
    (3,  'c',    1, 1, 1),
    (4,  'd',    1, 1, 1),
    (5,  'e',    1, 1, 1),
    (6,  'f',    1, 1, 1),
    (7,  'g',    1, 1, 1),
    (42, 'main', 1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (91,  1,  1, int4range(910, 919), 1),
    (92,  2,  1, int4range(920, 929), 1),
    (93,  3,  1, int4range(930, 939), 1),
    (94,  4,  1, int4range(940, 949), 1),
    (95,  5,  1, int4range(950, 959), 1),
    (86,  6,  2, int4range(860, 869), 1),
    (96,  6,  1, int4range(960, 969), 1),
    (97,  7,  1, int4range(970, 979), 1),
    (942, 42, 1, int4range(9420, 9429), 1);

INSERT INTO
    symbol_refs(to_symbol, from_object, from_offset_range)
VALUES
    (2, 1, int4range(911, 912)),
    (4, 1, int4range(921, 922)),
    (2, 1, int4range(931, 932)),
    (5, 1, int4range(941, 942)),
    (6, 1, int4range(942, 943)),
    (6, 1, int4range(951, 952)),
    (5, 1, int4range(971, 972)),
    (1, 1, int4range(9421, 9422)),
    (2, 1, int4range(9422, 9423));


-- main -> {a c}
-- {a c} -> b
-- b -> d -> {e f}
-- g -> e -> f
