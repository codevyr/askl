-- Composition resolves what neither neighbour resolves alone.
--
-- The shape is "find all spinlocks in kernel module x": a container with many
-- children, a widely-referenced symbol with many parents, and a small overlap.
-- Deriving the middle statement from either side alone exceeds a small cap;
-- deriving it from BOTH resolves to one instance.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-esc'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

-- Object 1 holds the module; object 2 holds unrelated callers.
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'modx.c', '/modx.c', 'cc', 'h1'),
       (2, 1, 'other.c', '/other.c', 'cc', 'h2');

-- Module (type 3) and the widely-referenced symbol (type 1).
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'modx', 1, 3, NULL),
       (2, 'lock', 1, 1, 1);

-- Ten functions inside the module, ten outside it.
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 100 + i, 'in_' || i, 1, 1, 1 FROM generate_series(1, 10) AS i;
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 200 + i, 'out_' || i, 1, 1, 1 FROM generate_series(1, 10) AS i;

-- The module instance spans object 1, so it CONTAINS in_1..in_10 and nothing else.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (10, 1, 1, int4range(0, 10000), 5);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 1000 + i, 100 + i, 1, int4range(i * 100, i * 100 + 50), 1
FROM generate_series(1, 10) AS i;

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 2000 + i, 200 + i, 2, int4range(i * 100, i * 100 + 50), 1
FROM generate_series(1, 10) AS i;

-- `lock` is defined outside the module so containment alone never selects it.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (3000, 2, 2, int4range(9000, 9050), 1);

-- Ten callers of `lock`: in_1 (inside the module) and out_1..out_9 (outside).
-- So parents-of-lock = 10, children-of-modx = 10, and the overlap is in_1.
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (2, 1, int4range(110, 120));
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
SELECT 2, 2, int4range(i * 100 + 10, i * 100 + 20) FROM generate_series(1, 9) AS i;

-- ---------------------------------------------------------------------------
-- Mixed-type callers, for the derive-then-filter (shrink) soundness test.
-- `hot` (a function) is called by six symbols: two functions (f_1, f_2) and
-- four data symbols (d_1..d_4).  A typed derive `func { "hot" }` fetches hot's
-- callers untyped (six > cap+1 at read_cap = 3, so the read caps), then filters
-- to functions (only two).  The filtered re-read sees two rows, under the cap,
-- so the by-id re-read cannot re-detect the truncation — only the carried
-- read-signal can.  This is the case M1's re-read misses.
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (3, 1, 'hot.c', '/hot.c', 'cc', 'h3');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (3, 'hot', 1, 1, 1);
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 400 + i, 'f_' || i, 1, 1, 1 FROM generate_series(1, 2) AS i;   -- functions
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 300 + i, 'd_' || i, 1, 6, 1 FROM generate_series(1, 4) AS i;   -- data (type 6)

-- hot's own instance lives in object 3, away from its callers' ranges.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (3001, 3, 3, int4range(0, 50), 1);

-- Caller instances in object 3: two functions, then four data symbols.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 3200 + i, 400 + i, 3, int4range(i * 100, i * 100 + 50), 1
FROM generate_series(1, 2) AS i;
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 3100 + i, 300 + i, 3, int4range((i + 2) * 100, (i + 2) * 100 + 50), 1
FROM generate_series(1, 4) AS i;

-- Each caller makes one reference to `hot` (symbol 3) inside its own range.
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
SELECT 3, 3, int4range(i * 100 + 10, i * 100 + 20) FROM generate_series(1, 2) AS i;
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
SELECT 3, 3, int4range((i + 2) * 100 + 10, (i + 2) * 100 + 20)
FROM generate_series(1, 4) AS i;

-- ---------------------------------------------------------------------------
-- Out-edge fan-out, for the E1 per-node edge-cap test.  `hub` references four
-- targets (t_1..t_4); `spoke` references one (t_1).  Isolated in object 4 and
-- referencing only these fresh Type symbols, so no existing escalation query
-- (which touch modx / lock / hot and their callers) changes.
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (4, 1, 'hub.c', '/hub.c', 'cc', 'h4');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (5, 'hub', 1, 1, 1),
       (6, 'spoke', 1, 1, 1);
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
SELECT 500 + i, 't_' || i, 1, 5, 1 FROM generate_series(1, 4) AS i;  -- Type (5)

-- hub spans [0,500] so its four refs fall inside it; spoke spans [500,600].
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (4001, 5, 4, int4range(0, 500), 1),
       (4002, 6, 4, int4range(500, 600), 1);
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
SELECT 4100 + i, 500 + i, 4, int4range(1000 + i * 10, 1000 + i * 10 + 5), 1
FROM generate_series(1, 4) AS i;

-- hub -> t_1..t_4 (four out-edges); spoke -> t_1 (one).
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
SELECT 500 + i, 4, int4range(i * 10, i * 10 + 5) FROM generate_series(1, 4) AS i;
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (501, 4, int4range(520, 525));

-- ---------------------------------------------------------------------------
-- Macro expansion, for the F2 references-pagination test.  `mac` (symbol 8)
-- has a Definition instance and an EXPANSION instance (instance_type 3), each
-- containing one reference to t_1.  A symbol-keyed references listing must
-- return refs from BOTH — macro expansion sites are ordinary instances.
-- Isolated in object 5 so no existing fixture count changes.
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (5, 1, 'mac.c', '/mac.c', 'cc', 'h5');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (8, 'mac', 1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (5001, 8, 5, int4range(0, 100), 1),    -- definition
       (5002, 8, 5, int4range(200, 300), 3);  -- expansion

INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (501, 5, int4range(10, 20)),
       (501, 5, int4range(210, 220));

-- ---------------------------------------------------------------------------
-- Self-reference, for the pagination-honesty test.  `selfy` (symbol 9) has ONE
-- instance which references ITSELF (ref A) and then t_1 (ref B).  The target
-- lookup finds no other instance for ref A, so that row is dropped from the
-- response — but it must still occupy a scan position, or a page boundary
-- landing on it reports has_more = false with rows remaining (production bug).
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (6, 1, 'selfy.c', '/selfy.c', 'cc', 'h6');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (9, 'selfy', 1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (6001, 9, 6, int4range(0, 100), 1);

INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (9, 6, int4range(10, 20)),
       (501, 6, int4range(30, 40));

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
