use crate::index_store::{IndexStore, ProjectTreeResult};
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
    let result = store.list_project_tree(1, "/", false).await.unwrap();

    match result {
        ProjectTreeResult::Nodes(nodes) => {
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

            let file_nodes: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_type == "file")
                .collect();
            assert_eq!(file_nodes.len(), 0, "Root should have no direct files");
        }
        other => panic!("Expected Nodes, got {:?}", other),
    }
}

#[tokio::test]
async fn tree_browser_nested_directory_returns_children() {
    let store = shared_test_store().await;
    let result = store.list_project_tree(1, "/src", false).await.unwrap();

    match result {
        ProjectTreeResult::Nodes(nodes) => {
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
}

#[tokio::test]
async fn tree_browser_leaf_directory_returns_only_files() {
    let store = shared_test_store().await;
    let result = store.list_project_tree(1, "/src/util", false).await.unwrap();

    match result {
        ProjectTreeResult::Nodes(nodes) => {
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
}

#[tokio::test]
async fn tree_browser_has_children_flag_correct() {
    let store = shared_test_store().await;
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
}

#[tokio::test]
async fn tree_browser_nonexistent_path_returns_not_directory() {
    let store = shared_test_store().await;
    let result = store.list_project_tree(1, "/nonexistent", false).await.unwrap();

    match result {
        ProjectTreeResult::NotDirectory => {}
        other => panic!("Expected NotDirectory, got {:?}", other),
    }
}
