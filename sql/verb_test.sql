SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES
    (1, 'test_project', '/test_project');

INSERT INTO modules (id, module_name, project_id)
VALUES
    (1, 'test', 1);

INSERT INTO files (id, project_id, module, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 1, 'main.c', '/main.c', 'cc', '');

INSERT INTO symbols (id, name, module, symbol_scope)
VALUES
    (1, 'foo', 1, 1),
    (2, 'foo.bar', 1, 1),
    (3, 'foobar', 1, 1),
    (4, 'tar', 1, 1),
    (5, 'sort.IsSorted', 1, 1),
    (6, 'sort.Sort', 1, 1);

INSERT INTO declarations (id, symbol, file_id, symbol_type, offset_range)
VALUES
    (91, 1, 1, 1, int4range(910, 919)),
    (92, 2, 1, 1, int4range(920, 929)),
    (93, 3, 1, 1, int4range(930, 939)),
    (94, 4, 1, 1, int4range(940, 949)),
    (95, 5, 1, 1, int4range(950, 959)),
    (96, 6, 1, 1, int4range(960, 969));

INSERT INTO
    symbol_refs(to_symbol, from_file, from_offset_range)
VALUES
    (2, 1, int4range(911, 912)),
    (2, 1, int4range(912, 913)),
    (3, 1, int4range(921, 922)),
    (4, 1, int4range(922, 923));
