//! MCP prompts — canned exploration starters that expand to an `askl_run`-first
//! instruction for the model.

use serde::{Deserialize, Serialize};
use serde_json::Value;
use std::collections::HashMap;

use super::protocol::{parse_params, to_value, RpcError};

/// `prompts/list` — the catalog of available prompts.
pub(super) fn list() -> Result<Value, RpcError> {
    to_value(PromptsListResult {
        prompts: prompt_definitions(),
    })
}

/// `prompts/get` — render one prompt with its arguments substituted in.
pub(super) fn get(params: Option<Value>) -> Result<Value, RpcError> {
    let params: PromptGetParams = parse_params(params)?;
    let result = prompt_content(&params.name, &params.arguments)
        .ok_or_else(|| RpcError::invalid_params(&format!("unknown prompt: {}", params.name)))?;
    to_value(result)
}

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

fn prompt_definitions() -> Vec<PromptDefinition> {
    vec![
        PromptDefinition {
            name: "find_callers",
            description: "Find and summarize everything that calls a function.",
            arguments: Some(vec![PromptArgument {
                name: "symbol",
                description: "Function name, e.g. vfs_read.",
                required: true,
            }]),
        },
        PromptDefinition {
            name: "find_callees",
            description: "Find and summarize what a function calls.",
            arguments: Some(vec![PromptArgument {
                name: "symbol",
                description: "Function name, e.g. vfs_read.",
                required: true,
            }]),
        },
        PromptDefinition {
            name: "trace_path",
            description: "Trace how one function reaches another through the call graph.",
            arguments: Some(vec![
                PromptArgument {
                    name: "from",
                    description: "Starting function.",
                    required: true,
                },
                PromptArgument {
                    name: "to",
                    description: "Target function.",
                    required: true,
                },
            ]),
        },
    ]
}

fn prompt_content(name: &str, arguments: &HashMap<String, String>) -> Option<PromptGetResult> {
    let arg = |key: &str| arguments.get(key).cloned().unwrap_or_default();
    let (description, text) = match name {
        "find_callers" => {
            let symbol = arg("symbol");
            (
                format!("Find the callers of {}", symbol),
                format!(
                    "Using the askl MCP, find everything that calls `{symbol}`. Run `askl_run` \
with query `{{ \"{symbol}\" }}` (caller scope). If it returns nothing, read `askl://syntax` and \
check the name against `askl_projects`. Summarize the callers grouped by file, each with its \
`file:line`."
                ),
            )
        }
        "find_callees" => {
            let symbol = arg("symbol");
            (
                format!("Find what {} calls", symbol),
                format!(
                    "Using the askl MCP, find what `{symbol}` calls. Run `askl_run` with query \
`\"{symbol}\" {{ }}` (callee scope). For any callee worth understanding, re-run `askl_run` on \
that name with `projection: \"body\"`. Summarize the call flow."
                ),
            )
        }
        "trace_path" => {
            let from = arg("from");
            let to = arg("to");
            (
                format!("Trace a path from {} to {}", from, to),
                format!(
                    "Using the askl MCP, determine how `{from}` reaches `{to}` through the call \
graph. Start from `\"{from}\" {{ }}` and expand promising callees toward `{to}`, growing the \
query iteratively (e.g. `\"{from}\" {{ \"intermediate\" {{ }} }}`). If direct calls do not \
connect them, try `search(\"{to}\")` to find where it is referenced. Report the chain of calls \
with `file:line`."
                ),
            )
        }
        _ => return None,
    };
    Some(PromptGetResult {
        description,
        messages: vec![PromptMessage {
            role: "user",
            content: PromptContent {
                content_type: "text",
                text,
            },
        }],
    })
}
