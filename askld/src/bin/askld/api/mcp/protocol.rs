//! JSON-RPC 2.0 envelope + the shared MCP protocol types (`initialize`, error
//! objects, param/result helpers). Tool-, resource-, and prompt-specific wire
//! types live with their own modules.

use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};

pub(super) const MCP_JSONRPC_VERSION: &str = "2.0";
const MCP_DEFAULT_PROTOCOL_VERSION: &str = "2024-11-05";

// JSON-RPC 2.0 standard error codes (https://www.jsonrpc.org/specification#error_object).
const PARSE_ERROR: i64 = -32700;
const INVALID_REQUEST: i64 = -32600;
const METHOD_NOT_FOUND: i64 = -32601;
const INVALID_PARAMS: i64 = -32602;
const INTERNAL_ERROR: i64 = -32603;

#[derive(Debug)]
pub(super) struct RpcError {
    code: i64,
    message: String,
    data: Option<Value>,
}

impl RpcError {
    pub(super) fn invalid_request(message: &str) -> Self {
        Self {
            code: INVALID_REQUEST,
            message: message.to_string(),
            data: None,
        }
    }

    pub(super) fn parse_error(message: &str) -> Self {
        Self {
            code: PARSE_ERROR,
            message: message.to_string(),
            data: None,
        }
    }

    pub(super) fn method_not_found() -> Self {
        Self {
            code: METHOD_NOT_FOUND,
            message: "Method not found".to_string(),
            data: None,
        }
    }

    pub(super) fn invalid_params(details: &str) -> Self {
        Self {
            code: INVALID_PARAMS,
            message: format!("Invalid params: {}", details),
            data: None,
        }
    }

    pub(super) fn internal(message: &str) -> Self {
        Self {
            code: INTERNAL_ERROR,
            message: message.to_string(),
            data: None,
        }
    }
}

pub(super) fn jsonrpc_result_value(id: Value, result: Value) -> Value {
    json!({ "jsonrpc": MCP_JSONRPC_VERSION, "id": id, "result": result })
}

pub(super) fn jsonrpc_error_value(id: Value, error: RpcError) -> Value {
    json!({
        "jsonrpc": MCP_JSONRPC_VERSION,
        "id": id,
        "error": { "code": error.code, "message": error.message, "data": error.data }
    })
}

/// Deserialize JSON-RPC `params` into `T`, defaulting a missing `params` to `{}`.
pub(super) fn parse_params<T: DeserializeOwned>(params: Option<Value>) -> Result<T, RpcError> {
    let value = params.unwrap_or_else(|| json!({}));
    serde_json::from_value(value).map_err(|err| RpcError::invalid_params(&err.to_string()))
}

/// Serialize a `result` payload, mapping any failure to an internal error.
pub(super) fn to_value<T: Serialize>(result: T) -> Result<Value, RpcError> {
    serde_json::to_value(result).map_err(|_| RpcError::internal("Failed to serialize result"))
}

/// `initialize` — advertise the protocol version, server identity, and the
/// tools/resources/prompts capabilities.
pub(super) fn initialize(params: Option<Value>) -> Result<Value, RpcError> {
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
            tools: Some(ListChangedCapability::default()),
            resources: Some(ListChangedCapability::default()),
            prompts: Some(ListChangedCapability::default()),
        },
    };
    to_value(result)
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
    tools: Option<ListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    resources: Option<ListChangedCapability>,
    #[serde(skip_serializing_if = "Option::is_none")]
    prompts: Option<ListChangedCapability>,
}

/// The three capability objects (tools/resources/prompts) share one shape. We
/// don't emit list-change notifications, so `listChanged` is always `false`.
#[derive(Debug, Serialize, Default)]
struct ListChangedCapability {
    #[serde(rename = "listChanged")]
    list_changed: bool,
}
