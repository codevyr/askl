-- Role words, and what they pass over.
--
-- Two projects, because what a role word can answer depends on what the
-- index it reads STATES, and that is a fact about each upload.
--
-- Project `stated` -- its references state roles, and it has `holds` rows
-- (no `declares` rows) -- one file, /a.c:
--
--     caller  (func)  CALLs `hook` and `shared`; TAKEs `hook`'s value;
--                     `counter++` (one token, WRITE|TAKE);
--                     and names `plain` (func), `maybe` (data), `T` (type)
--                     and `M` (macro) with NOTHING STATED.
--                     It names `shared` once more, nothing stated.
--     other   (func)  names `hook`, `shared` and `stored` with nothing
--                     stated.
--     via     (func)  CALLs `slot`, and CALLs `direct`.
--     ops     (type)  a struct with one field, `slot`;
--     slot    (field) HOLDS `my_read`, `direct` and `stored`; its
--                     declaration names `plain`, nothing stated.
--     my_read (func)  held, and no token names it.
--     direct  (func)  held, and called by name too.
--     stored  (func)  held, and named only where nothing is stated.
--     M       (macro) its body CALLs one `twin` and names the other.
--     twin    (func)  two static functions of one name; the first has a
--                     declaration as well.
--     M2      (macro) its body names the `twin` that `M` calls.
--     user_of_m (func) uses `M` and `M2`.
--     noisy   (func)  names `maybe` eleven times, nothing stated.
--     hook, shared, plain (func); maybe, counter (data); T (type)
--
-- Project `silent` -- an index that states nothing: no roles, no relations
-- -- one file, /b.c:
--
--     old_caller (func)  names its own `shared` and `old_target`.
--     shared, old_target (func); OLD_DEFINE (macro); old_lock (data)

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-role-words-stated'), 'hex'), 'root', TRUE),
       (1000002, NULL, decode(md5('fixture-root-role-words-silent'), 'hex'), 'root', TRUE);

-- What ingest would have observed (`projects.states_roles` and friends are
-- set by the upload; a fixture is not uploaded, so it says so itself).
INSERT INTO projects (id, project_name, root_path, root_layer_id, states_roles, has_declares, has_holds)
VALUES (1, 'stated', '/stated', 1000001, TRUE,  FALSE, TRUE),
       (2, 'silent', '/silent', 1000002, FALSE, FALSE, FALSE);

-- ---------------------------------------------------------------- stated
ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'a.c', '/a.c', 'cc', 'ha');

