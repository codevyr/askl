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
#[allow(dead_code)]
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
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<ResourcesCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompts: Option<PromptsCapability>,
}

#[derive(Debug, Serialize)]
struct ToolsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    list_changed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct ResourcesCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    list_changed: Option<bool>,
}

#[derive(Debug, Serialize)]
struct PromptsCapability {
    #[serde(rename = "listChanged", skip_serializing_if = "Option::is_none")]
    list_changed: Option<bool>,
}

// === Resources types ===

#[derive(Debug, Serialize)]
struct ResourcesListResult {
    resources: Vec<ResourceDefinition>,
}

#[derive(Debug, Serialize)]
struct ResourceDefinition {
    uri: &'static str,
    name: &'static str,
    description: &'static str,
    #[serde(rename = "mimeType")]
    mime_type: &'static str,
}

#[derive(Debug, Deserialize)]
struct ResourceReadParams {
    uri: String,
}

#[derive(Debug, Serialize)]
struct ResourceReadResult {
    contents: Vec<ResourceContent>,
}

#[derive(Debug, Serialize)]
struct ResourceContent {
    uri: String,
    #[serde(rename = "mimeType")]
    mime_type: &'static str,
    text: String,
}

// === Prompts types ===

#[derive(Debug, Serialize)]
struct PromptsListResult {
    prompts: Vec<PromptDefinition>,
}

#[derive(Debug, Serialize)]
struct PromptDefinition {
    name: &'static str,
    description: &'static str,
    #[serde(skip_serializing_if = "Option::is_none")]
    arguments: Option<Vec<PromptArgument>>,
}

#[derive(Debug, Serialize)]
struct PromptArgument {
    name: &'static str,
    description: &'static str,
    required: bool,
}

#[derive(Debug, Deserialize)]
struct PromptGetParams {
    name: String,
    #[serde(default)]
    arguments: HashMap<String, String>,
}

#[derive(Debug, Serialize)]
struct PromptGetResult {
    description: String,
    messages: Vec<PromptMessage>,
}

#[derive(Debug, Serialize)]
struct PromptMessage {
    role: &'static str,
    content: PromptContent,
}

