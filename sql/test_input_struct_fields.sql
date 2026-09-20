SET search_path TO index, public;

-- Struct/field fixture for the REFS direct-only level rule.
--
-- Levels (index.symbol_types): field=1, type=2, function=2, file=3, directory=5.
-- A struct's references all live *inside* its fields, so a `type` parent (level 2)
-- only sees them if a narrower `field` container (level 1) does not shadow it.
-- `Wrapper` is the control: a `type` nested in a `type` is at the *same* level and
-- must still shadow, so the direct-only filter cannot simply be dropped.

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-1'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

-- Object 1: /types.h content; object 2: sentinel for "/"
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'types.h', '/types.h', 'c', 'hash1');
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 1, '/', '/', 'directory', '');

-- Directory (type=4) and file (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (50, '/', 1, 4, NULL),
    (51, '/types.h', 1, 2, NULL);

-- Types (type=5, level=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (1, 'Outer', 1, 5, NULL),
    (4, 'Inner', 1, 5, NULL),
    (6, 'Wrapper', 1, 5, NULL),
    (7, 'Wrapper.Nested', 1, 5, NULL);

-- Fields (type=8, level=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (2, 'Outer.a', 1, 8, NULL),
    (3, 'Outer.b', 1, 8, NULL);

-- Function (type=1, level=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (5, 'Helper', 1, 1, NULL);

-- Directory sentinel + containment, file source instance
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (500, 50, 2, int4range(0, 0), 4),
    (501, 50, 1, int4range(0, 2000), 5),
    (510, 51, 1, int4range(0, 2000), 6);

-- struct Outer { Inner a; ... b; };  fields nest inside the struct
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (10, 1, 1, int4range(100, 400), 1),   -- Outer          [100, 400)
    (20, 2, 1, int4range(150, 200), 1),   -- Outer.a        [150, 200)
    (30, 3, 1, int4range(250, 300), 1),   -- Outer.b        [250, 300)
    (40, 4, 1, int4range(500, 600), 1),   -- Inner          [500, 600)
    (45, 5, 1, int4range(700, 800), 1),   -- Helper         [700, 800)
    (60, 6, 1, int4range(1000, 1400), 1), -- Wrapper        [1000, 1400)
    (70, 7, 1, int4range(1100, 1200), 1); -- Wrapper.Nested [1100, 1200)

INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range) VALUES
    (4, 1, int4range(170, 180)),    -- Outer.a names Inner  (field inside Outer)
    (5, 1, int4range(260, 270)),    -- Outer.b names Helper (field inside Outer)
    (4, 1, int4range(1150, 1160));  -- Wrapper.Nested names Inner (type inside type)

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
