use actix_web::{get, post, web, HttpResponse, Responder};
use askld::index_store::{IndexStore, ProjectTreeResult, StoreError};
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine as _;
use index::symbols::FileId;
use log::error;
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Number, Value};
use std::time::Instant;

use super::query::{execute_query, QueryFailure};
use super::types::{AsklData, ErrorResponse, Graph};

const MCP_JSONRPC_VERSION: &str = "2.0";
const MCP_DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

const TOOL_ASKL_QUERY_RUN: &str = "askl_query_run";
const TOOL_ASKL_PROJECTS_LIST: &str = "askl_projects_list";
const TOOL_ASKL_PROJECT_DETAILS: &str = "askl_project_details";
const TOOL_ASKL_TREE_LIST: &str = "askl_tree_list";
const TOOL_ASKL_SOURCE_GET: &str = "askl_source_get";

const PROJECT_SCOPE: [&str; 2] = ["kubernetes", "kueue"];

#[post("/mcp")]
pub async fn mcp_post(
    askl_data: web::Data<AsklData>,
    index_store: web::Data<IndexStore>,
    body: web::Bytes,
) -> impl Responder {
    let value: Value = match serde_json::from_slice(&body) {
        Ok(value) => value,
        Err(err) => {
            return HttpResponse::BadRequest().json(jsonrpc_error_value(
                Value::Null,
                RpcError::parse_error(&err.to_string()),
            ));
        }
    };

    let is_batch = matches!(value, Value::Array(_));
    let messages = match value {
        Value::Array(messages) => {
            if messages.is_empty() {
                return HttpResponse::BadRequest().json(jsonrpc_error_value(
                    Value::Null,
                    RpcError::invalid_request("Invalid Request: empty batch"),
                ));
            }
            messages
        }
        other => vec![other],
    };

    let mut responses = Vec::new();
    for message in messages {
        if let Some(response) = handle_message(&askl_data, &index_store, message).await {
            responses.push(response);
        }
    }

    if responses.is_empty() {
        return HttpResponse::Accepted().finish();
    }

    let response_body = if is_batch {
        Value::Array(responses)
    } else {
        responses.into_iter().next().unwrap_or_else(|| {
            jsonrpc_error_value(Value::Null, RpcError::internal("Missing response"))
        })
    };

    HttpResponse::Ok().json(response_body)
}

#[get("/mcp")]
pub async fn mcp_get() -> impl Responder {
    HttpResponse::MethodNotAllowed().finish()
}

#[derive(Debug, Deserialize)]
struct InitializeParams {
    #[serde(rename = "protocolVersion")]
    protocol_version: Option<String>,
}

#[derive(Debug, Serialize)]
struct InitializeResult {
    #[serde(rename = "protocolVersion")]
    protocol_version: String,
    #[serde(rename = "serverInfo")]
    server_info: ServerInfo,
    capabilities: ServerCapabilities,
}

#[derive(Debug, Serialize)]
struct ServerInfo {
    name: String,
    version: String,
}

#[derive(Debug, Serialize)]
struct ServerCapabilities {
    #[serde(skip_serializing_if = "Option::is_none")]
    tools: Option<ToolsCapability>,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    list_changed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ToolsListResult {
    tools: Vec<ToolDefinition>,
}

#[derive(Debug, Serialize)]
struct ToolDefinition {
    name: &'static str,
    description: &'static str,
    #[serde(rename = "inputSchema")]
    input_schema: Value,
}

#[derive(Debug, Deserialize)]
struct ToolsCallParams {
    name: String,
    arguments: Option<Value>,
}

#[derive(Debug, Serialize)]
struct ToolCallResult {
    content: Vec<ContentBlock>,
    #[serde(rename = "isError", skip_serializing_if = "Option::is_none")]
    is_error: Option<bool>,
}

#[derive(Debug, Serialize)]
#[serde(tag = "type", rename_all = "lowercase")]
enum ContentBlock {
    Text { text: String },
}

#[derive(Debug)]
struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    fn invalid_request(message: &str) -> Self {
        Self {
            code: -32600,
            message: message.to_string(),
            data: None,
        }
    }

    fn parse_error(message: &str) -> Self {
        Self {
            code: -32700,
            message: message.to_string(),
            data: None,
        }
    }

    fn method_not_found() -> Self {
        Self {
            code: -32601,
            message: "Method not found".to_string(),
            data: None,
        }
    }

