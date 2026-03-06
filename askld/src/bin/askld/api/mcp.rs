use actix_web::{web, HttpRequest, HttpResponse, Responder};
use askld::auth::insecure_connections_allowed;
use askld::index_store::{IndexStore, ProjectTreeResult, StoreError};
use base64::engine::general_purpose::STANDARD as BASE64_ENGINE;
use base64::Engine as _;
use bytes::Bytes;
use futures::stream;
use index::symbols::FileId;
use log::{debug, error, info, warn};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Map, Value};
use std::collections::HashMap;
use std::sync::{Arc, Mutex};
use std::time::Instant;
use uuid::Uuid;

use super::query::{execute_query, QueryFailure};
use super::types::{slice_content, AsklData, ErrorResponse, Graph};

const MCP_JSONRPC_VERSION: &str = "2.0";
const MCP_DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

/// Maximum request body size for MCP endpoint (1MB)
pub const MAX_MCP_REQUEST_BYTES: usize = 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Tool {
    QueryRun,
    ProjectsList,
    ProjectDetails,
    TreeList,
    SourceGet,
}

impl Tool {
    fn as_str(&self) -> &'static str {
        match self {
            Tool::QueryRun => "askl_query_run",
            Tool::ProjectsList => "askl_projects_list",
            Tool::ProjectDetails => "askl_project_details",
            Tool::TreeList => "askl_tree_list",
            Tool::SourceGet => "askl_source_get",
        }
    }

    fn from_str(s: &str) -> Option<Tool> {
        match s {
            "askl_query_run" => Some(Tool::QueryRun),
            "askl_projects_list" => Some(Tool::ProjectsList),
            "askl_project_details" => Some(Tool::ProjectDetails),
            "askl_tree_list" => Some(Tool::TreeList),
            "askl_source_get" => Some(Tool::SourceGet),
            _ => None,
        }
    }
}

/// SSE session state for MCP connections
pub struct SseSession {
    id: String,
    responses: Arc<Mutex<Vec<Value>>>,
}

/// Global SSE session store
pub type SseSessionStore = Arc<Mutex<HashMap<String, Arc<SseSession>>>>;

/// Create a new SSE session store
pub fn new_sse_session_store() -> SseSessionStore {
    Arc::new(Mutex::new(HashMap::new()))
}

fn is_secure_request(req: &HttpRequest) -> bool {
    if req.connection_info().scheme().eq_ignore_ascii_case("https") {
        return true;
    }
    if let Some(value) = req.headers().get("x-forwarded-proto") {
        if let Ok(proto) = value.to_str() {
            let proto = proto.split(',').next().unwrap_or("").trim();
            if proto.eq_ignore_ascii_case("https") {
                return true;
            }
        }
    }
    false
}

/// SSE endpoint for MCP transport
/// Establishes an SSE connection and sends an endpoint event with the session-specific POST URL
pub async fn mcp_sse_handler(
    req: HttpRequest,
    sessions: web::Data<SseSessionStore>,
) -> impl Responder {
    // Check security - require HTTPS unless insecure connections allowed
    if !is_secure_request(&req) && !insecure_connections_allowed() {
        return HttpResponse::Forbidden()
            .body("SSE transport requires HTTPS (set ASKL_ALLOW_INSECURE_CONNECTIONS=true for local development)");
    }

    let session_id = Uuid::new_v4().to_string();
    let session = Arc::new(SseSession {
        id: session_id.clone(),
        responses: Arc::new(Mutex::new(Vec::new())),
    });

    // Register session
    {
        let mut store = sessions.lock().unwrap();
        store.insert(session_id.clone(), session.clone());
    }

    info!("MCP SSE session started: {}", session_id);

    // Build the endpoint URL that the client should POST to
    let host = req
        .connection_info()
        .host()
        .to_string();
    let scheme = if is_secure_request(&req) { "https" } else { "http" };
    let endpoint_url = format!("{}://{}/mcp/session/{}", scheme, host, session_id);

    // Send initial endpoint event
    let endpoint_event = format!(
        "event: endpoint\ndata: {}\n\n",
        serde_json::to_string(&json!({ "uri": endpoint_url })).unwrap()
    );

    // Create SSE response stream
    let stream = stream::once(async move { Ok::<_, std::io::Error>(Bytes::from(endpoint_event)) });

    HttpResponse::Ok()
        .content_type("text/event-stream")
        .insert_header(("Cache-Control", "no-cache"))
        .insert_header(("Connection", "keep-alive"))
        .streaming(stream)
}

