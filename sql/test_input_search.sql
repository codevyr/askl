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

INSERT INTO projects (id, project_name, root_path) VALUES
    (1, 'search_proj_1', '/p1'),
    (2, 'search_proj_2', '/p2');

INSERT INTO content_store (content_hash, content) VALUES
    ('cs_basic',      E'hello foo world\n'),
    ('cs_multi',      E'foo foo foo\n'),
    ('cs_boundary',   E'foobar foo foo_bar foo.bar\n'),
    ('cs_mixedcase',  E'Foo FOO foo\n'),
    ('cs_docless',    E'doc-only content with foo here\n'),
    ('cs_binary',     E'\\xfffefa666f6f0bff'::bytea),
    -- Cross-project shared content: hash referenced by an object in each
    -- project.  Exercises the (content_hash, project_id) constraint that
    -- the search SQL applies to keep cross-project leakage out of project-
    -- scoped results.
    ('cs_shared',     E'shared_token across projects\n');

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash) VALUES
    (1, 1, 'basic.c',    '/p1/basic.c',    'cc', 'cs_basic'),
    (2, 1, 'multi.c',    '/p1/multi.c',    'cc', 'cs_multi'),
    (3, 1, 'boundary.c', '/p1/boundary.c', 'cc', 'cs_boundary'),
    (4, 2, 'case.c',     '/p2/case.c',     'cc', 'cs_mixedcase'),
    (5, 2, 'README.md',  '/p2/README.md',  'md', 'cs_docless'),
    (6, 2, 'data.bin',   '/p2/data.bin',   'bin', 'cs_binary'),
    -- Same cs_shared content_hash in both projects; the per-project search
    -- must return only the project's object.
    (7, 1, 'shared.h',   '/p1/shared.h',   'cc', 'cs_shared'),
    (8, 2, 'shared.h',   '/p2/shared.h',   'cc', 'cs_shared');

-- Sentinel function symbols for objects 1-4 just so composite-filter tests
-- have somewhere to attach.  Object 5 is intentionally left symbol-less to
-- prove search() reaches it through the content_store ⋈ objects skeleton.
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope) VALUES
    (10, 'fn_basic',    1, 1, 1),
    (11, 'fn_multi',    1, 1, 1),
    (12, 'fn_boundary', 1, 1, 1),
    (13, 'fn_case',     2, 1, 1);

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type) VALUES
    (110, 10, 1, int4range(0, 16),  1),
    (111, 11, 2, int4range(0, 12),  1),
    (112, 12, 3, int4range(0, 27),  1),
    (113, 13, 4, int4range(0, 12),  1);