-- function 1, type 5, data 6, macro 7, field 8
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1,  'caller',  1, 1, 1),
       (2,  'other',   1, 1, 1),
       (3,  'via',     1, 1, 1),
       (4,  'ops',     1, 5, 1),
       (5,  'slot',    1, 8, 1),
       (6,  'my_read', 1, 1, 1),
       (7,  'hook',    1, 1, 1),
       (8,  'shared',  1, 1, 1),
       (9,  'plain',   1, 1, 1),
       (10, 'maybe',   1, 6, 1),
       (11, 'counter', 1, 6, 1),
       (12, 'T',       1, 5, 1),
       (13, 'M',       1, 7, 1),
       (14, 'direct',  1, 1, 1),
       (15, 'stored',  1, 1, 1),
       (16, 'twin',    1, 1, 1),
       (17, 'twin',    1, 1, 1),
       (18, 'noisy',   1, 1, 1),
       (19, 'M2',      1, 7, 1),
       (20, 'user_of_m', 1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (10,  1,  1, int4range(100, 500), 1),     -- caller
       (20,  2,  1, int4range(600, 700), 1),     -- other
       (30,  3,  1, int4range(800, 900), 1),     -- via
       (40,  4,  1, int4range(1000, 1200), 1),   -- ops
       (50,  5,  1, int4range(1010, 1030), 1),   -- slot, inside ops
       (60,  6,  1, int4range(1300, 1400), 1),   -- my_read
       (70,  7,  1, int4range(1500, 1600), 1),   -- hook
       (80,  8,  1, int4range(1700, 1800), 1),   -- shared
       (90,  9,  1, int4range(1900, 2000), 1),   -- plain
       (100, 10, 1, int4range(2100, 2110), 1),   -- maybe
       (110, 11, 1, int4range(2200, 2210), 1),   -- counter
       (120, 12, 1, int4range(2300, 2310), 1),   -- T
       (130, 13, 1, int4range(2400, 2410), 1),   -- M, the #define
       (140, 14, 1, int4range(2500, 2550), 1),   -- direct
       (150, 15, 1, int4range(2600, 2650), 1),   -- stored
       (160, 16, 1, int4range(2700, 2750), 1),   -- twin (one static function)
       (170, 17, 1, int4range(2800, 2850), 1),   -- twin (another)
       (180, 18, 1, int4range(3000, 3100), 1),   -- noisy
       (190, 19, 1, int4range(3200, 3210), 1),   -- M2, the #define
       (200, 20, 1, int4range(3300, 3400), 1),   -- user_of_m
       (165, 16, 1, int4range(3500, 3510), 2);   -- twin (the first), its declaration

-- Inside `caller`: what it states ...
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (7,  1, int4range(110, 114), 1),   -- hook(x)        CALL
       (8,  1, int4range(120, 126), 1),   -- shared(x)      CALL
       (7,  1, int4range(130, 134), 4),   -- g = hook       TAKE
       (11, 1, int4range(140, 147), 6);   -- counter++      WRITE|TAKE
-- ... and what it only names.  `shared` again: the same function it calls
-- elsewhere, here in a macro argument, say.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (9,  1, int4range(200, 205)),      -- plain
       (10, 1, int4range(210, 215)),      -- maybe
       (12, 1, int4range(220, 221)),      -- T
       (13, 1, int4range(230, 231)),      -- M
       (8,  1, int4range(240, 246));      -- shared

-- Inside `other`: nothing stated.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (7,  1, int4range(610, 614)),      -- hook
       (8,  1, int4range(620, 626)),      -- shared
       (15, 1, int4range(630, 636));      -- stored

-- Inside `via`: a call through the slot, and a call of `direct` by name.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (5,  1, int4range(810, 814), 1),
       (14, 1, int4range(820, 826), 1);

-- On `slot`'s declaration: the slot holds my_read, direct and stored ...
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (6,  1, int4range(1012, 1019), 2),
       (14, 1, int4range(1026, 1027), 2),
       (15, 1, int4range(1028, 1029), 2);
-- ... and its declaration names `plain`, nothing stated.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (9, 1, int4range(1020, 1025));

-- In the body of `M`: it calls one `twin` and names the other.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (16, 1, int4range(2402, 2404), 1);
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (17, 1, int4range(2405, 2407));

-- In the body of `M2`: it names the `twin` that `M` calls.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (16, 1, int4range(3202, 3204));

-- Inside `user_of_m`: it uses both macros (a use states nothing).
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (13, 1, int4range(3310, 3311)),
       (19, 1, int4range(3320, 3322));

-- Inside `noisy`: eleven mentions of `maybe`, nothing stated.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 10, 1, int4range(3010 + 4 * i, 3012 + 4 * i) FROM generate_series(0, 10) AS i;

-- ---------------------------------------------------------------- silent
ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000002;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000002;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000002;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000002;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 2, 'b.c', '/b.c', 'cc', 'hb');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (101, 'old_caller', 2, 1, 1),
       (102, 'shared',     2, 1, 1),
       (103, 'old_target', 2, 1, 1),
       (104, 'OLD_DEFINE', 2, 7, 1),
       (105, 'old_lock',   2, 6, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (1010, 101, 2, int4range(100, 300), 1),   -- old_caller
       (1020, 102, 2, int4range(400, 500), 1),   -- shared
       (1030, 103, 2, int4range(600, 700), 1),   -- old_target
       (1040, 104, 2, int4range(800, 850), 1),   -- OLD_DEFINE
       (1050, 105, 2, int4range(900, 910), 1);   -- old_lock

INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (102, 2, int4range(110, 116)),     -- shared(x): a call nobody recorded
       (103, 2, int4range(120, 130));     -- old_target
-- ... and `old_target` eleven times more: more references than the one-row
-- look at a silent index has a budget for.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
SELECT 103, 2, int4range(140 + 12 * i, 150 + 12 * i) FROM generate_series(0, 10) AS i;

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
