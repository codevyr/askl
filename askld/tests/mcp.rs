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

/// Helper to create MCP test request
fn mcp_request(method: &str, id: Option<i32>, params: Option<Value>) -> Value {
    let mut req = json!({
        "jsonrpc": "2.0",
        "method": method
    });
    if let Some(id) = id {
        req["id"] = json!(id);
    }
    if let Some(params) = params {
        req["params"] = params;
    }
    req
}

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

#[tokio::test]
async fn mcp_initialize_and_ping() {
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

    // Test initialize
    let init_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("initialize", Some(1), Some(json!({
            "protocolVersion": "2024-11-05"
        }))))
        .to_request();
    let init_resp = test::call_service(&app, init_req).await;
    assert!(init_resp.status().is_success());

    let init_body: Value = test::read_body_json(init_resp).await;
    assert_eq!(init_body["result"]["protocolVersion"], "2024-11-05");
    assert!(init_body["result"]["serverInfo"]["name"].as_str().is_some());
    assert!(init_body["result"]["capabilities"]["tools"].is_object());

    // Test ping
    let ping_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("ping", Some(2), None))
        .to_request();
    let ping_resp = test::call_service(&app, ping_req).await;
    assert!(ping_resp.status().is_success());

    let ping_body: Value = test::read_body_json(ping_resp).await;
    assert_eq!(ping_body["result"], json!({}));
}

#[tokio::test]
async fn mcp_error_cases() {
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

    // Test unknown tool
    let unknown_tool_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("tools/call", Some(1), Some(json!({
            "name": "nonexistent_tool",
            "arguments": {}
        }))))
        .to_request();
    let unknown_tool_resp = test::call_service(&app, unknown_tool_req).await;
    assert!(unknown_tool_resp.status().is_success()); // JSON-RPC errors return 200

    let unknown_tool_body: Value = test::read_body_json(unknown_tool_resp).await;
    assert!(unknown_tool_body["error"]["message"].as_str().unwrap().contains("unknown tool"));

    // Test unknown method
    let unknown_method_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("nonexistent/method", Some(2), None))
        .to_request();
    let unknown_method_resp = test::call_service(&app, unknown_method_req).await;
    assert!(unknown_method_resp.status().is_success());

    let unknown_method_body: Value = test::read_body_json(unknown_method_resp).await;
    assert_eq!(unknown_method_body["error"]["code"], -32601); // Method not found

    // Test missing jsonrpc version
    let missing_version_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!({
            "id": 3,
            "method": "ping"
        }))
        .to_request();
    let missing_version_resp = test::call_service(&app, missing_version_req).await;
    // This returns 400 for invalid request
    let missing_version_body: Value = test::read_body_json(missing_version_resp).await;
    assert!(missing_version_body["error"]["message"].as_str().unwrap().contains("missing jsonrpc"));
}

#[tokio::test]
async fn mcp_batch_requests() {
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

    // Test batch request
    let batch_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(json!([
            mcp_request("ping", Some(1), None),
            mcp_request("tools/list", Some(2), None),
            mcp_request("ping", Some(3), None)
        ]))
        .to_request();
    let batch_resp = test::call_service(&app, batch_req).await;
    assert!(batch_resp.status().is_success());

    let batch_body: Value = test::read_body_json(batch_resp).await;
    let responses = batch_body.as_array().expect("batch response should be array");
    assert_eq!(responses.len(), 3);

    // Check IDs match
    let ids: Vec<i64> = responses.iter().filter_map(|r| r["id"].as_i64()).collect();
    assert!(ids.contains(&1));
    assert!(ids.contains(&2));
    assert!(ids.contains(&3));
}

#[tokio::test]
async fn mcp_notification_no_response() {
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

    // Test notification (no id = no response expected)
    let notification_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("notifications/initialized", None, None))
        .to_request();
    let notification_resp = test::call_service(&app, notification_req).await;

    // Notifications should return 202 Accepted with no body
    assert_eq!(notification_resp.status().as_u16(), 202);
}