    fn invalid_params() -> Self {
        Self {
            code: -32602,
            message: "Invalid params".to_string(),
            data: None,
        }
    }

    fn internal(message: &str) -> Self {
        Self {
            code: -32603,
            message: message.to_string(),
            data: None,
        }
    }
}

fn jsonrpc_result_value(id: Value, result: Value) -> Value {
    json!({
        "jsonrpc": MCP_JSONRPC_VERSION,
        "id": id,
        "result": result
    })
}

fn jsonrpc_error_value(id: Value, error: RpcError) -> Value {
    json!({
        "jsonrpc": MCP_JSONRPC_VERSION,
        "id": id,
        "error": {
            "code": error.code,
            "message": error.message,
            "data": error.data
        }
    })
}

async fn handle_message(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    message: Value,
) -> Option<Value> {
    let obj = match message.as_object() {
        Some(obj) => obj,
        None => {
            return Some(jsonrpc_error_value(
                Value::Null,
                RpcError::invalid_request("Invalid Request"),
            ));
        }
    };

    if let Some(version) = obj.get("jsonrpc") {
        if version != MCP_JSONRPC_VERSION {
            let id = obj.get("id").cloned().unwrap_or(Value::Null);
            return Some(jsonrpc_error_value(
                id,
                RpcError::invalid_request("Invalid Request: unsupported jsonrpc version"),
            ));
        }
    } else {
        let id = obj.get("id").cloned().unwrap_or(Value::Null);
        return Some(jsonrpc_error_value(
            id,
            RpcError::invalid_request("Invalid Request: missing jsonrpc version"),
        ));
    }

    if let Some(method_value) = obj.get("method") {
        let method = match method_value.as_str() {
            Some(method) => method,
            None => {
                let id = obj.get("id").cloned().unwrap_or(Value::Null);
                return Some(jsonrpc_error_value(
                    id,
                    RpcError::invalid_request("Invalid Request: method must be a string"),
                ));
            }
        };
        let params = obj.get("params").cloned();
        let id = obj.get("id").cloned();
        if id.is_none() {
            handle_notification(method).await;
            return None;
        }

        let id = id.unwrap_or(Value::Null);
        return match dispatch_method(askl_data, index_store, method, params).await {
            Ok(result) => Some(jsonrpc_result_value(id, result)),
            Err(err) => Some(jsonrpc_error_value(id, err)),
        };
    }

    if obj.contains_key("result") || obj.contains_key("error") {
        return None;
    }

    let id = obj.get("id").cloned().unwrap_or(Value::Null);
    Some(jsonrpc_error_value(
        id,
        RpcError::invalid_request("Invalid Request"),
    ))
}

async fn handle_notification(method: &str) {
    if method == "notifications/initialized" {
        return;
    }
}

async fn dispatch_method(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    method: &str,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    match method {
        "initialize" => {
            let params: InitializeParams = parse_params(params)?;
            let protocol_version = params
                .protocol_version
                .unwrap_or_else(|| MCP_DEFAULT_PROTOCOL_VERSION.to_string());
            let result = InitializeResult {
                protocol_version,
                server_info: ServerInfo {
                    name: "askld-mcp".to_string(),
                    version: env!("CARGO_PKG_VERSION").to_string(),
                },
                capabilities: ServerCapabilities {
                    tools: Some(ToolsCapability { list_changed: Some(false) }),
                },
            };
            serde_json::to_value(result)
                .map_err(|_| RpcError::internal("Failed to serialize result"))
        }
        "tools/list" => {
            let result = ToolsListResult {
                tools: tool_definitions(),
            };
            serde_json::to_value(result)
                .map_err(|_| RpcError::internal("Failed to serialize result"))
        }
        "tools/call" => {
            let call_params: ToolsCallParams = parse_params(params)?;
            let arguments = call_params.arguments.unwrap_or_else(|| json!({}));
            let result = match call_params.name.as_str() {
                TOOL_ASKL_QUERY_RUN => tool_askl_query_run(askl_data, arguments).await,
                TOOL_ASKL_PROJECTS_LIST => tool_askl_projects_list(index_store).await,
                TOOL_ASKL_PROJECT_DETAILS => {
                    tool_askl_project_details(index_store, arguments).await
                }
                TOOL_ASKL_TREE_LIST => tool_askl_tree_list(index_store, arguments).await,
                TOOL_ASKL_SOURCE_GET => {
                    tool_askl_source_get(askl_data, index_store, arguments).await
                }
                _ => return Err(RpcError::invalid_params()),
            };

            let tool_result = match result {
                Ok(value) => ToolCallResult {
                    content: vec![value_to_text_content(value)],
                    is_error: None,
                },
                Err(err) => ToolCallResult {
                    content: vec![value_to_text_content(err)],
                    is_error: Some(true),
                },
            };

            serde_json::to_value(tool_result)
                .map_err(|_| RpcError::internal("Failed to serialize tool result"))
        }
        _ => Err(RpcError::method_not_found()),
    }
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    let value = params.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|_| RpcError::invalid_params())
}

