//! MCP resources — the askl documentation the agent should read before querying.
//! Bodies live as markdown files under `resources/` and are embedded with
//! `include_str!`, so the docs are editable as data, not string literals.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::protocol::{parse_params, to_value, RpcError};

const SYNTAX_RESOURCE: &str = include_str!("resources/syntax.md");
const WORKFLOW_RESOURCE: &str = include_str!("resources/workflow.md");
const COOKBOOK_RESOURCE: &str = include_str!("resources/cookbook.md");
const LIMITATIONS_RESOURCE: &str = include_str!("resources/limitations.md");

const MIME_MARKDOWN: &str = "text/markdown";

/// `resources/list` — the catalog of available resources.
pub(super) fn list() -> Result<Value, RpcError> {
    to_value(ResourcesListResult {
        resources: resource_definitions(),
    })
}

/// `resources/read` — the body of one resource by URI.
pub(super) fn read(params: Option<Value>) -> Result<Value, RpcError> {
    let params: ResourceReadParams = parse_params(params)?;
    let content = resource_content(&params.uri)
        .ok_or_else(|| RpcError::invalid_params(&format!("unknown resource: {}", params.uri)))?;
    to_value(ResourceReadResult {
        contents: vec![content],
    })
}

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

fn resource_definitions() -> Vec<ResourceDefinition> {
    vec![
        ResourceDefinition {
            uri: "askl://syntax",
            name: "Askl syntax reference",
            description: "The askl query language: selectors, scopes (callers/callees/has), \
filters, paths, search(). Read this before writing queries.",
            mime_type: MIME_MARKDOWN,
        },
        ResourceDefinition {
            uri: "askl://workflow",
            name: "Exploration workflow",
            description: "How to explore a codebase with askl: discover projects, root a query, \
narrow, deepen the projection, and read source.",
            mime_type: MIME_MARKDOWN,
        },
        ResourceDefinition {
            uri: "askl://cookbook",
            name: "Query cookbook",
            description: "Copy-paste askl recipes for the common exploration questions.",
            mime_type: MIME_MARKDOWN,
        },
        ResourceDefinition {
            uri: "askl://limitations",
            name: "Limitations",
            description: "What askl does and does not cover: indexed languages, symbol focus, \
static edges, read-only, result caps.",
            mime_type: MIME_MARKDOWN,
        },
    ]
}

fn resource_content(uri: &str) -> Option<ResourceContent> {
    let text = match uri {
        "askl://syntax" => SYNTAX_RESOURCE,
        "askl://workflow" => WORKFLOW_RESOURCE,
        "askl://cookbook" => COOKBOOK_RESOURCE,
        "askl://limitations" => LIMITATIONS_RESOURCE,
        _ => return None,
    };
    Some(ResourceContent {
        uri: uri.to_string(),
        mime_type: MIME_MARKDOWN,
        text: text.to_string(),
    })
}
