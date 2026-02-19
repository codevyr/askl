SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

INSERT INTO modules (id, module_name, project_id)
VALUES (1, 'test', 1);

INSERT INTO directories (id, project_id, parent_id, path)
VALUES (1, 1, NULL, '/');

INSERT INTO files (id, project_id, module, directory_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 1, 1, 'main.c', '/main.c', 'cc', '');

INSERT INTO symbols (id, name, module, symbol_scope)
VALUES
    (1,  'a',    1, 1),
    (2,  'b',    1, 1),
    (3,  'c',    1, 1),
    (4,  'd',    1, 1),
    (5,  'e',    1, 1),
    (6,  'f',    1, 1),
    (7,  'g',    1, 1),
    (42, 'main', 1, 1);

INSERT INTO declarations (id, symbol, file_id, symbol_type, offset_range)
VALUES
    (91,  1,  1, 1, int4range(910, 919)),
    (92,  2,  1, 1, int4range(920, 929)),
    (93,  3,  1, 1, int4range(930, 939)),
    (94,  4,  1, 1, int4range(940, 949)),
    (95,  5,  1, 1, int4range(950, 959)),
    (96,  6,  1, 1, int4range(960, 969)),
    (97,  7,  1, 1, int4range(970, 979)),
    (942, 42, 1, 1, int4range(9420, 9429));

INSERT INTO
    symbol_refs(to_symbol, from_file, from_offset_range)
VALUES
    (2, 1, int4range(911, 912)),
    (2, 1, int4range(912, 913)),
    (5, 1, int4range(941, 942)),
    (6, 1, int4range(942, 943)),
    (7, 1, int4range(961, 962)),
    (1, 1, int4range(9421, 9422)),
    (2, 1, int4range(9422, 9423));

-- main -> {a b}
-- a -> b
-- d -> {e f}
-- f -> g