fn value_to_text_content(value: Value) -> ContentBlock {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
    ContentBlock::Text { text }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: TOOL_ASKL_QUERY_RUN,
            description:
                "Run an Askl semantic query over the indexed code graph and return graph results. Use for nontrivial code navigation, call chains, and architecture questions; narrow iteratively with follow-up queries.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Askl query string, include @preamble and @project(...)."
                    }
                }
            }),
        },
        ToolDefinition {
            name: TOOL_ASKL_PROJECTS_LIST,
            description: "List available indexed projects.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: TOOL_ASKL_PROJECT_DETAILS,
            description: "Fetch metadata for a single indexed project.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["project_id"],
                "properties": {
                    "project_id": { "type": "integer" }
                }
            }),
        },
        ToolDefinition {
            name: TOOL_ASKL_TREE_LIST,
            description: "List project tree nodes for a path.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["project_id"],
                "properties": {
                    "project_id": { "type": "integer" },
                    "path": { "type": "string", "description": "Absolute POSIX path (defaults to /)." },
                    "expand": {
                        "type": "array",
                        "items": { "type": "string" }
                    },
                    "compact": { "type": "boolean" }
                }
            }),
        },
        ToolDefinition {
            name: TOOL_ASKL_SOURCE_GET,
            description: "Fetch file contents by project path or file_id with byte ranges.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "oneOf": [
                    {
                        "required": ["file_id"],
                        "properties": {
                            "file_id": { "type": "integer" },
                            "start_offset": { "type": "integer", "minimum": 0 },
                            "end_offset": { "type": "integer", "minimum": 0 }
                        }
                    },
                    {
                        "required": ["project_id", "path"],
                        "properties": {
                            "project_id": { "type": "integer" },
                            "path": { "type": "string" },
                            "start_offset": { "type": "integer", "minimum": 0 },
                            "end_offset": { "type": "integer", "minimum": 0 }
                        }
                    }
                ]
            }),
        },
    ]
}

#[derive(Debug, Deserialize)]
struct QueryRunArgs {
    query: String,
}

async fn tool_askl_query_run(
    askl_data: &web::Data<AsklData>,
    arguments: Value,
) -> Result<Value, Value> {
    let args: QueryRunArgs = parse_args(arguments)?;
    let started = Instant::now();
    let result = execute_query(&askl_data.cfg, &args.query).await;
    match result {
        Ok(graph) => {
            let stats = graph_stats(&graph);
            let payload = json!({
                "graph": graph,
                "stats": stats
            });
            Ok(wrap_with_meta(payload, started))
        }
        Err(QueryFailure::BadRequest(err)) => Err(wrap_tool_error(
            400,
            "Query parse/execute error",
            Some(err),
            started,
        )),
        Err(QueryFailure::Timeout) => Err(wrap_tool_error(
            408,
            "Query timed out",
            None,
            started,
        )),
    }
}

async fn tool_askl_projects_list(index_store: &web::Data<IndexStore>) -> Result<Value, Value> {
    let started = Instant::now();
    match index_store.list_projects().await {
        Ok(projects) => Ok(wrap_with_meta(json!({ "projects": projects }), started)),
        Err(StoreError::Storage(message)) => {
            error!("Failed to list projects: {}", message);
            Err(wrap_tool_error(500, "Failed to list projects", None, started))
        }
    }
}

#[derive(Debug, Deserialize)]
struct ProjectDetailsArgs {
    project_id: i32,
}

async fn tool_askl_project_details(
    index_store: &web::Data<IndexStore>,
    arguments: Value,
) -> Result<Value, Value> {
    let args: ProjectDetailsArgs = parse_args(arguments)?;
    let started = Instant::now();
    match index_store.get_project_details(args.project_id).await {
        Ok(Some(details)) => Ok(wrap_with_meta(json!({ "project": details }), started)),
        Ok(None) => Err(wrap_tool_error(404, "Project not found", None, started)),
        Err(StoreError::Storage(message)) => {
            error!("Failed to load project {}: {}", args.project_id, message);
            Err(wrap_tool_error(500, "Failed to load project", None, started))
        }
    }
}