/// Session-specific MCP message handler (receives POSTs from SSE clients)
pub async fn mcp_session_handler(
    askl_data: web::Data<AsklData>,
    index_store: web::Data<IndexStore>,
    sessions: web::Data<SseSessionStore>,
    path: web::Path<String>,
    body: web::Bytes,
) -> impl Responder {
    let session_id = path.into_inner();

    // Verify session exists
    {
        let store = sessions.lock().unwrap();
        if !store.contains_key(&session_id) {
            return HttpResponse::NotFound().body("Session not found");
        }
    }

    debug!("MCP session message: {}", session_id);

    // Process message using existing handler logic
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

pub async fn mcp_handler(
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

    fn invalid_params(details: &str) -> Self {
        Self {
            code: -32602,
            message: format!("Invalid params: {}", details),
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
    debug!("MCP notification: {}", method);
    if method == "notifications/initialized" {
        return;
    }
    warn!("MCP unknown notification: {}", method);
}

async fn dispatch_method(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    method: &str,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    info!("MCP request: {}", method);
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
                    tools: Some(ToolsCapability {
                        list_changed: Some(false),
                    }),
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
            debug!("MCP tools/call: {}", call_params.name);
            let arguments = call_params.arguments.unwrap_or_else(|| json!({}));
            let tool = match Tool::from_str(&call_params.name) {
                Some(t) => t,
                None => {
                    return Err(RpcError::invalid_params(&format!(
                        "unknown tool: {}",
                        call_params.name
                    )))
                }
            };
            let result = match tool {
                Tool::QueryRun => tool_askl_query_run(askl_data, arguments).await,
                Tool::ProjectsList => tool_askl_projects_list(index_store).await,
                Tool::ProjectDetails => tool_askl_project_details(index_store, arguments).await,
                Tool::TreeList => tool_askl_tree_list(index_store, arguments).await,
                Tool::SourceGet => tool_askl_source_get(askl_data, index_store, arguments).await,
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
        "ping" => Ok(json!({})),
        _ => Err(RpcError::method_not_found()),
    }
}

fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    let value = params.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|err| RpcError::invalid_params(&err.to_string()))
}

fn value_to_text_content(value: Value) -> ContentBlock {
    let text = serde_json::to_string(&value).unwrap_or_else(|_| value.to_string());
    ContentBlock::Text { text }
}

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: Tool::QueryRun.as_str(),
            description: r#"Execute an Askl semantic query over the Go code graph. Returns nodes (functions/methods), edges (caller/callee relationships), and file locations.

QUERY SYNTAX:
- "symbol" — Select by name (e.g., "NewPodInformer", "pkg.Func")
- "a" {} — Find callees of "a" (functions "a" calls)
- {"a"} — Find callers of "a" (functions that call "a")
- {{"a"}} — 2-hop callers; nest {} for more hops
- "a" {{"b"}} — "a" and 2-hop path to "b"
- "a"; "b" — Select both (semicolon separates statements)
- !"symbol" — Force include even if no edges found

DIRECTIVES (use with @preamble for global effect):
- @project("name") — Filter to project (REQUIRED; use askl_projects_list to discover available projects)
- @module("path") — Filter to Go module
- @ignore("name") or @ignore(package="path") — Exclude matches

EXAMPLES:
- @project("myproject") "Handler" {} — Handler and its direct callees
- @project("myproject") {"ProcessRequest"} — All callers of ProcessRequest
- @preamble @project("myproject") @ignore(package="vendor/"); "main" {{}} — main's 2-hop callees, excluding vendor

OUTPUT: {graph: {nodes[], edges[], files[]}, stats: {node_count, edge_count, file_count}, warnings[]}"#,
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["query"],
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "Askl query. Start with @project(\"name\") (use askl_projects_list to find available projects), use \"symbol\" for selection, {} for callees, {\"x\"} for callers. Use @preamble for global directives."
                    }
                }
            }),
        },
        ToolDefinition {
            name: Tool::ProjectsList.as_str(),
            description: "List all indexed projects. Returns project_id (required for other tools), name, and indexing status. Use this first to discover available projects and their IDs.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: Tool::ProjectDetails.as_str(),
            description: "Get detailed metadata for a project: module paths, total file count, symbol count, and last indexed timestamp. Use after askl_projects_list to inspect a specific project.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["project_id"],
                "properties": {
                    "project_id": {
                        "type": "integer",
                        "description": "Project ID from askl_projects_list"
                    }
                }
            }),
        },
        ToolDefinition {
            name: Tool::TreeList.as_str(),
            description: "Browse project directory structure. Returns immediate children of path with type (file/dir) and file_id. Use to navigate before fetching source, or to understand package layout. Note: No pagination; large directories return all entries. Use specific subpaths for large projects.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "required": ["project_id"],
                "properties": {
                    "project_id": {
                        "type": "integer",
                        "description": "Project ID from askl_projects_list"
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute POSIX path to list (default: \"/\"). Example: \"/cmd/kubelet\""
                    },
                    "expand": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Additional absolute paths to expand in same request. Example: [\"/pkg/scheduler\", \"/pkg/api\"]"
                    },
                    "compact": {
                        "type": "boolean",
                        "description": "When true (default), collapses single-child directory chains into one entry (e.g., \"/a/b/c\" instead of nested \"/a\", \"/b\", \"/c\")"
                    }
                }
            }),
        },
        ToolDefinition {
            name: Tool::SourceGet.as_str(),
            description: "Fetch file source code. Provide either file_id (from query results) OR project_id+path (from tree listing). Supports byte range slicing for large files. Returns content_text (UTF-8) and content_base64, plus range metadata.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false,
                "properties": {
                    "file_id": {
                        "type": "integer",
                        "description": "File ID from query results (graph.files[].file_id). Use this OR project_id+path."
                    },
                    "project_id": {
                        "type": "integer",
                        "description": "Project ID. Required if using path instead of file_id."
                    },
                    "path": {
                        "type": "string",
                        "description": "Absolute file path within project (e.g., \"/cmd/kubelet/app/server.go\"). Required if using project_id."
                    },
                    "start_offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "Start byte offset for range request (inclusive, default: 0)"
                    },
                    "end_offset": {
                        "type": "integer",
                        "minimum": 0,
                        "description": "End byte offset for range request (exclusive, default: end of file)"
                    }
                }
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
        Err(QueryFailure::Timeout) => Err(wrap_tool_error(408, "Query timed out", None, started)),
    }
}