#[tokio::test]
async fn mcp_resources_and_prompts() {
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

    // Test initialize advertises resources and prompts
    let init_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("initialize", Some(1), Some(json!({
            "protocolVersion": "2024-11-05"
        }))))
        .to_request();
    let init_resp = test::call_service(&app, init_req).await;
    assert!(init_resp.status().is_success());

    let init_body: Value = test::read_body_json(init_resp).await;
    assert!(init_body["result"]["capabilities"]["resources"].is_object());
    assert!(init_body["result"]["capabilities"]["prompts"].is_object());

    // Test resources/list
    let resources_list_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("resources/list", Some(2), None))
        .to_request();
    let resources_list_resp = test::call_service(&app, resources_list_req).await;
    assert!(resources_list_resp.status().is_success());

    let resources_list_body: Value = test::read_body_json(resources_list_resp).await;
    let resources = resources_list_body["result"]["resources"]
        .as_array()
        .expect("resources list array");
    assert!(!resources.is_empty());
    let resource_uris: Vec<&str> = resources
        .iter()
        .filter_map(|r| r["uri"].as_str())
        .collect();
    assert!(resource_uris.contains(&"askl://workflow"));
    assert!(resource_uris.contains(&"askl://syntax"));
    assert!(resource_uris.contains(&"askl://limitations"));

    // Test resources/read
    let resources_read_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("resources/read", Some(3), Some(json!({
            "uri": "askl://syntax"
        }))))
        .to_request();
    let resources_read_resp = test::call_service(&app, resources_read_req).await;
    assert!(resources_read_resp.status().is_success());

    let resources_read_body: Value = test::read_body_json(resources_read_resp).await;
    let contents = resources_read_body["result"]["contents"]
        .as_array()
        .expect("contents array");
    assert!(!contents.is_empty());
    assert!(contents[0]["text"].as_str().unwrap().contains("Askl Query Syntax"));

    // Test prompts/list
    let prompts_list_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("prompts/list", Some(4), None))
        .to_request();
    let prompts_list_resp = test::call_service(&app, prompts_list_req).await;
    assert!(prompts_list_resp.status().is_success());

    let prompts_list_body: Value = test::read_body_json(prompts_list_resp).await;
    let prompts = prompts_list_body["result"]["prompts"]
        .as_array()
        .expect("prompts list array");
    assert!(!prompts.is_empty());
    let prompt_names: Vec<&str> = prompts
        .iter()
        .filter_map(|p| p["name"].as_str())
        .collect();
    assert!(prompt_names.contains(&"find_callers"));
    assert!(prompt_names.contains(&"find_callees"));
    assert!(prompt_names.contains(&"trace_path"));
    assert!(prompt_names.contains(&"kubernetes_preamble"));

    // Test prompts/get for kubernetes_preamble
    let prompts_get_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("prompts/get", Some(5), Some(json!({
            "name": "kubernetes_preamble"
        }))))
        .to_request();
    let prompts_get_resp = test::call_service(&app, prompts_get_req).await;
    assert!(prompts_get_resp.status().is_success());

    let prompts_get_body: Value = test::read_body_json(prompts_get_resp).await;
    let messages = prompts_get_body["result"]["messages"]
        .as_array()
        .expect("messages array");
    assert!(!messages.is_empty());
    let text = messages[0]["content"]["text"].as_str().unwrap();
    assert!(text.contains("@ignore(package=\"builtin\")"));
    assert!(text.contains("@project(\"kubernetes\")"));

    // Test prompts/get for find_callers with arguments
    let prompts_get_callers_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("prompts/get", Some(6), Some(json!({
            "name": "find_callers",
            "arguments": {
                "function": "CreatePod",
                "project": "kubernetes",
                "depth": "2"
            }
        }))))
        .to_request();
    let prompts_get_callers_resp = test::call_service(&app, prompts_get_callers_req).await;
    assert!(prompts_get_callers_resp.status().is_success());

    let prompts_get_callers_body: Value = test::read_body_json(prompts_get_callers_resp).await;
    let callers_messages = prompts_get_callers_body["result"]["messages"]
        .as_array()
        .expect("messages array");
    let callers_text = callers_messages[0]["content"]["text"].as_str().unwrap();
    assert!(callers_text.contains("CreatePod"));
    assert!(callers_text.contains("kubernetes"));

    // Test unknown resource
    let unknown_resource_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("resources/read", Some(7), Some(json!({
            "uri": "askl://nonexistent"
        }))))
        .to_request();
    let unknown_resource_resp = test::call_service(&app, unknown_resource_req).await;
    assert!(unknown_resource_resp.status().is_success()); // JSON-RPC errors return 200

    let unknown_resource_body: Value = test::read_body_json(unknown_resource_resp).await;
    assert!(unknown_resource_body["error"]["message"].as_str().unwrap().contains("unknown resource"));

    // Test unknown prompt
    let unknown_prompt_req = test::TestRequest::post()
        .uri("/mcp")
        .set_json(mcp_request("prompts/get", Some(8), Some(json!({
            "name": "nonexistent_prompt"
        }))))
        .to_request();
    let unknown_prompt_resp = test::call_service(&app, unknown_prompt_req).await;
    assert!(unknown_prompt_resp.status().is_success());

    let unknown_prompt_body: Value = test::read_body_json(unknown_prompt_resp).await;
    assert!(unknown_prompt_body["error"]["message"].as_str().unwrap().contains("unknown prompt"));
}
