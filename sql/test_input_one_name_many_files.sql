-- One name written in many files.
--
--     /drv_1.c .. /drv_12.c   twelve files of one module, `drivers`.
--     init_module             ONE symbol with twelve instances, one per file
--                             (C has one symbol per name).
--     probe_1 .. probe_12     twelve symbols, one per file.
--
-- `file("/drv_3.c") has { "init_module" }` has one answer, the instance in
-- that file, although the name alone has twelve.  The mirror: the module
-- `drivers` has an instance in every file, and `mod("drivers") has
-- { "probe_3" }` has one answer too.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-one-name-many-files'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
SELECT i, 1, 'drv_' || i || '.c', '/drv_' || i || '.c', 'cc', 'h' || i
FROM generate_series(1, 12) AS i;

-- The module (type 3), the files (type 2), the functions (type 1).
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'drivers',     1, 3, NULL),
       (2, 'init_module', 1, 1, 1);
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 100 + i, '/drv_' || i || '.c', 1, 2, NULL FROM generate_series(1, 12) AS i;
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 300 + i, 'probe_' || i, 1, 1, 1 FROM generate_series(1, 12) AS i;

-- In file i: the file 100+i, the module 500+i, init_module 200+i, probe_i 300+i.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 100 + i, 100 + i, i, int4range(0, 1000), 6 FROM generate_series(1, 12) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 500 + i, 1, i, int4range(0, 1000), 5 FROM generate_series(1, 12) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 200 + i, 2, i, int4range(100, 200), 1 FROM generate_series(1, 12) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 300 + i, 300 + i, i, int4range(300, 400), 1 FROM generate_series(1, 12) AS i;

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
