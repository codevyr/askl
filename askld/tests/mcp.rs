#[path = "../src/bin/askld/api/mod.rs"]
mod api;

use actix_web::{test, web, App};
use askld::auth::AuthStore;
use askld::cfg::ControlFlowGraph;
use askld::index_store::IndexStore;
use askld::test_support::wait_for_postgres;
use diesel::prelude::*;
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use index::db_diesel::Index;
use serde_json::{json, Value};
use testcontainers::{clients, core::WaitFor, GenericImage};

use api::types::AsklData;

#[tokio::test]
async fn mcp_tools_list_and_call_happy_path() {
    let docker = clients::Cli::default();
    let image = GenericImage::new("postgres", "15-alpine")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "askl")
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ));
    let node = docker.run(image);
    let port = node.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/askl", port);

    wait_for_postgres(&url).await.expect("wait for postgres");

    let manager = ConnectionManager::<PgConnection>::new(&url);
    let pool = Pool::builder()
        .build(manager)
        .expect("build database pool");

    let auth_store = AuthStore::from_pool(pool.clone()).expect("init auth store");

    let index_diesel = Index::from_pool(pool.clone()).expect("init index");
    index_diesel
        .load_test_input(Index::TEST_INPUT_MODULES)
        .await
        .expect("load test input");

    let index_store = IndexStore::from_pool(pool.clone());
    let askl_data = web::Data::new(AsklData {
        cfg: ControlFlowGraph::from_symbols(index_diesel),
    });

    let app = test::init_service(
        App::new()
            .app_data(askl_data.clone())
            .app_data(web::Data::new(auth_store))
            .app_data(web::Data::new(index_store))
            .configure(api::configure),
    )
    .await;

    let tools_list_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({
            "jsonrpc": "2.0",
            "id": 1,
            "method": "tools/list"
        }))
        .to_request();
    let tools_list_resp = test::call_service(&app, tools_list_req).await;
    assert!(tools_list_resp.status().is_success());

    let tools_list_body: Value = test::read_body_json(tools_list_resp).await;
    let tools = tools_list_body["result"]["tools"]
        .as_array()
        .expect("tools list array");
    let tool_names: Vec<&str> = tools
        .iter()
        .filter_map(|tool| tool["name"].as_str())
        .collect();
    assert!(tool_names.contains(&"askl_query_run"));
    assert!(tool_names.contains(&"askl_projects_list"));

    let query_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({
            "jsonrpc": "2.0",
            "id": 2,
            "method": "tools/call",
            "params": {
                "name": "askl_query_run",
                "arguments": {
                    "query": "@project(\"test_project\") \"a\""
                }
            }
        }))
        .to_request();
    let query_resp = test::call_service(&app, query_req).await;
    assert!(query_resp.status().is_success());

    let query_body: Value = test::read_body_json(query_resp).await;
    let content = &query_body["result"]["content"];
    let payload_text = content[0]["text"].as_str().unwrap();
    let payload: Value = serde_json::from_str(payload_text).unwrap();
    let nodes = payload["graph"]["nodes"].as_array().unwrap();
    assert!(!nodes.is_empty());
    assert!(payload.get("limitations").is_some());
    assert!(payload.get("telemetry").is_some());
}

#[tokio::test]
async fn mcp_source_get_respects_ranges() {
    let docker = clients::Cli::default();
    let image = GenericImage::new("postgres", "15-alpine")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "askl")
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ));
    let node = docker.run(image);
    let port = node.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/askl", port);

    wait_for_postgres(&url).await.expect("wait for postgres");

    let manager = ConnectionManager::<PgConnection>::new(&url);
    let pool = Pool::builder()
        .build(manager)
        .expect("build database pool");

    let auth_store = AuthStore::from_pool(pool.clone()).expect("init auth store");

    let index_diesel = Index::from_pool(pool.clone()).expect("init index");
    index_diesel
        .load_test_input(Index::TEST_INPUT_MODULES)
        .await
        .expect("load test input");

    {
        let mut conn = pool.get().expect("get connection");
        diesel::sql_query("INSERT INTO index.file_contents (file_id, content) VALUES ($1, $2)")
            .bind::<diesel::sql_types::Integer, _>(1)
            .bind::<diesel::sql_types::Binary, _>(b"hello world".to_vec())
            .execute(&mut conn)
            .expect("insert file contents");
    }

    let index_store = IndexStore::from_pool(pool.clone());
    let askl_data = web::Data::new(AsklData {
        cfg: ControlFlowGraph::from_symbols(index_diesel),
    });

    let app = test::init_service(
        App::new()
            .app_data(askl_data.clone())
            .app_data(web::Data::new(auth_store))
            .app_data(web::Data::new(index_store))
            .configure(api::configure),
    )
    .await;

    let source_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({
            "jsonrpc": "2.0",
            "id": 3,
            "method": "tools/call",
            "params": {
                "name": "askl_source_get",
                "arguments": {
                    "file_id": 1,
                    "start_offset": 0,
                    "end_offset": 5
                }
            }
        }))
        .to_request();
    let source_resp = test::call_service(&app, source_req).await;
    assert!(source_resp.status().is_success());

    let source_body: Value = test::read_body_json(source_resp).await;
    let payload_text = source_body["result"]["content"][0]["text"]
        .as_str()
        .unwrap();
    let payload: Value = serde_json::from_str(payload_text).unwrap();
    assert_eq!(payload["content_text"].as_str().unwrap(), "hello");
    assert_eq!(payload["range"]["start_offset"].as_i64().unwrap(), 0);
    assert_eq!(payload["range"]["end_offset"].as_i64().unwrap(), 5);
}
