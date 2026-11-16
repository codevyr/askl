-- A parent whose children are mostly hidden, and one whose children are all
-- listed.
--
--     GEN   refers to `shown` and to twelve folded symbols hid_1..hid_12
--           (a macro and the declarations its invocations generated);
--     wide  refers to twelve listed symbols vis_1..vis_12;
--     host  contains `inner` and twelve folded declarations g_1..g_12.
--
-- `"GEN" { }` lists one child.  The twelve it does not list are hidden by
-- what they are, not cut by a limit, and must not be reported as an
-- incomplete answer.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-hidden-children'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'gen.c', '/gen.c', 'cc', 'h1');

-- The container (a module, type 3); everything else is a function (type 1).
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'genmod', 1, 3, NULL),
       (2, 'GEN',    1, 1, 1),
       (3, 'shown',  1, 1, 1),
       (4, 'wide',   1, 1, 1),
       (5, 'host',   1, 1, 1),
       (6, 'inner',  1, 1, 1);
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 100 + i, 'hid_' || i, 1, 1, 1 FROM generate_series(1, 12) AS i;
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 200 + i, 'vis_' || i, 1, 1, 1 FROM generate_series(1, 12) AS i;
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 300 + i, 'g_' || i, 1, 1, 1 FROM generate_series(1, 12) AS i;

UPDATE symbols SET folded = TRUE WHERE id BETWEEN 101 AND 112 OR id BETWEEN 301 AND 312;

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (10, 1, 1, int4range(0, 100000), 5),
       (20, 2, 1, int4range(100, 1000), 1),    -- GEN
       (40, 4, 1, int4range(1000, 2000), 1),   -- wide
       (50, 5, 1, int4range(2000, 4000), 1),   -- host
       (60, 6, 1, int4range(2010, 2050), 1),   -- inner, in host
       (30, 3, 1, int4range(5000, 5050), 1);   -- shown

-- g_1..g_12 sit inside host.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 3000 + i, 300 + i, 1, int4range(2100 + i * 100, 2150 + i * 100), 1
FROM generate_series(1, 12) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 1000 + i, 100 + i, 1, int4range(6000 + i * 100, 6050 + i * 100), 1
FROM generate_series(1, 12) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 2000 + i, 200 + i, 1, int4range(8000 + i * 100, 8050 + i * 100), 1
FROM generate_series(1, 12) AS i;

-- GEN: one reference to `shown`, twelve to the hidden symbols.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (3, 1, int4range(110, 115));
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 100 + i, 1, int4range(120 + i * 10, 125 + i * 10) FROM generate_series(1, 12) AS i;

-- wide: twelve references, all to listed symbols.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 200 + i, 1, int4range(1020 + i * 10, 1025 + i * 10) FROM generate_series(1, 12) AS i;
