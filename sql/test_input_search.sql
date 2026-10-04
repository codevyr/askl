-- Fixture for `search(...)` verb tests.
--
-- Two projects, each with a file pointing into content_store.  Content is
-- chosen so the same hashes cover the cases the test matrix exercises:
--   * basic single match               -> "hello foo world\n"           (proj 1, obj 1)
--   * multi-match in one file          -> "foo foo foo\n"               (proj 1, obj 2)
--   * substring vs whole-word toggle   -> "foobar foo foo_bar foo.bar\n" (proj 1, obj 3)
--   * mixed case                       -> "Foo FOO foo\n"               (proj 2, obj 4)
--   * symbol-less file (no symbols/instances created for it) -> "doc-only content with foo here\n" (proj 2, obj 5)
--   * binary blob (non-UTF-8)          -> bytes containing \xfffoo\xff   (proj 2, obj 6)
--
-- Each content row is registered with a stable, hand-picked content_hash so
-- the fixture is self-contained.  The objects ROW points at the content via
-- content_hash; that's the join the search query uses.
--
-- Objects 1-4 have at least one symbol/instance so they participate in
-- composite filters that constrain through symbol_instances if a future
-- filter introduces such a join.  Object 5 is deliberately symbol-less to
-- prove search() reaches it (content_store ⋈ objects only).

SET search_path TO index, public;