#[derive(Debug, Serialize)]
struct PromptContent {
    #[serde(rename = "type")]
    content_type: &'static str,
    text: String,
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
                    resources: Some(ResourcesCapability {
                        list_changed: Some(false),
                    }),
                    prompts: Some(PromptsCapability {
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
        "resources/list" => {
            let result = ResourcesListResult {
                resources: resource_definitions(),
            };
            serde_json::to_value(result)
                .map_err(|_| RpcError::internal("Failed to serialize result"))
        }
        "resources/read" => {
            let params: ResourceReadParams = parse_params(params)?;
            let content = resource_content(&params.uri)
                .ok_or_else(|| RpcError::invalid_params(&format!("unknown resource: {}", params.uri)))?;
            let result = ResourceReadResult {
                contents: vec![content],
            };
            serde_json::to_value(result)
                .map_err(|_| RpcError::internal("Failed to serialize result"))
        }
        "prompts/list" => {
            let result = PromptsListResult {
                prompts: prompt_definitions(),
            };
            serde_json::to_value(result)
                .map_err(|_| RpcError::internal("Failed to serialize result"))
        }
        "prompts/get" => {
            let params: PromptGetParams = parse_params(params)?;
            let result = prompt_content(&params.name, &params.arguments)
                .ok_or_else(|| RpcError::invalid_params(&format!("unknown prompt: {}", params.name)))?;
            serde_json::to_value(result)
                .map_err(|_| RpcError::internal("Failed to serialize result"))
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
            description: r#"**USE THIS FIRST** for code exploration, architecture docs, and understanding call flow. Do NOT default to grep/file-reading for structural questions.

## When to Use (BEFORE grep)

| Task | Use This Tool |
|------|---------------|
| Architecture/design docs | YES - map call graphs first |
| "What calls X?" | `{"FunctionName"}` |
| "What does X call?" | `"FunctionName" {}` |
| "How does A reach B?" | `"A" {"B"}` or `"A" {{"B"}}` |
| Understanding plugin/interface flow | YES - find implementers/callers |
| Tracing execution paths | YES - multi-hop queries |

Only use grep AFTER askl for: type definitions, string literals, config values.

## Workflow for Architecture Docs

1. **Start here**: `@project("name") "EntryPoint" {}` to map what functions call
2. **Then expand**: `@project("name") {"TargetFunc"}` to see callers
3. **Trace paths**: `@project("name") "A" {{"B"}}` for multi-hop
4. **Only then**: grep for type definitions, askl_source_get for code

## Query Syntax

- `"symbol"` — Select by name (e.g., `"RunScorePlugins"`)
- `"a" {}` — Callees: functions "a" calls
- `{"a"}` — Callers: functions that call "a"
- `{{"a"}}` — 2-hop callers (nest `{}` for more hops)
- `"a" {{}}` — 2-hop callees from "a"
- `"a" {{"b"}}` — Path from "a" to "b" (2 hops)
- `"a"; "b"` — Select both (semicolon separates)
- `!"symbol"` — Forced edge (for passed functions)

## Required Directives

- `@project("name")` — **REQUIRED**. Call askl_projects_list first.
- `@ignore(package="path")` — Exclude noisy packages
- `@preamble` — Apply directives to all statements

## Examples

```askl
# Find all callers of Score method
@project("kubernetes") {"Score"}

# What does RunScorePlugins call?
@project("kubernetes") "RunScorePlugins" {}

# Trace from scheduleOne to Score (2 hops)
@project("kubernetes") "scheduleOne" {{"Score"}}

# Multi-hop with noise filtering
@preamble @project("kubernetes") @ignore(package="fmt");
"main" {{}}
```"#,
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
            description: "**CALL THIS FIRST** when starting code exploration. Returns available Go projects. You need the project name for askl_query_run's @project() directive. After this, use askl_query_run to explore call graphs BEFORE falling back to grep.",
            input_schema: json!({
                "type": "object",
                "additionalProperties": false
            }),
        },
        ToolDefinition {
            name: Tool::ProjectDetails.as_str(),
            description: "Get project metadata: Go module paths, file/symbol counts, last indexed time. Useful to verify a project is indexed and see its scope.",
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
            description: "Browse project directory tree. Use when you know the path but not the file_id, or to explore package structure before querying. Returns file_ids needed for askl_source_get.",
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
            description: "Fetch source code for files found via askl_query_run or askl_tree_list. Use file_id from query results, or project_id+path from tree listing. Supports byte offset ranges for extracting specific functions.",
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

// === Resource definitions ===

fn resource_definitions() -> Vec<ResourceDefinition> {
    vec![
        ResourceDefinition {
            uri: "askl://workflow",
            name: "Getting Started",
            description: "READ THIS FIRST: When to use Askl tools vs grep/file reading, and the recommended workflow",
            mime_type: "text/markdown",
        },
        ResourceDefinition {
            uri: "askl://syntax",
            name: "Askl Query Syntax",
            description: "Complete reference for Askl query language syntax including selectors, operators, and directives",
            mime_type: "text/markdown",
        },
        ResourceDefinition {
            uri: "askl://limitations",
            name: "Askl Limitations",
            description: "Current limitations of the Askl semantic code graph: supported languages, scope, and known constraints",
            mime_type: "text/markdown",
        },
    ]
}

fn resource_content(uri: &str) -> Option<ResourceContent> {
    match uri {
        "askl://workflow" => Some(ResourceContent {
            uri: uri.to_string(),
            mime_type: "text/markdown",
            text: WORKFLOW_RESOURCE.to_string(),
        }),
        "askl://syntax" => Some(ResourceContent {
            uri: uri.to_string(),
            mime_type: "text/markdown",
            text: SYNTAX_RESOURCE.to_string(),
        }),
        "askl://limitations" => Some(ResourceContent {
            uri: uri.to_string(),
            mime_type: "text/markdown",
            text: LIMITATIONS_RESOURCE.to_string(),
        }),
        _ => None,
    }
}

const WORKFLOW_RESOURCE: &str = r#"# Askl-First Workflow

**CRITICAL: For architecture docs and code exploration, START with askl_query_run, not grep.**

Grep finds text matches. Askl finds actual call relationships from static analysis.

## Decision Tree

```
Task: Explore code / write architecture doc
    ↓
Step 1: askl_projects_list (get project name)
    ↓
Step 2: askl_query_run (map call graph)
    - "EntryPoint" {}     → what it calls
    - {"TargetFunc"}      → what calls it
    - "A" {{"B"}}         → trace path A→B
    ↓
Step 3: ONLY THEN use grep for:
    - Type/interface definitions
    - Config values, constants
    - String literals
    ↓
Step 4: askl_source_get for specific code
```

## Anti-Pattern to Avoid

❌ **WRONG**: Start with `rg` and file reads, use askl occasionally
✅ **RIGHT**: Start with askl_query_run, use grep only for definitions

## Use Askl FIRST For:

1. **Architecture/design docs** — Map call graphs before reading files
2. **"What calls X?"** — `{"FunctionName"}` (not grep)
3. **"What does X call?"** — `"FunctionName" {}` (not file reading)
4. **"How does A reach B?"** — `"A" {{"B"}}` (not manual tracing)
5. **Plugin/interface flow** — `{"Score"}` finds actual invocations
6. **Entry-point tracing** — `"main" {{}}` for call tree

## Use Grep ONLY For:

- **WHERE is X defined?** → type/interface/struct definitions
- String literals, comments, configuration values
- Constants or struct field names
- Non-Go code (Askl only supports Go)

## Example: Wrong vs Right Approach

### Task: "Document how Score plugins are invoked"

❌ **Wrong approach** (what agents typically do):
1. `rg "Score"` → finds 500+ matches
2. Read framework.go → see some code
3. `rg "RunScorePlugins"` → more matches
4. Read schedule_one.go → more code
5. Eventually try 1-2 askl queries

✅ **Right approach**:
1. `askl_projects_list` → "kubernetes"
2. `askl_query_run: @project("kubernetes") {"Score"}` → see all callers
3. `askl_query_run: @project("kubernetes") "RunScorePlugins" {}` → see what it calls
4. `askl_query_run: @project("kubernetes") "scheduleOne" {{"Score"}}` → trace the path
5. NOW grep for "type ScorePlugin interface" if needed
6. askl_source_get for specific function code

## Recommended Workflow

```
Step 1: Discover projects
   askl_projects_list → returns project names and IDs

Step 2: Locate files (if needed)
   askl_tree_list → browse directory structure

Step 3: Query the call graph
   askl_query_run with @project("name") → returns functions and relationships

Step 4: Read specific code
   askl_source_get with file_id + offsets → returns source code
   askl_source_get also accepts project_id + path
```

## Example: Tracing Kubelet Entry Path

**Task:** "Trace the kubelet main function to cli.Run"

```
1. askl_projects_list
   → Shows "kubernetes" is available

2. askl_query_run:
   @preamble @project("kubernetes");
   "main" {
     "cli.Run" {}
   };

   → Returns:
   - k8s.io/kubernetes/cmd/kubelet.main → k8s.io/component-base/cli.Run
   - k8s.io/component-base/cli.Run → k8s.io/component-base/cli.run
   - k8s.io/kubernetes/cmd/kubelet.main → app.NewKubeletCommand

3. askl_source_get: file_id from results, with start_offset + end_offset
   → Returns the function source code
```

## Example: Architecture Doc for Scheduler Plugins

**Task:** "Document how Score plugins are invoked in the scheduler"

**Step 1: Find definitions (grep)**
- Search "type ScorePlugin interface" → found in framework/interface.go
- Search "KubeSchedulerConfiguration" → found in config/v1/types.go

**Step 2: Understand call flow (askl_query_run)**
```
askl_query_run: @project("kubernetes") {"Score"}
→ Shows: RunScorePlugins calls Score, framework orchestrates it

askl_query_run: @project("kubernetes") "RunScorePlugins" {}
→ Shows: What RunScorePlugins calls (parallelization, normalization)

askl_query_run: @project("kubernetes") "PodTopologySpread.Score" {}
→ Shows: What a real Score plugin depends on
```

**Step 3: Read specific code (askl_source_get)**
- Use file_ids from query results to read implementation details

**The pattern:** Grep finds WHERE → askl_query_run shows HOW → askl_source_get shows WHAT

## Tips for Effective Queries

- **Start broad, then narrow**: `"ScheduleOne" {}` for discovery, then add @ignore for noise
- **Use package tokens**: `"pkg.scheduler"` matches all symbols under that package path
- **Iterate multi-hop**: Graph shows direct edges only; expand one hop at a time
- **Partial names work**: `"Run"` or `"Scheduler.Run"` — tokens are subset-matched

## Common Patterns

| Task | Query |
|------|-------|
| Find callers | `{"FunctionName"}` |
| Find callees | `"FunctionName" {}` |
| 2-hop callers | `{{"FunctionName"}}` |
| Trace A→B | `"A" {"B"}` |
| Multiple functions | `{"Foo"}; {"Bar"}` |
| Package scope | `"pkg.scheduler" { "Sort" }` |
| Forced reference | `!"pkg/path.Func"` (for passed functions) |
| Exclude noise | `@preamble @ignore(package="fmt"); ...` |
"#;

const SYNTAX_RESOURCE: &str = r#"# Askl Query Syntax Reference

Askl is a domain-specific language for querying Go code graphs. It finds functions, methods, and their caller/callee relationships.

## Token Matching Rules

- **Case-sensitive** — `"Run"` won't match `"run"`
- **Subset matching** — Tokens are non-ASCII-separated, so you can use partial paths:
  - `"Run"` matches any function named Run
  - `"Scheduler.Run"` matches Run method on Scheduler
  - `"pkg.scheduler"` matches symbols under `k8s.io/kubernetes/pkg/scheduler` and subpackages
- **Package tokens** — `"pkg.scheduler" { "Sort" }` finds pkg/scheduler symbols that call Sort

## Basic Selectors

- `"symbol"` — Select functions/methods by name
- `!"symbol"` — **Forced edge**: Include even if no call edges found

## Forced Edges

Graph edges represent direct function calls only. If a function is passed as a value (not called), it won't have an edge:

```go
// This is NOT a call edge - ScheduleOne is passed, not called
wait.UntilWithContext(ctx, sched.ScheduleOne, 0)
```

Use forced edges to include function references: `!"pkg/path.Func"`

## Edge Operators

- `"a" {}` — Find callees: functions that "a" calls
- `{"a"}` — Find callers: functions that call "a"
- `{{"a"}}` — 2-hop callers (nest `{}` for more hops)
- `"a" {{}}` — 2-hop callees from "a"
- `"a" {{"b"}}` — Path from "a" to "b" with 2 hops

## Statement Separator

- `"a"; "b"` — Select both (semicolon separates independent statements)

## Directives

- `@project("name")` — **Required**: Filter to a specific project
- `@module("path")` — Filter to a Go module path
- `@ignore("name")` — Exclude symbols matching name
- `@ignore(package="path")` — Exclude entire packages
- `@preamble` — Apply following directives globally

**Important**: `@preamble` blocks must contain ONLY directives (verbs), not selectors.

```askl
# CORRECT: preamble has only directives
@preamble @project("kubernetes") @ignore(package="fmt");
"main" {}

# WRONG: selector in preamble
@preamble @project("kubernetes") "main" {};
```

## Examples

```askl
# Find Handler and its direct callees
@project("myproject") "Handler" {}

# Find all callers of ProcessRequest
@project("myproject") {"ProcessRequest"}

# Multi-statement with nested calls
@preamble @project("kubernetes");
"main" {
  "cli.Run" {}
};

# Package-scoped query
@project("kubernetes") "pkg.scheduler" { "Sort" }
```

## Output Format

Queries return:
- `graph.nodes[]` — Functions/methods found
- `graph.edges[]` — Caller/callee relationships
- `graph.files[]` — Source file locations with file_id for askl_source_get
- `stats` — Counts of nodes, edges, files
- `warnings[]` — Any query warnings
"#;

const LIMITATIONS_RESOURCE: &str = r#"# Askl Limitations

## Supported Languages
- **Go only** — Other languages are not currently supported

## Graph Scope
- **Functions and methods only** — Variables, types, constants are not in the graph
- **Direct caller/callee edges** — Transitive calls require explicit multi-hop queries (`{{}}`, `{{{}}}`, etc.)
- **Static analysis** — Dynamic dispatch (interfaces, reflection) may miss some edges

## Query Constraints
- **Project required** — All queries must include `@project("name")`
- **Timeout** — Queries time out after 1 second
- **No pagination** — Large result sets return all matches

## Index Limitations
- **Read-only** — Cannot modify the code graph
- **Point-in-time** — Reflects code at indexing time, not live changes
- **Package-level granularity** — Cannot query individual statements within functions
"#;

// === Prompt definitions ===

fn prompt_definitions() -> Vec<PromptDefinition> {
    vec![
        PromptDefinition {
            name: "find_callers",
            description: "Generate a query to find all callers of a function",
            arguments: Some(vec![
                PromptArgument {
                    name: "function",
                    description: "Name of the function to find callers for",
                    required: true,
                },
                PromptArgument {
                    name: "project",
                    description: "Project name to search in",
                    required: true,
                },
                PromptArgument {
                    name: "depth",
                    description: "Number of caller hops (1-3, default: 1)",
                    required: false,
                },
            ]),
        },
        PromptDefinition {
            name: "find_callees",
            description: "Generate a query to find all functions called by a function",
            arguments: Some(vec![
                PromptArgument {
                    name: "function",
                    description: "Name of the function to find callees for",
                    required: true,
                },
                PromptArgument {
                    name: "project",
                    description: "Project name to search in",
                    required: true,
                },
                PromptArgument {
                    name: "depth",
                    description: "Number of callee hops (1-3, default: 1)",
                    required: false,
                },
            ]),
        },
        PromptDefinition {
            name: "trace_path",
            description: "Generate a query to trace the call path between two functions",
            arguments: Some(vec![
                PromptArgument {
                    name: "from",
                    description: "Starting function name",
                    required: true,
                },
                PromptArgument {
                    name: "to",
                    description: "Target function name",
                    required: true,
                },
                PromptArgument {
                    name: "project",
                    description: "Project name to search in",
                    required: true,
                },
            ]),
        },
        PromptDefinition {
            name: "kubernetes_preamble",
            description: "Standard preamble for Kubernetes codebase queries with common package ignores",
            arguments: None,
        },
    ]
}

fn prompt_content(name: &str, arguments: &HashMap<String, String>) -> Option<PromptGetResult> {
    match name {
        "find_callers" => {
            let function = arguments.get("function")?;
            let project = arguments.get("project")?;
            let depth = arguments
                .get("depth")
                .and_then(|d| d.parse::<u8>().ok())
                .unwrap_or(1)
                .min(3)
                .max(1);
            let braces = "{".repeat(depth as usize);
            let braces_close = "}".repeat(depth as usize);
            let query = format!(
                r#"@project("{}") {}"{}"{})"#,
                project, braces, function, braces_close
            );
            Some(PromptGetResult {
                description: format!("Find {}-hop callers of {} in {}", depth, function, project),
                messages: vec![PromptMessage {
                    role: "user",
                    content: PromptContent {
                        content_type: "text",
                        text: format!(
                            "Run this Askl query to find callers of `{}`:\n\n```askl\n{}\n```",
                            function, query
                        ),
                    },
                }],
            })
        }
        "find_callees" => {
            let function = arguments.get("function")?;
            let project = arguments.get("project")?;
            let depth = arguments
                .get("depth")
                .and_then(|d| d.parse::<u8>().ok())
                .unwrap_or(1)
                .min(3)
                .max(1);
            let braces = "{}".repeat(depth as usize);
            let query = format!(r#"@project("{}") "{}" {}"#, project, function, braces);
            Some(PromptGetResult {
                description: format!("Find {}-hop callees of {} in {}", depth, function, project),
                messages: vec![PromptMessage {
                    role: "user",
                    content: PromptContent {
                        content_type: "text",
                        text: format!(
                            "Run this Askl query to find functions called by `{}`:\n\n```askl\n{}\n```",
                            function, query
                        ),
                    },
                }],
            })
        }
        "trace_path" => {
            let from = arguments.get("from")?;
            let to = arguments.get("to")?;
            let project = arguments.get("project")?;
            let query = format!(r#"@project("{}") "{}" {{"{}"}}"#, project, from, to);
            Some(PromptGetResult {
                description: format!("Trace call path from {} to {} in {}", from, to, project),
                messages: vec![PromptMessage {
                    role: "user",
                    content: PromptContent {
                        content_type: "text",
                        text: format!(
                            "Run this Askl query to trace the call path from `{}` to `{}`:\n\n```askl\n{}\n```\n\nIf no path is found with one hop, try adding more hops: `\"{}\" {{\"{}\"}}` (2 hops) or `\"{}\" {{{{\"{}\"}}}}` (3 hops).",
                            from, to, query, from, to, from, to
                        ),
                    },
                }],
            })
        }
        "kubernetes_preamble" => Some(PromptGetResult {
            description: "Standard preamble for querying the Kubernetes codebase".to_string(),
            messages: vec![PromptMessage {
                role: "user",
                content: PromptContent {
                    content_type: "text",
                    text: KUBERNETES_PREAMBLE.to_string(),
                },
            }],
        }),
        _ => None,
    }
}

const KUBERNETES_PREAMBLE: &str = r#"Use this preamble when querying the Kubernetes codebase to exclude common utility packages that add noise:

```askl
@preamble
@ignore(package="builtin")
@ignore(package="fmt")
@ignore(package="context")
@ignore(package="os")
@ignore(package="log")
@ignore(package="runtime")
@ignore(package="internal")
@ignore(package="ioutil")
@ignore(package="golang")
@ignore(package="k8s.io/klog");

@preamble
@project("kubernetes");
```

After this preamble, add your query. For example:
- `"NewPodInformer" {}` — Find callees of NewPodInformer
- `{"CreatePod"}` — Find callers of CreatePod
- `"Scheduler" {{"Schedule"}}` — Trace from Scheduler to Schedule
"#;
