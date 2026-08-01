//! Model Context Protocol (MCP) server surface.
//!
//! Exposes askl to an AI agent as JSON-RPC 2.0 over HTTP (`POST /mcp`, Streamable
//! HTTP: the reply is returned in the response body). The agent writes raw askl
//! query strings; every tool returns **markdown** (never JSON) rendered by the
//! same code path as `/query?format=markdown`, so the MCP and the web UI share
//! one source of truth.
//!
//! This module is the transport; the surface is split by concern:
//! [`protocol`] (envelope + `initialize`), [`tools`], [`resources`], [`prompts`].

mod prompts;
mod protocol;
mod resources;
mod tools;

use actix_web::{web, HttpResponse, Responder};
use log::{debug, info, warn};
use serde_json::{json, Value};

use super::types::AsklData;
use askld::index_store::IndexStore;
use protocol::{jsonrpc_error_value, jsonrpc_result_value, RpcError, MCP_JSONRPC_VERSION};

/// `POST /mcp` — a single JSON-RPC 2.0 message or a batch array. The reply is
/// returned in the response body.
pub async fn mcp_handler(
    askl_data: web::Data<AsklData>,
    index_store: web::Data<IndexStore>,
    body: web::Bytes,
) -> impl Responder {
    process_rpc_body(&askl_data, &index_store, &body).await
}

/// Parse a JSON-RPC request body (single or batch), dispatch each message, and
/// build the HTTP reply. Notifications (no `id`) produce no response; an
/// all-notification batch yields `202 Accepted`.
async fn process_rpc_body(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    body: &[u8],
) -> HttpResponse {
    let value: Value = match serde_json::from_slice(body) {
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
        if let Some(response) = handle_message(askl_data, index_store, message).await {
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

/// Validate one JSON-RPC envelope and route it. Returns `None` for
/// notifications and for result/error echoes (which carry no reply).
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

    match obj.get("jsonrpc") {
        Some(version) if version == MCP_JSONRPC_VERSION => {}
        Some(_) => {
            let id = obj.get("id").cloned().unwrap_or(Value::Null);
            return Some(jsonrpc_error_value(
                id,
                RpcError::invalid_request("Invalid Request: unsupported jsonrpc version"),
            ));
        }
        None => {
            let id = obj.get("id").cloned().unwrap_or(Value::Null);
            return Some(jsonrpc_error_value(
                id,
                RpcError::invalid_request("Invalid Request: missing jsonrpc version"),
            ));
        }
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
        let id = match obj.get("id").cloned() {
            None => {
                handle_notification(method).await;
                return None;
            }
            Some(id) => id,
        };
        return match dispatch_method(askl_data, index_store, method, params).await {
            Ok(result) => Some(jsonrpc_result_value(id, result)),
            Err(err) => Some(jsonrpc_error_value(id, err)),
        };
    }

    // A result/error echo from the peer — nothing to reply to.
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
    if method != "notifications/initialized" {
        warn!("MCP unknown notification: {}", method);
    }
}

async fn dispatch_method(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    method: &str,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    info!("MCP request: {}", method);
    match method {
        "initialize" => protocol::initialize(params),
        "tools/list" => tools::list(),
        "tools/call" => tools::call(askl_data, index_store, params).await,
        "resources/list" => resources::list(),
        "resources/read" => resources::read(params),
        // Fixed-URI resources only, no templates. Clients (Claude Code) probe
        // this on connect; answer with an empty list so the call succeeds.
        "resources/templates/list" => Ok(json!({ "resourceTemplates": [] })),
        "prompts/list" => prompts::list(),
        "prompts/get" => prompts::get(params),
        "ping" => Ok(json!({})),
        _ => Err(RpcError::method_not_found()),
    }
}
