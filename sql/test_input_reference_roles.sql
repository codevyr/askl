-- References that say what they do, and one row that is not a reference.
--
--     caller  (func)   calls `hook`, takes its value, and `hook++`s it;
--                      and refers to `plain` with nothing stated.
--     hook    (data)   a function pointer.
--     ops     (type)   a struct with one field, `slot`.
--     slot    (field)  HOLDS `my_read`: a fact placed on the field's
--                      declaration, not something a token there says.
--     my_read, plain (func)
--
-- The pair caller -> hook has three references: CALL (1), TAKE (4) and
-- WRITE|TAKE (6).  What an answer says about the pair is their union.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-reference-roles'), 'hex'), 'root', TRUE);

-- What ingest would have observed of these rows: roles are stated, there is
-- a `holds` row and no `declares` row.
INSERT INTO projects (id, project_name, root_path, root_layer_id, states_roles, has_holds)
VALUES (1, 'test_project', '/test_project', 1000001, TRUE, TRUE);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'a.c', '/a.c', 'cc', 'ha');

-- function 1, type 5, data 6, field 8
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'caller',  1, 1, 1),
       (2, 'hook',    1, 6, 1),
       (3, 'slot',    1, 8, 1),
       (4, 'my_read', 1, 1, 1),
       (5, 'plain',   1, 1, 1),
       (6, 'ops',     1, 5, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (10, 1, 1, int4range(100, 500), 1),    -- caller
       (20, 2, 1, int4range(600, 620), 1),    -- hook
       (60, 6, 1, int4range(700, 900), 1),    -- ops
       (30, 3, 1, int4range(710, 730), 1),    -- slot, inside ops
       (40, 4, 1, int4range(1000, 1100), 1),  -- my_read
       (50, 5, 1, int4range(1200, 1300), 1);  -- plain

-- Inside `caller`.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (2, 1, int4range(110, 114), 1),   -- hook(x)
       (2, 1, int4range(120, 124), 4),   -- g = hook
       (2, 1, int4range(130, 134), 6);   -- hook++
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (5, 1, int4range(140, 145));      -- plain: nothing stated

-- On `slot`'s declaration: the slot holds my_read.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (4, 1, int4range(712, 716), 2);

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