-- Root layers for the fixture projects.  Persistent inserts inherit
-- layer via the column DEFAULT, switched per project section.
INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES
    (1000001, NULL, decode(md5('fixture-root-1'), 'hex'), 'root', TRUE),
    (1000002, NULL, decode(md5('fixture-root-2'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id) VALUES
    (1, 'search_proj_1', '/p1', 1000001),
    (2, 'search_proj_2', '/p2', 1000002);

-- Project 1 section: symbols/instances below inherit project 1's root layer.
ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO content_store (content_hash, content) VALUES
    ('cs_basic',      E'hello foo world\n'),
    ('cs_multi',      E'foo foo foo\n'),
    ('cs_boundary',   E'foobar foo foo_bar foo.bar\n'),
    ('cs_mixedcase',  E'Foo FOO foo\n'),
    ('cs_docless',    E'doc-only content with foo here\n'),
    -- 131 matches inside ONE owner: more than the 127 slots the ephemeral
    -- grid fits in the stride between an owner and its first real child.
    -- Exercises the per-owner slot ordinal; with a batch-global ordinal the
    -- rows past 127 were numbered outside their own container.
    ('cs_many',       convert_to(repeat(E'zqx\n', 131), 'UTF8')),
    -- ONE match, in a DIFFERENT object, in the same search batch.  Its owner
    -- holds a single ephemeral row, so any per-owner numbering places it in
    -- the first slot; a batch-global ordinal gives it the 132nd.
    ('cs_tail',       E'tail zqx here\n'),
    -- A co-extensive cluster, the PMU_FORMAT_ATTR shape.  `wq9` sits in the
    -- macro ARGUMENTS, outside the macro's clamped name range but inside the
    -- extent every generated symbol shares.  Which of them owns an ephemeral
    -- row placed there is what the owner lookup has to decide.
    ('cs_coext',      E'COEXT_ATTR(wq9, cfg)\n'),
    -- Object 14: `zk4` sits OUTSIDE every declaration, in the position a Go
    -- import block or package clause occupies; `zk5` sits inside the function.
    -- Both are inside the co-extensive module/directory/file chain.
    ('cs_chain',      E'import zk4\nfunc h(){ zk5 }\n'),
    -- NESTED macros.  `k7z` sits inside MAC_IN, inside MAC_OUT, inside
    -- fn_host.  Its owner is MAC_IN (the narrowest container) at fold_depth 1,
    -- while the innermost NON-FOLDED container is fn_host at fold_depth 0 --
    -- the only shape where those two differ, and so the only shape that can
    -- tell the two fold-depth rules apart.
    --   offsets: OUT(IN(k7z)) = [15,27), IN(k7z) = [19,26), k7z = [22,25)
    ('cs_nestmac',    E'int host(void){OUT(IN(k7z));}\n'),
    -- The realistic macro shape, with the generated symbol marked FOLDED as the
    -- indexer marks it.  `zq8` is a macro ARGUMENT -- text the author of h13
    -- typed -- so its owner must be h13, not the expansion and not the symbol
    -- the expansion generated.  A macro expansion owns no text.
    --   offsets: M13(zq8,c) = [15,25), zq8 = [19,22)
    ('cs_argown',     E'void h13(void){M13(zq8,c);}\n'),
    ('cs_binary',     E'\\xfffefa666f6f0bff'::bytea),
    -- Cross-project shared content: hash referenced by an object in each
    -- project.  Exercises the (content_hash, project_id) constraint that
    -- the search SQL applies to keep cross-project leakage out of project-
    -- scoped results.
    ('cs_shared',     E'shared_token across projects\n');

-- Project 1 objects (layer DEFAULT = 1000001 set above).
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash) VALUES
    (1, 1, 'basic.c',    '/p1/basic.c',    'cc', 'cs_basic'),
    (2, 1, 'multi.c',    '/p1/multi.c',    'cc', 'cs_multi'),
    (3, 1, 'boundary.c', '/p1/boundary.c', 'cc', 'cs_boundary'),
    -- Same cs_shared content_hash in both projects; the per-project search
    -- must return only the project's object.
    (7, 1, 'shared.h',   '/p1/shared.h',   'cc', 'cs_shared'),
    (9, 1, 'many.c',     '/p1/many.c',     'cc', 'cs_many'),
    (10, 1, 'tail.c',    '/p1/tail.c',     'cc', 'cs_tail'),
    (11, 1, 'coext.c',   '/p1/coext.c',    'cc', 'cs_coext'),
    (12, 1, 'nestmac.c', '/p1/nestmac.c',  'cc', 'cs_nestmac'),
    (13, 1, 'argown.c',  '/p1/argown.c',   'cc', 'cs_argown'),
    (14, 1, 'chain.go',  '/p1/chain.go',   'go', 'cs_chain');

-- Sentinel function symbols for objects 1-3 just so composite-filter tests
-- have somewhere to attach.  Object 5 is intentionally left symbol-less to
-- prove search() reaches it through the content_store ⋈ objects skeleton.
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (10, 'fn_basic',    1, 1, 1),
    (11, 'fn_multi',    1, 1, 1),
    (12, 'fn_boundary', 1, 1, 1),
    (14, 'fn_many',     1, 1, 1),
    (15, 'fn_tail',     1, 1, 1),
    -- Co-extensive cluster: a macro plus the three symbols it generates.
    (16, 'COEXT_ATTR',   1, 7, 2),
    (17, 'coext_show',   1, 1, 2),
    (18, 'coext_assert', 1, 1, 2),
    (19, 'coext_attr',   1, 6, 2),
    -- Nested-macro cluster.
    (20, 'fn_host',      1, 1, 2),
    (21, 'MAC_OUT',      1, 7, 2),
    (22, 'MAC_IN',       1, 7, 2),
    -- Argument-ownership cluster.
    (23, 'h13',          1, 1, 2),
    (24, 'M13',          1, 7, 2),
    (25, 'g13',          1, 1, 2),
    -- Object 14's container chain: a module, a directory and a file, all
    -- covering the whole object, plus one ordinary function inside.
    (26, 'chain_mod',    1, 3, 2),
    (27, '/p1',          1, 4, 2),
    (28, '/p1/chain.go', 1, 2, 2),
    (29, 'h14',          1, 1, 2);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (110, 10, 1, int4range(0, 16),  1),
    (111, 11, 2, int4range(0, 12),  1),
    (112, 12, 3, int4range(0, 27),  1),
    (114, 14, 9, int4range(0, 524), 1),
    (115, 15, 10, int4range(0, 14), 1);

-- Object 11: the co-extensive cluster, numbered BY HAND because this is exactly
-- the shape ranges cannot express.  The macro is clamped to its own name, so its
-- extent is NARROWER than the symbols it owns -- the deliberate inversion the
-- DFS interval exists for.  `number_fixture_tree` leaves already-numbered rows
-- alone, so every instance of this object carries explicit numbers.
--
--   enter  exit depth fold  extent      symbol
--    1024 10240   0     0   [0,21)      COEXT_ATTR   (folded: the boundary)
-- The macro's extent is the WHOLE INVOCATION, as the un-clamped indexer emits
-- it.  That is load-bearing for scoped search: `macro(...) { search(...) }`
-- fuses the macro's byte ranges into the scan's corpus, so a name-only extent
-- would exclude the argument text and the search would correctly find nothing.
--    2048  5120   1     1   [0,21)        coext_show
--    3072  4096   2     1   [0,21)          coext_assert
--    6144  7168   1     1   [0,21)        coext_attr
INSERT INTO symbol_instances
    (id, symbol, object_id, offset_range, instance_type,
     dfs_enter, dfs_exit, nesting_level, folded, fold_depth) VALUES
    (116, 16, 11, int4range(0, 21), 3, 1024, 10240, 0, TRUE,  0),
    (117, 17, 11, int4range(0, 21), 1, 2048,  5120, 1, FALSE, 1),
    (118, 18, 11, int4range(0, 21), 2, 3072,  4096, 2, FALSE, 1),
    (119, 19, 11, int4range(0, 21), 1, 6144,  7168, 1, FALSE, 1);

-- Object 12: nested macros.  Ranges nest properly here, so
-- `number_fixture_tree` derives the whole tree -- including fold_depth, which
-- it accumulates from the `folded` flags below.  Leaving dfs_exit at 0 is what
-- opts these rows into that pass.
INSERT INTO symbol_instances
    (id, symbol, object_id, offset_range, instance_type, folded) VALUES
    (120, 20, 12, int4range(0, 29),  1, FALSE),  -- fn_host, fold_depth 0
    (121, 21, 12, int4range(15, 27), 3, TRUE),   -- MAC_OUT, folded, fold_depth 0
    (122, 22, 12, int4range(19, 26), 3, TRUE);   -- MAC_IN,  folded, fold_depth 1

-- Object 13: a macro invocation inside a function, with the expansion and the
-- symbol it generates both FOLDED -- what the indexer really produces.  The
-- expansion's extent is the whole invocation (un-clamped), which is exactly why
-- ownership must come from `NOT folded` rather than from the narrowest extent.
INSERT INTO symbol_instances
    (id, symbol, object_id, offset_range, instance_type, folded) VALUES
    (123, 23, 13, int4range(0, 27),  1, FALSE),  -- h13, the caller
    (124, 24, 13, int4range(15, 25), 3, TRUE),   -- M13 expansion, whole invocation
    (125, 25, 13, int4range(15, 25), 1, TRUE);   -- g13, generated, same extent

-- Object 14: the CO-EXTENSIVE CONTAINER CHAIN every real file object carries --
-- its module, its directory and the file itself, all covering [0, len).  Stated
-- rather than derived, because `number_fixture_tree` ranks directories outermost
-- while the clang and Go producers rank modules outermost; what matters here is
-- only that the three are nested and share an extent.
--
--    enter  exit  lvl  type  range       symbol
--     1024 10240   0    mod  [0,27)   chain_mod
--     2048  9216   1    dir  [0,27)     /p1
--     3072  8192   2   file  [0,27)       /p1/chain.go
--     4096  5120   3   func  [11,26)        h14
--
-- `zk4` at [7,10) is inside the file but outside h14, so its owner is decided
-- entirely by the chain -- which is the case this object exists for.
INSERT INTO symbol_instances
    (id, symbol, object_id, offset_range, instance_type,
     dfs_enter, dfs_exit, nesting_level, folded, fold_depth) VALUES
    (126, 26, 14, int4range(0, 27),  5, 1024, 10240, 0, FALSE, 0),
    (127, 27, 14, int4range(0, 27),  5, 2048,  9216, 1, FALSE, 0),
    (128, 28, 14, int4range(0, 27),  9, 3072,  8192, 2, FALSE, 0),
    (129, 29, 14, int4range(11, 26), 1, 4096,  5120, 3, FALSE, 0);

-- Project 2 section: switch the layer DEFAULT to project 2's root layer.
ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000002;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000002;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000002;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000002;

-- Project 2 objects (layer DEFAULT = 1000002 set above).
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash) VALUES
    (4, 2, 'case.c',     '/p2/case.c',     'cc', 'cs_mixedcase'),
    (5, 2, 'README.md',  '/p2/README.md',  'md', 'cs_docless'),
    (6, 2, 'data.bin',   '/p2/data.bin',   'bin', 'cs_binary'),
    (8, 2, 'shared.h',   '/p2/shared.h',   'cc', 'cs_shared');

INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (13, 'fn_case',     2, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (113, 13, 4, int4range(0, 12),  1);

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
