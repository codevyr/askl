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

-- ============================================================================
-- Data inheritance pruning test data
-- Models: driver → id_table → {info_a, info_b} → {config_a, config_b} → {channels_a, channels_b}
-- Query: data(inherit="true") "driver" {{{{"channels_a"}}}}
-- Expected: only the path driver → id_table → info_a → config_a → channels_a
-- ============================================================================

-- Data symbols (type=6): driver chain
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (20, 'driver',     1, 6, 2),
    (21, 'id_table',   1, 6, 2),
    (22, 'info_a',     1, 6, 2),
    (23, 'info_b',     1, 6, 2),
    (24, 'config_a',   1, 6, 2),
    (25, 'config_b',   1, 6, 2),
    (26, 'channels_a', 1, 6, 2),
    (27, 'channels_b', 1, 6, 2);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range)
VALUES
    (200, 20, 1, int4range(100, 120)),
    (210, 21, 1, int4range(200, 300)),
    (220, 22, 1, int4range(300, 320)),
    (230, 23, 1, int4range(320, 340)),
    (240, 24, 1, int4range(340, 360)),
    (250, 25, 1, int4range(360, 380)),
    (260, 26, 1, int4range(380, 400)),
    (270, 27, 1, int4range(400, 420));

-- driver refs id_table
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (21, 1, int4range(110, 115));

-- id_table refs info_a AND info_b
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES
    (22, 1, int4range(210, 215)),
    (23, 1, int4range(220, 225));

-- info_a refs config_a, info_b refs config_b
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES
    (24, 1, int4range(310, 315)),
    (25, 1, int4range(330, 335));

-- config_a refs channels_a, config_b refs channels_b
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES
    (26, 1, int4range(350, 355)),
    (27, 1, int4range(370, 375));
