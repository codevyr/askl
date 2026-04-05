SET search_path TO index, public;

INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

-- Object 1: source file
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'file_x.c', '/file_x.c', 'c', 'hash1');

-- Object 2: sentinel directory
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 1, '/', '/', 'directory', '');

-- Symbols
-- Directory (type=4)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (1, 'dir_root', 1, 4, NULL);

-- File (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (2, 'file_x', 1, 2, NULL);

-- Function (type=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (3, 'func_a', 1, 1, NULL),
    (4, 'func_b', 1, 1, NULL);

-- Macro (type=7)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (5, 'macro_m', 1, 7, NULL);

-- Data (type=6)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (6, 'data_d', 1, 6, NULL),
    (7, 'data_macro_only', 1, 6, NULL);

-- Symbol instances
-- dir_root: sentinel instance on directory object
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (100, 1, 2, int4range(0, 0), 4);

-- dir_root: containment instance on source file [0, 20000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (101, 1, 1, int4range(0, 20000), 5);

-- file_x [0, 10000)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (102, 2, 1, int4range(0, 10000), 6);

-- func_a [100, 700)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (103, 3, 1, int4range(100, 700), 1);

-- func_b [800, 900)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (104, 4, 1, int4range(800, 900), 1);

-- macro_m [500, 600) — inside func_a
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (105, 5, 1, int4range(500, 600), 1);

-- data_d [300, 400) — inside func_a
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (106, 6, 1, int4range(300, 400), 1);

-- data_macro_only [510, 550) — inside macro_m, inside func_a
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (107, 7, 1, int4range(510, 550), 1);

-- References
-- func_a refs data_d (from inside func_a's range)
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (6, 1, int4range(150, 160));

-- func_a refs macro_m (from inside func_a's range)
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (5, 1, int4range(200, 210));
