SET search_path TO index, public;

-- Project setup
INSERT INTO projects (id, project_name, root_path)
VALUES (1, 'test_project', '/test_project');

-- File content objects
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES
    (1, 1, 'main.go', '/src/main.go', 'go', 'hash1'),
    (2, 1, 'util.go', '/src/util/util.go', 'go', 'hash2'),
    (3, 1, 'helper.go', '/src/util/helper.go', 'go', 'hash3'),
    (4, 1, 'config.go', '/src/config/config.go', 'go', 'hash4'),
    (5, 1, 'readme.md', '/docs/readme.md', 'md', 'hash5');

-- Sentinel objects for directories (one per directory, zero content)
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES
    (100, 1, '/', '/', 'directory', ''),
    (101, 1, '/src', '/src', 'directory', ''),
    (102, 1, '/docs', '/docs', 'directory', ''),
    (103, 1, '/src/util', '/src/util', 'directory', ''),
    (104, 1, '/src/config', '/src/config', 'directory', '');

-- Directory symbols (type=4)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (100, '/', 1, 4, NULL),
    (101, '/src', 1, 4, NULL),
    (102, '/docs', 1, 4, NULL),
    (103, '/src/util', 1, 4, NULL),
    (104, '/src/config', 1, 4, NULL);

-- File symbols (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (201, '/src/main.go', 1, 2, NULL),
    (202, '/src/util/util.go', 1, 2, NULL),
    (203, '/src/util/helper.go', 1, 2, NULL),
    (204, '/src/config/config.go', 1, 2, NULL),
    (205, '/docs/readme.md', 1, 2, NULL);

-- Directory self-instances on sentinel objects [0, 0)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (1000, 100, 100, int4range(0, 0), 4),
    (1010, 101, 101, int4range(0, 0), 4),
    (1020, 102, 102, int4range(0, 0), 4),
    (1030, 103, 103, int4range(0, 0), 4),
    (1040, 104, 104, int4range(0, 0), 4);

-- Directory instances on direct child files (for containment queries).
-- "/src" claims /src/main.go
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (1011, 101, 1, int4range(0, 1000), 5);

-- "/docs" claims /docs/readme.md
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (1021, 102, 5, int4range(0, 1000), 5);

-- "/src/util" claims util.go and helper.go
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (1031, 103, 2, int4range(0, 1000), 5),
    (1032, 103, 3, int4range(0, 1000), 5);

-- "/src/config" claims config.go
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (1041, 104, 4, int4range(0, 1000), 5);

-- File instances (each file on its own content object)
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (2010, 201, 1, int4range(0, 1000), 6),
    (2020, 202, 2, int4range(0, 1000), 6),
    (2030, 203, 3, int4range(0, 1000), 6),
    (2040, 204, 4, int4range(0, 1000), 6),
    (2050, 205, 5, int4range(0, 1000), 6);

-- Directory→directory hierarchy via symbol_refs.
-- from_object = parent's sentinel object.
-- "/" → /src and /docs
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (101, 100, int4range(0, 0)),
    (102, 100, int4range(0, 0));

-- /src → /src/util and /src/config
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (103, 101, int4range(0, 0)),
    (104, 101, int4range(0, 0));

-- Expected directory structure:
-- /
-- ├── src/
-- │   ├── main.go
-- │   ├── util/
-- │   │   ├── util.go
-- │   │   └── helper.go
-- │   └── config/
-- │       └── config.go
-- └── docs/
--     └── readme.md

-- Expected list_project_tree results (uses name-based queries, not instances):
-- list_project_tree("/") -> ["/src" (dir), "/docs" (dir)]
-- list_project_tree("/src") -> ["/src/util" (dir), "/src/config" (dir), "/src/main.go" (file)]
-- list_project_tree("/src/util") -> ["/src/util/util.go" (file), "/src/util/helper.go" (file)]
-- list_project_tree("/docs") -> ["/docs/readme.md" (file)]