#[derive(Debug, Deserialize)]
struct TreeListArgs {
    project_id: i32,
    path: Option<String>,
    #[serde(default)]
    expand: Vec<String>,
    compact: Option<bool>,
}

#[derive(Debug, Serialize)]
struct TreeResponse {
    base_path: String,
    nodes: Vec<askld::index_store::ProjectTreeNode>,
    expanded: std::collections::HashMap<String, Vec<askld::index_store::ProjectTreeNode>>,
}

async fn tool_askl_tree_list(
    index_store: &web::Data<IndexStore>,
    arguments: Value,
) -> Result<Value, Value> {
    let args: TreeListArgs = parse_args(arguments)?;
    let started = Instant::now();

    let mut path = args.path.unwrap_or_else(|| "/".to_string());
    if path.is_empty() {
        path = "/".to_string();
    }
    if !path.starts_with('/') {
        return Err(wrap_tool_error(400, "path must be an absolute path", None, started));
    }
    for expand_path in &args.expand {
        if !expand_path.starts_with('/') {
            return Err(wrap_tool_error(
                400,
                "expand must be absolute paths",
                None,
                started,
            ));
        }
    }

    let compact = args.compact.unwrap_or(true);

    let base_nodes = match index_store
        .list_project_tree(args.project_id, &path, compact)
        .await
    {
        Ok(ProjectTreeResult::Nodes(nodes)) => nodes,
        Ok(ProjectTreeResult::ProjectNotFound) => {
            return Err(wrap_tool_error(404, "Project not found", None, started));
        }
        Ok(ProjectTreeResult::NotDirectory) => {
            return Err(wrap_tool_error(400, "path is not a directory", None, started));
        }
        Err(StoreError::Storage(message)) => {
            error!(
                "Failed to load project tree {}: {}",
                args.project_id, message
            );
            return Err(wrap_tool_error(500, "Failed to load project tree", None, started));
        }
    };

    let mut expanded = std::collections::HashMap::new();
    for expand_path in &args.expand {
        let nodes = match index_store
            .list_project_tree(args.project_id, expand_path, compact)
            .await
        {
            Ok(ProjectTreeResult::Nodes(nodes)) => nodes,
            Ok(ProjectTreeResult::ProjectNotFound) => {
                return Err(wrap_tool_error(404, "Project not found", None, started));
            }
            Ok(ProjectTreeResult::NotDirectory) => {
                return Err(wrap_tool_error(
                    400,
                    &format!("expand path is not a directory: {}", expand_path),
                    None,
                    started,
                ));
            }
            Err(StoreError::Storage(message)) => {
                error!(
                    "Failed to load project tree {}: {}",
                    args.project_id, message
                );
                return Err(wrap_tool_error(500, "Failed to load project tree", None, started));
            }
        };
        expanded.insert(expand_path.clone(), nodes);
    }

    let response = TreeResponse {
        base_path: path,
        nodes: base_nodes,
        expanded,
    };

    Ok(wrap_with_meta(json!({ "tree": response }), started))
}

#[derive(Debug, Deserialize)]
struct SourceGetArgs {
    project_id: Option<i32>,
    path: Option<String>,
    file_id: Option<i64>,
    start_offset: Option<i64>,
    end_offset: Option<i64>,
}

