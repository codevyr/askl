SET search_path TO index, public;

-- With modules as symbols, we test project filtering by having multiple projects.
-- Module filtering now works by symbol name matching (modules are symbols with type=MODULE).

INSERT INTO projects (id, project_name, root_path)
VALUES
    (1, 'test_project', '/test_project'),
    (2, 'other_project', '/other_project');

-- directories table has been removed - directories are now symbols

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 'main.c', '/main.c', 'cc', ''),
    (2, 1, 'bar.c', '/bar.c', 'cc', ''),
    (3, 1, 'main.c', '/other_main.c', 'cc', ''),
    (4, 2, 'main.c', '/project_only_main.c', 'cc', '');

-- File symbols (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (500, '/main.c', 1, 2, NULL),
    (501, '/bar.c', 1, 2, NULL),
    (502, '/other_main.c', 1, 2, NULL),
    (503, '/project_only_main.c', 2, 2, NULL);

-- Directory symbols (type=4)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (510, '/', 1, 4, NULL),
    (511, '/', 2, 4, NULL);

-- File symbol instances
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (5001, 500, 1, int4range(0, 10000), 6),
    (5011, 501, 2, int4range(0, 10000), 6),
    (5021, 502, 3, int4range(0, 10000), 6),
    (5031, 503, 4, int4range(0, 10000), 6);

-- Directory symbol instances
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (5100, 510, 1, int4range(0, 10000), 5),
    (5101, 510, 2, int4range(0, 10000), 5),
    (5102, 510, 3, int4range(0, 10000), 5),
    (5110, 511, 4, int4range(0, 10000), 5);

-- Symbols are now project-scoped. For the "module filter" tests, we use symbol names
-- that include a module-like prefix (e.g., "test.a", "other.a") to simulate modules.
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (1,   'test.a',    1, 1, 1),
    (2,   'test.b',    1, 1, 1),
    (3,   'test.c',    1, 1, 1),
    (4,   'test.d',    1, 1, 1),
    (5,   'test.e',    1, 1, 1),
    (6,   'test.f',    1, 1, 1),
    (7,   'test.g',    1, 1, 1),
    (42,  'test.main', 1, 1, 1),
    (101, 'other.a',    1, 1, 1),
    (102, 'other.b',    1, 1, 1),
    (142, 'other.main', 1, 1, 1),
    (301, 'project_only.a',    2, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (91,  1,   1, int4range(910, 919), 1),
    (92,  2,   1, int4range(920, 929), 1),
    (93,  3,   1, int4range(930, 939), 1),
    (94,  4,   1, int4range(940, 949), 1),
    (95,  5,   1, int4range(950, 959), 1),
    (86,  6,   2, int4range(860, 869), 1),
    (96,  6,   1, int4range(960, 969), 1),
    (97,  7,   1, int4range(970, 979), 1),
    (942, 42,  1, int4range(9420, 9429), 1),
    (201, 101, 3, int4range(2010, 2019), 1),
    (202, 102, 3, int4range(2020, 2029), 1),
    (242, 142, 3, int4range(2420, 2429), 1),
    (301, 301, 4, int4range(3010, 3019), 1);

INSERT INTO
    symbol_refs(to_symbol, from_object, from_offset_range)
VALUES
    (2,   1, int4range(911, 912)),
    (4,   1, int4range(921, 922)),
    (2,   1, int4range(931, 932)),
    (5,   1, int4range(941, 942)),
    (6,   1, int4range(942, 943)),
    (6,   1, int4range(951, 952)),
    (5,   1, int4range(971, 972)),
    (1,   1, int4range(9421, 9422)),
    (2,   1, int4range(9422, 9423)),
    (102, 3, int4range(2011, 2012)),
    (101, 3, int4range(2421, 2422));

-- "test" symbols has the same layout as test_input_b.
-- "other" symbols mirrors a subset of the data to exercise module-like filtering.
-- "project_only" symbols belongs to a separate project to exercise project filtering.
