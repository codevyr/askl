SET search_path TO index, public;

INSERT INTO projects (id, project_name)
VALUES
    (1, 'test_project');

INSERT INTO modules (id, module_name, project_id)
VALUES
    (1, 'test', 1);

INSERT INTO files (id, module, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 'main.c', '/main.c', 'cc', '');

INSERT INTO symbols (id, name, module, symbol_scope)
VALUES
    (1, 'foo', 1, 1),
    (2, 'foo.bar', 1, 1),
    (3, 'foobar', 1, 1),
    (4, 'tar', 1, 1),
    (5, 'sort.IsSorted', 1, 1),
    (6, 'sort.Sort', 1, 1);

INSERT INTO declarations (id, symbol, file_id, symbol_type, start_offset, end_offset)
VALUES
    (91, 1, 1, 1, 910, 919),
    (92, 2, 1, 1, 920, 929),
    (93, 3, 1, 1, 930, 939),
    (94, 4, 1, 1, 940, 949),
    (95, 5, 1, 1, 950, 959),
    (96, 6, 1, 1, 960, 969);

INSERT INTO
    symbol_refs(to_symbol, from_file, from_offset_start, from_offset_end)
VALUES
    (2, 1, 911, 911),
    (2, 1, 912, 912),
    (3, 1, 921, 921),
    (4, 1, 922, 922);
