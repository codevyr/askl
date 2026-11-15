-- One token read inside two declarations.
--
--     #define FAMILY(name, fn)                       \
--         int name##_a(int k) { return fn(k); }      \
--         int name##_b(int k) { return fn(k) + other(k); }
--     FAMILY(gen, target)
--
-- `gen_a` and `gen_b` are generated on ONE extent, and each calls `target` on
-- the one `target` the caller typed.  That is two references that differ only
-- in where they sit, which ranges cannot express -- so this fixture numbers
-- its own tree instead of leaving it to `number_fixture_tree`.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-refid'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'fam.c', '/fam.c', 'cc', 'h1');

-- The container (a module, type 3) and four functions (type 1).
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'fam',    1, 3, NULL),
       (2, 'gen_a',  1, 1, 1),
       (3, 'gen_b',  1, 1, 1),
       (4, 'target', 1, 1, 1),
       (5, 'other',  1, 1, 1);

--   fam    [0,1000)   (1024, 11264)
--     target [0,50)     (2048, 3072)
--     other  [50,90)    (4096, 5120)
--     gen_a  [100,200)  (6144, 7168)
--     gen_b  [100,200)  (8192, 9216)
INSERT INTO symbol_instances
    (id, symbol, object_id, offset_range, instance_type, nesting_level, dfs_enter, dfs_exit)
VALUES (10, 1, 1, int4range(0, 1000),  5, 0, 1024, 11264),
       (11, 4, 1, int4range(0, 50),    1, 1, 2048,  3072),
       (12, 5, 1, int4range(50, 90),   1, 1, 4096,  5120),
       (13, 2, 1, int4range(100, 200), 1, 1, 6144,  7168),
       (14, 3, 1, int4range(100, 200), 1, 1, 8192,  9216);

-- `target` at [150,156): once in gen_a, once in gen_b.
-- `other`  at [160,165): in gen_b only.
INSERT INTO symbol_refs (id, to_symbol, from_object, from_offset_range, nesting_level, dfs_enter)
VALUES (101, 4, 1, int4range(150, 156), 2, 6145),
       (102, 4, 1, int4range(150, 156), 2, 8193),
       (103, 5, 1, int4range(160, 165), 2, 8193);