async fn tool_askl_source_get(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    arguments: Value,
) -> Result<Value, Value> {
    let args: SourceGetArgs = parse_args(arguments)?;
    let started = Instant::now();

    let content = if let Some(file_id) = args.file_id {
        let file_id = FileId::from(file_id);
        match askl_data.cfg.index.get_file_contents(file_id).await {
            Ok(source) => source.into_bytes(),
            Err(_) => {
                return Err(wrap_tool_error(404, "File not found", None, started));
            }
        }
    } else if let (Some(project_id), Some(path)) = (args.project_id, args.path.as_ref()) {
        let path = path.trim();
        if path.is_empty() {
            return Err(wrap_tool_error(400, "path is required", None, started));
        }
        if !path.starts_with('/') {
            return Err(wrap_tool_error(400, "path must be an absolute path", None, started));
        }
        match index_store
            .get_project_file_contents_by_path(project_id, path)
            .await
        {
            Ok(Some(content)) => content,
            Ok(None) => return Err(wrap_tool_error(404, "File not found", None, started)),
            Err(StoreError::Storage(message)) => {
                error!(
                    "Failed to load project source {}: {}",
                    project_id, message
                );
                return Err(wrap_tool_error(500, "Failed to load project source", None, started));
            }
        }
    } else {
        return Err(wrap_tool_error(
            400,
            "Provide file_id or project_id + path",
            None,
            started,
        ));
    };

    let slice = match slice_content(content, args.start_offset, args.end_offset) {
        Ok(slice) => slice,
        Err(message) => return Err(wrap_tool_error(400, &message, None, started)),
    };

    let content_text = String::from_utf8(slice.clone()).ok();
    let encoded = BASE64_ENGINE.encode(&slice);
    let start_offset = args.start_offset.unwrap_or(0);
    let end_offset = args.end_offset.unwrap_or(start_offset + slice.len() as i64);

    let mut payload = json!({
        "content_base64": encoded,
        "content_encoding": "base64",
        "range": {
            "start_offset": start_offset,
            "end_offset": end_offset,
            "len": slice.len()
        }
    });

    if let Some(text) = content_text {
        if let Value::Object(ref mut map) = payload {
            map.insert("content_text".to_string(), Value::String(text));
        }
    }

    if let Some(file_id) = args.file_id {
        if let Value::Object(ref mut map) = payload {
            map.insert("file_id".to_string(), Value::Number(Number::from(file_id)));
        }
    }

    if let Some(project_id) = args.project_id {
        if let Value::Object(ref mut map) = payload {
            map.insert("project_id".to_string(), Value::Number(Number::from(project_id)));
        }
    }

    if let Some(path) = args.path {
        if let Value::Object(ref mut map) = payload {
            map.insert("path".to_string(), Value::String(path));
        }
    }

    Ok(wrap_with_meta(payload, started))
}

fn slice_content(
    content: Vec<u8>,
    start_offset: Option<i64>,
    end_offset: Option<i64>,
) -> Result<Vec<u8>, String> {
    let len = content.len();
    let start = start_offset.unwrap_or(0);
    let end = end_offset.unwrap_or(len as i64);
    if start < 0 || end < 0 {
        return Err("Offsets must be non-negative".to_string());
    }
    let start = start as usize;
    let end = end as usize;
    if start > end || end > len {
        return Err("Invalid offset range".to_string());
    }
    Ok(content[start..end].to_vec())
}

fn parse_args<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, Value> {
    serde_json::from_value::<T>(arguments)
        .map_err(|_| wrap_tool_error(400, "Invalid arguments", None, Instant::now()))
}

fn graph_stats(graph: &Graph) -> Value {
    json!({
        "node_count": graph.nodes.len(),
        "edge_count": graph.edges.len(),
        "file_count": graph.files.len()
    })
}

fn wrap_tool_error(
    status: u16,
    message: &str,
    error: Option<ErrorResponse>,
    started: Instant,
) -> Value {
    let error_value = json!({
        "error": ToolError {
            status,
            message: message.to_string(),
            error,
        }
    });
    wrap_with_meta(error_value, started)
}

fn wrap_with_meta(payload: Value, started: Instant) -> Value {
    let mut map = match payload {
        Value::Object(map) => map,
        other => {
            let mut map = Map::new();
            map.insert("data".to_string(), other);
            map
        }
    };

    map.insert("limitations".to_string(), limitations_value());
    map.insert(
        "telemetry".to_string(),
        json!({
            "latency_ms": started.elapsed().as_millis(),
            "bytes_out": 0
        }),
    );

    let mut result = Value::Object(map);
    let bytes_out = serde_json::to_vec(&result)
        .map(|data| data.len())
        .unwrap_or(0);
    if let Value::Object(ref mut map) = result {
        if let Some(Value::Object(ref mut telemetry)) = map.get_mut("telemetry") {
            telemetry.insert(
                "bytes_out".to_string(),
                Value::Number(Number::from(bytes_out as u64)),
            );
        }
    }

    result
}

fn limitations_value() -> Value {
    json!({
        "project_scope": PROJECT_SCOPE,
        "read_only": true,
        "graph_scope": "functions/methods with direct caller/callee edges",
        "language": "go",
        "multi_hop": "explicit"
    })
}

#[derive(Debug, Serialize)]
struct ToolError {
    status: u16,
    message: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    error: Option<ErrorResponse>,
}
