SET search_path TO index, public;

-- Root layer for the fixture project.  Persistent inserts below inherit it
-- via the layer column DEFAULT.
INSERT INTO layers (id, parent_id, hash, kind, populated)
OVERRIDING SYSTEM VALUE
VALUES (1000001, NULL, decode(md5('fixture-root-1'), 'hex'), 'root', TRUE);

INSERT INTO projects (id, project_name, root_path, root_layer_id)
VALUES (1, 'test_project', '/test_project', 1000001);

ALTER TABLE symbols          ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_instances ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE symbol_refs      ALTER COLUMN layer SET DEFAULT 1000001;
ALTER TABLE objects          ALTER COLUMN layer SET DEFAULT 1000001;

-- directories table has been removed - directories are now symbols

INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'kube.go', '/kube.go', 'go', 'hash');

-- Sentinel object for "/" directory
INSERT INTO objects (id, project_id, module_path, filesystem_path, filetype, content_hash)
VALUES (2, 1, '/', '/', 'directory', '');

-- File symbol (type=2)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (100, '/kube.go', 1, 2, NULL);

-- Directory symbol (type=4)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES (101, '/', 1, 4, NULL);

-- File instance
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (1001, 100, 1, int4range(0, 100), 6);

-- Directory self-instance on sentinel object
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (1002, 101, 2, int4range(0, 0), 4);

-- Directory instance on /kube.go for containment
INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES (1003, 101, 1, int4range(0, 100), 5);

-- Function symbols (type=1)
INSERT INTO symbols (id, name, project_id, symbol_type, symbol_scope)
VALUES
    (
        1,
        'kubelet.aaaaaaaaaaa.run',
        1,
        1,
        2
    ),
    (2, 'kubernetes.run', 1, 1, 2),
    (3, 'kubeleter.run', 1, 1, 2),
    (4, 'Kubelet.run', 1, 1, 2),
    (
        5,
        '(*k8s.io/kubernetes/pkg/kubelet.Kubelet).Run',
        1,
        1,
        2
    );

INSERT INTO symbol_instances (id, symbol, object_id, offset_range, instance_type)
VALUES
    (1, 1, 1, int4range(0, 5), 1),
    (2, 2, 1, int4range(10, 15), 1),
    (3, 3, 1, int4range(20, 25), 1),
    (4, 4, 1, int4range(30, 35), 1),
    (5, 5, 1, int4range(40, 45), 1);

ALTER TABLE symbols          ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_instances ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE symbol_refs      ALTER COLUMN layer DROP DEFAULT;
ALTER TABLE objects          ALTER COLUMN layer DROP DEFAULT;
