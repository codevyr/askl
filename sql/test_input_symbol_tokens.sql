SET search_path TO index, public;

INSERT INTO projects (id, project_name)
VALUES (1, 'test_project');

INSERT INTO modules (id, module_name, project_id)
VALUES (1, 'test_module', 1);

INSERT INTO files (id, module, module_path, filesystem_path, filetype, content_hash)
VALUES (1, 1, 'kube.go', 'kube.go', 'go', 'hash');

INSERT INTO symbols (id, name, symbol_path, module, symbol_scope)
VALUES
    (
        1,
        'kubelet.aaaaaaaaaaa.run',
        'kubelet.aaaaaaaaaaa.run',
        1,
        2
    ),
    (2, 'kubernetes.run', 'kubernetes.run', 1, 2),
    (3, 'kubeleter.run', 'kubeleter.run', 1, 2),
    (4, 'Kubelet.run', 'Kubelet.run', 1, 2),
    (
        5,
        '(*k8s.io/kubernetes/pkg/kubelet.Kubelet).Run',
        'k8s.io.kubernetes.pkg.kubelet.Kubelet.Run',
        1,
        2
    );

INSERT INTO declarations (id, symbol, file_id, symbol_type, start_offset, end_offset)
VALUES
    (1, 1, 1, 1, 0, 5),
    (2, 2, 1, 1, 10, 15),
    (3, 3, 1, 1, 20, 25),
    (4, 4, 1, 1, 30, 35),
    (5, 5, 1, 1, 40, 45);
