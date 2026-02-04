SET search_path TO index, public;

INSERT INTO projects (id, project_name)
VALUES
    (1, 'test_project'),
    (2, 'other_project');

INSERT INTO modules (id, module_name, project_id)
VALUES
    (1, 'test', 1),
    (2, 'other', 1),
    (3, 'project_only', 2);

INSERT INTO files (id, module, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 'main.c', '/main.c', 'cc', ''),
    (2, 1, 'bar.c', '/bar.c', 'cc', ''),
    (3, 2, 'main.c', '/other_main.c', 'cc', ''),
    (4, 3, 'main.c', '/project_only_main.c', 'cc', '');

INSERT INTO symbols (id, name, module, symbol_scope)
VALUES
    (1,   'a',    1, 1),
    (2,   'b',    1, 1),
    (3,   'c',    1, 1),
    (4,   'd',    1, 1),
    (5,   'e',    1, 1),
    (6,   'f',    1, 1),
    (7,   'g',    1, 1),
    (42,  'main', 1, 1),
    (101, 'a',    2, 1),
    (102, 'b',    2, 1),
    (142, 'main', 2, 1),
    (301, 'a',    3, 1);

INSERT INTO declarations (id, symbol, file_id, symbol_type, offset_range)
VALUES
    (91,  1,   1, 1, int4range(910, 919)),
    (92,  2,   1, 1, int4range(920, 929)),
    (93,  3,   1, 1, int4range(930, 939)),
    (94,  4,   1, 1, int4range(940, 949)),
    (95,  5,   1, 1, int4range(950, 959)),
    (86,  6,   2, 1, int4range(860, 869)),
    (96,  6,   1, 1, int4range(960, 969)),
    (97,  7,   1, 1, int4range(970, 979)),
    (942, 42,  1, 1, int4range(9420, 9429)),
    (201, 101, 3, 1, int4range(2010, 2019)),
    (202, 102, 3, 1, int4range(2020, 2029)),
    (242, 142, 3, 1, int4range(2420, 2429)),
    (301, 301, 4, 1, int4range(3010, 3019));

INSERT INTO
    symbol_refs(to_symbol, from_file, from_offset_range)
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

-- Module "test" has the same layout as test_input_b.
-- Module "other" mirrors a subset of the data to exercise module filtering.
-- Module "project_only" belongs to a separate project to exercise project filtering.
