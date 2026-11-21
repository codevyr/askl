-- One function that refers many times to a symbol with several instances.
--
--     user   (a.c)             refers to `twin` four times and to `solo` once.
--     twin   (b.h, c.h, d.h)   ONE symbol with three instances.
--     solo   (a.c)
--
-- What `user` refers to is four instances: twin's three and solo.  A read
-- that joins each reference to every instance of its target sees thirteen
-- rows for them (4 x 3 + 1).

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-many-refs-one-target'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'a.c', '/a.c', 'cc', 'ha'),
       (2, 1, 'b.h', '/b.h', 'cc', 'hb'),
       (3, 1, 'c.h', '/c.h', 'cc', 'hc'),
       (4, 1, 'd.h', '/d.h', 'cc', 'hd');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'user', 1, 1, 1),
       (2, 'twin', 1, 1, 1),
       (3, 'solo', 1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (10, 1, 1, int4range(100, 500), 1),   -- user
       (20, 2, 2, int4range(10, 20), 1),     -- twin, three times
       (21, 2, 3, int4range(10, 20), 1),
       (22, 2, 4, int4range(10, 20), 1),
       (30, 3, 1, int4range(600, 700), 1);   -- solo

-- Inside `user`: four references to `twin`, one to `solo`.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 2, 1, int4range(100 + i * 10, 105 + i * 10) FROM generate_series(1, 4) AS i;
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (3, 1, int4range(200, 205));

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
