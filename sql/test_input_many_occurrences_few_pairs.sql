-- Many occurrences between few rows.
--
--     flood    a function that refers to `knob` 250 times, to `dial` 3 times
--              and to `hid` once.
--     quiet    a function that refers to `knob` once and to `only_q` once.
--     torrent  a function that refers to `knob` 1,700 times.
--     brook    a function that refers to `dial` 1,700 times.
--     knob, dial, spare, only_q   variables; nothing refers to `spare`.
--     hid      a folded variable (a listing leaves it out).
--     /big.c   the file that holds them all.
--
-- The lists between two statements here are long and the statements short:
-- `"flood" "quiet" { "knob" "spare" }` is two rows over two rows with 251
-- reference occurrences between them, so a list cut at a small limit holds
-- `flood -> knob` over and over and never reaches `quiet -> knob`.  Which
-- rows relate is still a small question: both functions, and `knob`.
--
-- `torrent` and `brook` make the references on either side of one question
-- more than an ask may read at a small cap.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-many-occurrences-few-pairs'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'big.c', '/big.c', 'c', 'h1');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (51, '/big.c', 1, 2, NULL),   -- file
       (1, 'flood', 1, 1, NULL),     -- functions
       (2, 'quiet', 1, 1, NULL),
       (3, 'torrent', 1, 1, NULL),
       (4, 'brook', 1, 1, NULL),
       (11, 'knob', 1, 6, NULL),     -- data
       (12, 'dial', 1, 6, NULL),
       (13, 'spare', 1, 6, NULL),
       (14, 'only_q', 1, 6, NULL),
       (15, 'hid', 1, 6, NULL);

UPDATE symbols SET folded = TRUE WHERE id = 15;

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (510, 51, 1, int4range(0, 200000), 6),     -- the file
       (10, 1, 1, int4range(10000, 20000), 1),    -- flood
       (20, 2, 1, int4range(30000, 31000), 1),    -- quiet
       (30, 3, 1, int4range(40000, 80000), 1),    -- torrent
       (40, 4, 1, int4range(90000, 130000), 1),   -- brook
       (110, 11, 1, int4range(100, 110), 1),      -- knob
       (120, 12, 1, int4range(200, 210), 1),      -- dial
       (130, 13, 1, int4range(300, 310), 1),      -- spare
       (140, 14, 1, int4range(400, 410), 1),      -- only_q
       (150, 15, 1, int4range(500, 510), 1);      -- hid

-- flood: 250 references to knob, 3 to dial, 1 to hid.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 11, 1, int4range(10000 + i * 10, 10004 + i * 10) FROM generate_series(1, 250) AS i;
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 12, 1, int4range(15000 + i * 10, 15004 + i * 10) FROM generate_series(1, 3) AS i;
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (15, 1, int4range(16000, 16004));
-- quiet: one to knob, one to only_q.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (11, 1, int4range(30010, 30014)),
       (14, 1, int4range(30020, 30024));
-- torrent: 1,700 to knob.  brook: 1,700 to dial.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 11, 1, int4range(40000 + i * 10, 40004 + i * 10) FROM generate_series(1, 1700) AS i;
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 12, 1, int4range(90000 + i * 10, 90004 + i * 10) FROM generate_series(1, 1700) AS i;

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
