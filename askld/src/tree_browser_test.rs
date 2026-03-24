use crate::index_store::{IndexStore, ProjectTreeResult};
use crate::test_util::{get_shared_db_url, TEST_INPUT_TREE_BROWSER};
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use tokio::{runtime::Runtime, task};

fn shared_test_store() -> IndexStore {
    let url = get_shared_db_url(TEST_INPUT_TREE_BROWSER);
    let manager = ConnectionManager::<PgConnection>::new(url);
    let pool = Pool::builder()
        .test_on_check_out(true)
        .build(manager)
        .unwrap();
    IndexStore::from_pool(pool)
}

#[test]
fn tree_browser_root_returns_direct_children() {
    let rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&rt, async {
        let store = shared_test_store();
        let result = store.list_project_tree(1, "/", false).await.unwrap();

        match result {
            ProjectTreeResult::Nodes(nodes) => {
                // Root should have 2 direct children: /src and /docs
                let dir_nodes: Vec<_> = nodes
                    .iter()
                    .filter(|n| n.node_type == "dir")
                    .collect();
                assert_eq!(
                    dir_nodes.len(),
                    2,
                    "Root should have 2 direct child directories, got: {:?}",
                    dir_nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
                );

                let paths: Vec<_> = dir_nodes.iter().map(|n| n.path.as_str()).collect();
                assert!(paths.contains(&"/src"), "Should contain /src");
                assert!(paths.contains(&"/docs"), "Should contain /docs");

                // No files directly in root
                let file_nodes: Vec<_> = nodes
                    .iter()
                    .filter(|n| n.node_type == "file")
                    .collect();
                assert_eq!(file_nodes.len(), 0, "Root should have no direct files");
            }
            other => panic!("Expected Nodes, got {:?}", other),
        }
    });
}

#[test]
fn tree_browser_nested_directory_returns_children() {
    let rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&rt, async {
        let store = shared_test_store();
        let result = store.list_project_tree(1, "/src", false).await.unwrap();

        match result {
            ProjectTreeResult::Nodes(nodes) => {
                // /src should have:
                // - 2 directories: /src/util, /src/config
                // - 1 file: /src/main.go
                let dir_nodes: Vec<_> = nodes
                    .iter()
                    .filter(|n| n.node_type == "dir")
                    .collect();
                assert_eq!(
                    dir_nodes.len(),
                    2,
                    "/src should have 2 direct child directories, got: {:?}",
                    dir_nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
                );

                let dir_paths: Vec<_> = dir_nodes.iter().map(|n| n.path.as_str()).collect();
                assert!(dir_paths.contains(&"/src/util"), "Should contain /src/util");
                assert!(dir_paths.contains(&"/src/config"), "Should contain /src/config");

                let file_nodes: Vec<_> = nodes
                    .iter()
                    .filter(|n| n.node_type == "file")
                    .collect();
                assert_eq!(
                    file_nodes.len(),
                    1,
                    "/src should have 1 direct file, got: {:?}",
                    file_nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
                );
                assert_eq!(file_nodes[0].path, "/src/main.go");
            }
            other => panic!("Expected Nodes, got {:?}", other),
        }
    });
}

#[test]
fn tree_browser_leaf_directory_returns_only_files() {
    let rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&rt, async {
        let store = shared_test_store();
        let result = store.list_project_tree(1, "/src/util", false).await.unwrap();

        match result {
            ProjectTreeResult::Nodes(nodes) => {
                // /src/util should have:
                // - 0 directories
                // - 2 files: util.go, helper.go
                let dir_nodes: Vec<_> = nodes
                    .iter()
                    .filter(|n| n.node_type == "dir")
                    .collect();
                assert_eq!(dir_nodes.len(), 0, "/src/util should have no child directories");

                let file_nodes: Vec<_> = nodes
                    .iter()
                    .filter(|n| n.node_type == "file")
                    .collect();
                assert_eq!(
                    file_nodes.len(),
                    2,
                    "/src/util should have 2 files, got: {:?}",
                    file_nodes.iter().map(|n| &n.path).collect::<Vec<_>>()
                );

                let file_paths: Vec<_> = file_nodes.iter().map(|n| n.path.as_str()).collect();
                assert!(file_paths.contains(&"/src/util/util.go"), "Should contain util.go");
                assert!(file_paths.contains(&"/src/util/helper.go"), "Should contain helper.go");
            }
            other => panic!("Expected Nodes, got {:?}", other),
        }
    });
}

#[test]
fn tree_browser_has_children_flag_correct() {
    let rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&rt, async {
        let store = shared_test_store();
        let result = store.list_project_tree(1, "/", false).await.unwrap();

        match result {
            ProjectTreeResult::Nodes(nodes) => {
                for node in &nodes {
                    if node.path == "/src" {
                        assert!(
                            node.has_children,
                            "/src should have has_children=true"
                        );
                    } else if node.path == "/docs" {
                        assert!(
                            node.has_children,
                            "/docs should have has_children=true"
                        );
                    }
                }
            }
            other => panic!("Expected Nodes, got {:?}", other),
        }
    });
}

#[test]
fn tree_browser_nonexistent_path_returns_not_directory() {
    let rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&rt, async {
        let store = shared_test_store();
        let result = store.list_project_tree(1, "/nonexistent", false).await.unwrap();

        match result {
            ProjectTreeResult::NotDirectory => {}
            other => panic!("Expected NotDirectory, got {:?}", other),
        }
    });
}