async fn tool_askl_projects_list(index_store: &web::Data<IndexStore>) -> Result<Value, Value> {
    let started = Instant::now();
    match index_store.list_projects().await {
        Ok(projects) => Ok(wrap_with_meta(json!({ "projects": projects }), started)),
        Err(StoreError::Storage(message)) => {
            error!("Failed to list projects: {}", message);
            Err(wrap_tool_error(
                500,
                "Failed to list projects",
                None,
                started,
            ))
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
            Err(wrap_tool_error(
                500,
                "Failed to load project",
                None,
                started,
            ))
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
        return Err(wrap_tool_error(
            400,
            "path must be an absolute path",
            None,
            started,
        ));
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
            return Err(wrap_tool_error(
                400,
                "path is not a directory",
                None,
                started,
            ));
        }
        Err(StoreError::Storage(message)) => {
            error!(
                "Failed to load project tree {}: {}",
                args.project_id, message
            );
            return Err(wrap_tool_error(
                500,
                "Failed to load project tree",
                None,
                started,
            ));
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
                return Err(wrap_tool_error(
                    500,
                    "Failed to load project tree",
                    None,
                    started,
                ));
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
            return Err(wrap_tool_error(
                400,
                "path must be an absolute path",
                None,
                started,
            ));
        }
        match index_store
            .get_project_file_contents_by_path(project_id, path)
            .await
        {
            Ok(Some(content)) => content,
            Ok(None) => return Err(wrap_tool_error(404, "File not found", None, started)),
            Err(StoreError::Storage(message)) => {
                error!("Failed to load project source {}: {}", project_id, message);
                return Err(wrap_tool_error(
                    500,
                    "Failed to load project source",
                    None,
                    started,
                ));
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

    let mut payload = Map::new();
    payload.insert("content_base64".into(), json!(encoded));
    payload.insert("content_encoding".into(), json!("base64"));
    payload.insert(
        "range".into(),
        json!({
            "start_offset": start_offset,
            "end_offset": end_offset,
            "len": slice.len()
        }),
    );

    if let Some(text) = content_text {
        payload.insert("content_text".into(), json!(text));
    }
    if let Some(file_id) = args.file_id {
        payload.insert("file_id".into(), json!(file_id));
    }
    if let Some(project_id) = args.project_id {
        payload.insert("project_id".into(), json!(project_id));
    }
    if let Some(path) = args.path {
        payload.insert("path".into(), json!(path));
    }

    Ok(wrap_with_meta(Value::Object(payload), started))
}

fn parse_args<T: for<'de> Deserialize<'de>>(arguments: Value) -> Result<T, Value> {
    serde_json::from_value::<T>(arguments).map_err(|err| {
        wrap_tool_error(
            400,
            &format!("Invalid arguments: {}", err),
            None,
            Instant::now(),
        )
    })
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
        json!({ "latency_ms": started.elapsed().as_millis() }),
    );

    Value::Object(map)
}

fn limitations_value() -> Value {
    json!({
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
