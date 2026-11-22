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
--   and, for what a listing passes over when its parent calls through a
--   slot (nothing above names any of these):
--
--     multi   (func)  CALLs six slots of `ops2` and CALLs `helper` by name;
--                     names a seventh slot, `quiet`, with nothing stated;
--                     and CALLs `fp`, a variable.
--     ops2    (type)  a struct with seven fields:
--       slot_b  HOLDS `b_read` and `b_other`;   slot_c  HOLDS `c_read`;
--       slot_e  HOLDS `e_read`;                 slot_f  HOLDS `f_read`;
--       slot_d  HOLDS `helper`, which `multi` calls by name as well;
--       idle    holds nothing (its declaration names `leaf`);
--       quiet   HOLDS `q_read` (and is never CALLed).
--     helper, b_read (func)  each CALLs `leaf`.
--     b_other, c_read, e_read, f_read, q_read, leaf (func)
--     fp      (data)  a function pointer that is no field; HOLDS `g_read`.
--     g_read  (func)
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

-- A function that calls through several slots.
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (21, 'multi',   1, 1, 1),
       (22, 'ops2',    1, 5, 1),
       (23, 'slot_b',  1, 8, 1),
       (24, 'slot_c',  1, 8, 1),
       (25, 'slot_d',  1, 8, 1),
       (26, 'slot_e',  1, 8, 1),
       (27, 'slot_f',  1, 8, 1),
       (28, 'idle',    1, 8, 1),
       (29, 'quiet',   1, 8, 1),
       (30, 'helper',  1, 1, 1),
       (31, 'b_read',  1, 1, 1),
       (32, 'b_other', 1, 1, 1),
       (33, 'c_read',  1, 1, 1),
       (34, 'e_read',  1, 1, 1),
       (35, 'f_read',  1, 1, 1),
       (36, 'q_read',  1, 1, 1),
       (37, 'leaf',    1, 1, 1),
       (38, 'fp',      1, 6, 1),
       (39, 'g_read',  1, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (210, 21, 1, int4range(4000, 4200), 1),   -- multi
       (220, 22, 1, int4range(4300, 4500), 1),   -- ops2
       (230, 23, 1, int4range(4310, 4320), 1),   -- slot_b, inside ops2
       (240, 24, 1, int4range(4330, 4340), 1),   -- slot_c
       (250, 25, 1, int4range(4350, 4360), 1),   -- slot_d
       (260, 26, 1, int4range(4370, 4380), 1),   -- slot_e
       (270, 27, 1, int4range(4390, 4400), 1),   -- slot_f
       (280, 28, 1, int4range(4410, 4420), 1),   -- idle
       (290, 29, 1, int4range(4430, 4440), 1),   -- quiet
       (300, 30, 1, int4range(4600, 4650), 1),   -- helper
       (310, 31, 1, int4range(4700, 4750), 1),   -- b_read
       (320, 32, 1, int4range(4800, 4850), 1),   -- b_other
       (330, 33, 1, int4range(4900, 4950), 1),   -- c_read
       (340, 34, 1, int4range(5000, 5050), 1),   -- e_read
       (350, 35, 1, int4range(5100, 5150), 1),   -- f_read
       (360, 36, 1, int4range(5200, 5250), 1),   -- q_read
       (370, 37, 1, int4range(5300, 5350), 1),   -- leaf
       (380, 38, 1, int4range(5400, 5410), 1),   -- fp
       (390, 39, 1, int4range(5500, 5550), 1);   -- g_read

-- Inside `multi`: six calls through a slot, one call by name ...
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (23, 1, int4range(4010, 4016), 1),  -- ops->slot_b(x)   CALL
       (24, 1, int4range(4020, 4026), 1),  -- ops->slot_c(x)   CALL
       (25, 1, int4range(4030, 4036), 1),  -- ops->slot_d(x)   CALL
       (26, 1, int4range(4040, 4046), 1),  -- ops->slot_e(x)   CALL
       (27, 1, int4range(4050, 4056), 1),  -- ops->slot_f(x)   CALL
       (28, 1, int4range(4060, 4064), 1),  -- ops->idle(x)     CALL
       (30, 1, int4range(4070, 4076), 1),  -- helper(x)        CALL
       (38, 1, int4range(4090, 4092), 1);  -- fp(x)            CALL
-- ... and a slot it only names.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (29, 1, int4range(4080, 4085));     -- quiet

-- On the slots' declarations: what each holds.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, relation)
VALUES (31, 1, int4range(4311, 4312), 2),  -- slot_b holds b_read
       (32, 1, int4range(4313, 4314), 2),  -- slot_b holds b_other
       (33, 1, int4range(4331, 4332), 2),  -- slot_c holds c_read
       (30, 1, int4range(4351, 4352), 2),  -- slot_d holds helper
       (34, 1, int4range(4371, 4372), 2),  -- slot_e holds e_read
       (35, 1, int4range(4391, 4392), 2),  -- slot_f holds f_read
       (36, 1, int4range(4431, 4432), 2),  -- quiet holds q_read
       (39, 1, int4range(5401, 5402), 2);  -- fp holds g_read
-- `idle` holds nothing; its declaration names `leaf`, an ordinary reference.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range)
VALUES (37, 1, int4range(4411, 4415));

-- `helper` and `b_read` each call `leaf`.
INSERT INTO symbol_refs (to_symbol, from_object, from_offset_range, roles)
VALUES (37, 1, int4range(4610, 4614), 1),
       (37, 1, int4range(4710, 4714), 1);

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
