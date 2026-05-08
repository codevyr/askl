use crate::index_store::{IndexStore, MultiTreeResult, NodeType};
use crate::test_util::{get_shared_db_url, TEST_INPUT_TREE_BROWSER};
use diesel_async::pooled_connection::bb8::Pool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::AsyncPgConnection;

async fn shared_test_store() -> IndexStore {
    let url = get_shared_db_url(TEST_INPUT_TREE_BROWSER);
    let config = AsyncDieselConnectionManager::<AsyncPgConnection>::new(url);
    let pool = Pool::builder()
        .max_size(1)
        .build(config)
        .await
        .unwrap();
    IndexStore::from_pool(pool)
}

#[tokio::test]
async fn tree_browser_root_returns_direct_children() {
    let store = shared_test_store().await;
    let result = store.list_project_tree_multi(1, &["/".to_string()], false).await.unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let nodes = map.remove("/").unwrap_or_default();
            let dir_nodes: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_type == NodeType::Dir)
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

            let file_nodes: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_type == NodeType::File)
                .collect();
            assert_eq!(file_nodes.len(), 0, "Root should have no direct files");
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_nested_directory_returns_children() {
    let store = shared_test_store().await;
    let result = store.list_project_tree_multi(1, &["/src".to_string()], false).await.unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let nodes = map.remove("/src").unwrap_or_default();
            let dir_nodes: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_type == NodeType::Dir)
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
                .filter(|n| n.node_type == NodeType::File)
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
}

#[tokio::test]
async fn tree_browser_leaf_directory_returns_only_files() {
    let store = shared_test_store().await;
    let result = store.list_project_tree_multi(1, &["/src/util".to_string()], false).await.unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let nodes = map.remove("/src/util").unwrap_or_default();
            let dir_nodes: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_type == NodeType::Dir)
                .collect();
            assert_eq!(dir_nodes.len(), 0, "/src/util should have no child directories");

            let file_nodes: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_type == NodeType::File)
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
}

#[tokio::test]
async fn tree_browser_has_children_flag_correct() {
    let store = shared_test_store().await;
    let result = store.list_project_tree_multi(1, &["/".to_string()], false).await.unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let nodes = map.remove("/").unwrap_or_default();
            for node in &nodes {
                if node.path == "/src" {
                    assert!(node.has_children, "/src should have has_children=true");
                } else if node.path == "/docs" {
                    assert!(node.has_children, "/docs should have has_children=true");
                }
            }
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_nonexistent_path_returns_not_directory() {
    let store = shared_test_store().await;
    let result = store.list_project_tree_multi(1, &["/nonexistent".to_string()], false).await.unwrap();

    match result {
        MultiTreeResult::NotDirectory(_) => {}
        other => panic!("Expected NotDirectory, got {:?}", other),
    }
}

// --- Multi-path batching tests ---

#[tokio::test]
async fn tree_browser_multi_path_returns_all_paths() {
    let store = shared_test_store().await;
    let result = store
        .list_project_tree_multi(
            1,
            &["/".to_string(), "/src".to_string(), "/docs".to_string()],
            false,
        )
        .await
        .unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            assert_eq!(map.len(), 3, "Expected 3 entries in the map");

            let root_nodes = map.remove("/").unwrap_or_default();
            let root_dirs: Vec<_> = root_nodes.iter().filter(|n| n.node_type == NodeType::Dir).collect();
            assert_eq!(root_dirs.len(), 2, "Root should have 2 dirs");
            let root_paths: Vec<_> = root_dirs.iter().map(|n| n.path.as_str()).collect();
            assert!(root_paths.contains(&"/src") && root_paths.contains(&"/docs"));

            let src_nodes = map.remove("/src").unwrap_or_default();
            let src_dirs: Vec<_> = src_nodes.iter().filter(|n| n.node_type == NodeType::Dir).collect();
            let src_files: Vec<_> = src_nodes.iter().filter(|n| n.node_type == NodeType::File).collect();
            assert_eq!(src_dirs.len(), 2, "/src should have 2 child dirs");
            assert_eq!(src_files.len(), 1, "/src should have 1 file");

            let docs_nodes = map.remove("/docs").unwrap_or_default();
            let docs_files: Vec<_> = docs_nodes.iter().filter(|n| n.node_type == NodeType::File).collect();
            assert_eq!(docs_files.len(), 1, "/docs should have 1 file");
            assert_eq!(docs_files[0].path, "/docs/readme.md");
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_multi_path_nodes_do_not_cross_contaminate() {
    // Verify that children of /src don't appear under /docs and vice-versa.
    let store = shared_test_store().await;
    let result = store
        .list_project_tree_multi(
            1,
            &["/src".to_string(), "/docs".to_string()],
            false,
        )
        .await
        .unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let src_nodes = map.remove("/src").unwrap_or_default();
            let docs_nodes = map.remove("/docs").unwrap_or_default();

            let src_paths: Vec<_> = src_nodes.iter().map(|n| n.path.as_str()).collect();
            let docs_paths: Vec<_> = docs_nodes.iter().map(|n| n.path.as_str()).collect();

            // /src children must not appear under /docs
            for p in &src_paths {
                assert!(!docs_paths.contains(p), "{} appeared in /docs results", p);
            }
            // /docs children must not appear under /src
            for p in &docs_paths {
                assert!(!src_paths.contains(p), "{} appeared in /src results", p);
            }
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_has_children_true_for_dirs_with_files() {
    // has_children means "has any children" (dirs OR files).
    // /src/util and /src/config contain only files, but has_children must still be true.
    let store = shared_test_store().await;
    let result = store
        .list_project_tree_multi(1, &["/src".to_string()], false)
        .await
        .unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let nodes = map.remove("/src").unwrap_or_default();
            for node in &nodes {
                if node.path == "/src/util" || node.path == "/src/config" {
                    assert!(
                        node.has_children,
                        "{} has file children — has_children should be true",
                        node.path
                    );
                }
            }
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_invalid_expand_path_is_detected() {
    let store = shared_test_store().await;
    // /src is valid, /nonexistent is not.
    let result = store
        .list_project_tree_multi(
            1,
            &["/src".to_string(), "/nonexistent".to_string()],
            false,
        )
        .await
        .unwrap();

    match result {
        MultiTreeResult::NotDirectory(p) => {
            assert_eq!(p, "/nonexistent");
        }
        other => panic!("Expected NotDirectory, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_compact_mode_no_compact_path_when_not_compactable() {
    // Compaction only applies to directories whose single child is a directory.
    // /src/util and /src/config each have only file children, so they are NOT
    // compactable and compact_path must be None.
    let store = shared_test_store().await;
    let result = store
        .list_project_tree_multi(1, &["/src".to_string()], true)
        .await
        .unwrap();

    match result {
        MultiTreeResult::Nodes(mut map) => {
            let nodes = map.remove("/src").unwrap_or_default();
            let dirs: Vec<_> = nodes.iter().filter(|n| n.node_type == NodeType::Dir).collect();
            assert_eq!(dirs.len(), 2, "/src should have 2 child dirs");

            for dir in &dirs {
                assert!(
                    dir.compact_path.is_none(),
                    "{} has only file children — compact_path should be None",
                    dir.path
                );
            }
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}
