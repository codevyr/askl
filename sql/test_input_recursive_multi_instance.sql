-- A self-recursive symbol defined in two objects.
--
-- Exists for one edge case in `find_edges_between`: the reference's enclosing
-- declaration IS the lowest-id instance of the symbol it refers to, so the
-- `from_inst.id != to_inst.id` filter eliminates exactly the candidate the
-- `DISTINCT ON (from_inst.id, sr.id) ... ORDER BY ..., to_inst.id` would
-- otherwise pick, and the edge has to resolve to the next instance instead.
-- Any target-side pruning that keeps only ONE row per symbol drops this edge.

SET search_path TO index, public;

INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-rec'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'a.c', '/a.c', 'cc', 'hash_a'),
       (2, 1, 'b.c', '/b.c', 'cc', 'hash_b'),
       (3, 1, 'c.c', '/c.c', 'cc', 'hash_c');

-- `rec` is defined three times, so the target side has more instances than
-- the pruning keeps -- which is what makes the ORDER of that pruning
-- observable.  `caller` is deliberately NOT an instance of `rec`, so its edge
-- exercises the ordering with nothing removed by the self-exclusion.
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (1, 'rec', 1, 1, 1),
       (2, 'caller', 1, 1, 1);

-- Instance 10 is in object 2 and is the LOWEST id of symbol 1 — it is both
-- the caller below and the row `ORDER BY to_inst.id` would otherwise choose.
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (10, 1, 2, int4range(100, 200), 1),
       (20, 1, 1, int4range(100, 200), 1),
       (30, 1, 3, int4range(100, 200), 1),
       (40, 2, 1, int4range(300, 400), 1);

-- `rec` calls itself, from inside instance 10 (the lowest-id `rec`).
-- `caller` calls `rec` too, from an instance that is not a `rec` at all.
INSERT INTO symbol_refs(to_symbol, from_object, from_offset_range)
VALUES (1, 2, int4range(110, 111)),
       (1, 1, int4range(310, 311));

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
