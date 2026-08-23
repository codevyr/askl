//! The MCP tools (`askl_run`, `askl_projects`, `askl_read`) and their wire types.
//! Every tool returns **markdown** (never JSON); `askl_run` shares the exact
//! `/query?format=markdown` render path.

use actix_web::web;
use askld::index_store::{IndexStore, UploadStatus};
use askld::line_index::{line_to_offset, LineIndex};
use log::debug;
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use super::protocol::{parse_params, to_value, RpcError};
use crate::api::query::{build_result_graph, render_graph_markdown};
use crate::api::render::Projection;
use crate::api::types::AsklData;

/// `tools/list` — the tool catalog.
pub(super) fn list() -> Result<Value, RpcError> {
    to_value(ToolsListResult {
        tools: tool_definitions(),
    })
}

/// `tools/call` — dispatch to a tool and wrap its markdown in a tool result.
pub(super) async fn call(
    askl_data: &web::Data<AsklData>,
    index_store: &web::Data<IndexStore>,
    params: Option<Value>,
) -> Result<Value, RpcError> {
    let call: ToolsCallParams = parse_params(params)?;
    debug!("MCP tools/call: {}", call.name);
    let arguments = call.arguments.unwrap_or_else(|| json!({}));
    let output = match call.name.as_str() {
        "askl_run" => tool_askl_run(askl_data, arguments).await,
        "askl_projects" => tool_askl_projects(index_store).await,
        "askl_read" => tool_askl_read(askl_data, arguments).await,
        other => {
            return Err(RpcError::invalid_params(&format!(
                "unknown tool: {}",
                other
            )))
        }
    };
    to_value(ToolCallResult {
        content: vec![ContentBlock::Text { text: output.text }],
        is_error: if output.is_error { Some(true) } else { None },
    })
}

/// The rendered result of a tool call: markdown text plus whether it is an error
/// (surfaced to the agent via the MCP `isError` flag).
struct ToolOutput {
    text: String,
    is_error: bool,
}

impl ToolOutput {
    fn ok(text: String) -> Self {
        Self {
            text,
            is_error: false,
        }
    }

    fn error(text: String) -> Self {
        Self {
            text,
            is_error: true,
        }
    }

    /// An error result whose markdown is a `# Error` fence around `msg` — the
    /// shape every tool uses for a one-line failure.
    fn error_md(msg: impl std::fmt::Display) -> Self {
        Self::error(format!("# Error\n{}\n", msg))
    }
}

#[derive(Debug, Deserialize)]
struct AsklRunArgs {
    query: String,
    #[serde(default)]
    projection: Option<String>,
    #[serde(default)]
    limit: Option<usize>,
    /// Project names to make visible for this run; empty = every indexed
    /// project.
    #[serde(default)]
    projects: Vec<String>,
}

/// `askl_run` — execute a raw askl query and return the markdown report. Uses
/// the exact `build_result_graph` + `render_graph_markdown` path as
/// `/query?format=markdown`, so the output is identical.
async fn tool_askl_run(data: &web::Data<AsklData>, arguments: Value) -> ToolOutput {
    let args: AsklRunArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(err) => return ToolOutput::error_md(format!("Invalid arguments: {}", err)),
    };
    let projection = match args.projection.as_deref() {
        None => Projection::default(),
        Some(name) => match Projection::from_name(name) {
            Some(projection) => projection,
            None => {
                return ToolOutput::error_md(format!(
                    "unknown projection '{}'; expected 'names', 'signature', or 'body'",
                    name
                ))
            }
        },
    };

    match build_result_graph(data, &args.query, args.limit, &args.projects).await {
        Ok(graph) => {
            ToolOutput::ok(render_graph_markdown(data, &args.query, &graph, projection).await)
        }
        Err(err) => ToolOutput::error(err.to_markdown()),
    }
}

/// `askl_projects` — list the indexed projects with their scope name, root path,
/// status, and file/symbol counts. The **project name** is what `project("…")`
/// takes; the counts tell the agent how big a scope it's about to query.
async fn tool_askl_projects(store: &web::Data<IndexStore>) -> ToolOutput {
    let projects = match store.list_projects().await {
        Ok(projects) => projects,
        Err(err) => return ToolOutput::error_md(format!("Failed to list projects: {:?}", err)),
    };
    if projects.is_empty() {
        return ToolOutput::ok("# Projects\n\nNo projects are indexed.\n".to_string());
    }

    let mut md = String::from("# Projects\n\n");
    for project in projects {
        // Prefer the detailed row (file/symbol counts); fall back to the list
        // row if the project has since disappeared.
        match store.get_project_details(project.id).await {
            Ok(Some(details)) => md.push_str(&format!(
                "## {}\n- root: `{}`\n- status: {}\n- files: {}\n- symbols: {}\n\n",
                details.project_name,
                details.root_path,
                upload_status_str(details.upload_status),
                details.file_count,
                details.symbol_count,
            )),
            _ => md.push_str(&format!(
                "## {}\n- root: `{}`\n- status: {}\n\n",
                project.project_name,
                project.root_path,
                upload_status_str(project.upload_status),
            )),
        }
    }
    ToolOutput::ok(md)
}

#[derive(Debug, Deserialize)]
struct AsklReadArgs {
    file: String,
    #[serde(default)]
    project: Option<String>,
    #[serde(default)]
    start_line: Option<usize>,
    #[serde(default)]
    end_line: Option<usize>,
}

/// `askl_read` — return a line range of a source file as a fenced markdown block
/// with a `file:line` header. For reading non-symbol regions (headers, config,
/// context around a hit); symbol *bodies* come from `askl_run(projection=body)`.
/// The path is matched on a segment boundary, so `fs/read_write.c` resolves the
/// canonical `/project/fs/read_write.c` (never `.../ecryptfs/read_write.c`). When
/// several files still match, it reads the best-ranked one and lists the rest.
async fn tool_askl_read(data: &web::Data<AsklData>, arguments: Value) -> ToolOutput {
    let args: AsklReadArgs = match serde_json::from_value(arguments) {
        Ok(args) => args,
        Err(err) => return ToolOutput::error_md(format!("Invalid arguments: {}", err)),
    };

    let matches = match data
        .cfg
        .index
        .resolve_source_file(&args.file, args.project.as_deref())
        .await
    {
        Ok(matches) => matches,
        Err(err) => {
            return ToolOutput::error_md(format!("Failed to resolve '{}': {}", args.file, err))
        }
    };
    if matches.is_empty() {
        let scope = args
            .project
            .as_deref()
            .map(|p| format!(" in project '{}'", p))
            .unwrap_or_default();
        return ToolOutput::error_md(format!(
            "No file matching '{}'{} found in the index.",
            args.file, scope
        ));
    }
    // `resolve_source_file` orders exact-first then shortest-path, so the best
    // candidate leads.
    let chosen = &matches[0];

    let content = match data
        .cfg
        .index
        .get_file_contents_bytes(chosen.object_id)
        .await
    {
        Ok(content) => content,
        Err(err) => {
            return ToolOutput::error_md(format!(
                "Failed to read '{}': {}",
                chosen.filesystem_path, err
            ))
        }
    };

    let line_count = LineIndex::new(&content).line_count();
    let start_line = args.start_line.unwrap_or(1).max(1);
    let end_line = args.end_line.unwrap_or(line_count).max(start_line);

    let Some(start_off) = line_to_offset(&content, start_line) else {
        return ToolOutput::error_md(format!(
            "'{}' has {} lines; start_line {} is past the end.",
            chosen.filesystem_path, line_count, start_line
        ));
    };
    let start_off = start_off as usize;
    // Byte just after `end_line`'s newline, or end of file for the last line.
    let end_off = line_to_offset(&content, end_line + 1)
        .map(|off| off as usize)
        .unwrap_or(content.len())
        .max(start_off);

    let slice = String::from_utf8_lossy(&content[start_off..end_off]);
    let shown_end = end_line.min(line_count.max(start_line));
    let mut md = format!(
        "# {}:{}-{}\n\n```{}\n{}",
        chosen.filesystem_path,
        start_line,
        shown_end,
        lang_for_path(&chosen.filesystem_path),
        slice
    );
    if !md.ends_with('\n') {
        md.push('\n');
    }
    md.push_str("```\n");
    if matches.len() > 1 {
        md.push_str(&format!(
            "\n_{} files matched `{}`; showed `{}`. Other matches (re-read with a longer path or `project`):_\n",
            matches.len(),
            args.file,
            chosen.filesystem_path
        ));
        for other in matches.iter().skip(1) {
            md.push_str(&format!(
                "- `{}` (project {})\n",
                other.filesystem_path, other.project_name
            ));
        }
    }
    ToolOutput::ok(md)
}

fn upload_status_str(status: UploadStatus) -> &'static str {
    match status {
        UploadStatus::Uploading => "uploading",
        UploadStatus::Complete => "complete",
        UploadStatus::Failed => "failed",
        UploadStatus::Deleting => "deleting",
    }
}

/// Best-effort code-fence language hint from a file extension; `text` when unknown.
fn lang_for_path(path: &str) -> &'static str {
    let ext = path.rsplit('.').next().unwrap_or("");
    match ext {
        "c" | "h" => "c",
        "cc" | "cpp" | "cxx" | "hpp" | "hh" => "cpp",
        "go" => "go",
        "rs" => "rust",
        "py" => "python",
        "js" | "jsx" => "javascript",
        "ts" | "tsx" => "typescript",
        "java" => "java",
        "sh" => "bash",
        _ => "text",
    }
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
    annotations: ToolAnnotations,
    #[serde(rename = "_meta")]
    meta: ToolMeta,
}

/// MCP tool annotations (protocol revision 2025-03-26+). Claude Code maps
/// `readOnlyHint` onto its own `isReadOnly()`, which is what lets a tool run in
/// plan mode without asking the user for permission.
#[derive(Debug, Serialize)]
struct ToolAnnotations {
    #[serde(rename = "readOnlyHint")]
    read_only_hint: bool,
    #[serde(rename = "destructiveHint")]
    destructive_hint: bool,
    #[serde(rename = "openWorldHint")]
    open_world_hint: bool,
}

impl ToolAnnotations {
    /// Every askld tool only queries the index, so none of them is destructive.
    /// The world is open, though: which projects a given server has indexed is
    /// neither fixed nor enumerable in advance by the caller.
    ///
    /// `destructiveHint` and `openWorldHint` both default to `true` in the spec,
    /// so they are stated rather than left out.
    fn read_only() -> Self {
        Self {
            read_only_hint: true,
            destructive_hint: false,
            open_world_hint: true,
        }
    }
}

/// Anthropic's `_meta` extensions. `searchHint` is what a client with deferred
/// tool loading matches against when it decides whether to pull the tool's full
/// schema into the conversation.
#[derive(Debug, Serialize)]
struct ToolMeta {
    #[serde(rename = "anthropic/searchHint")]
    search_hint: &'static str,
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

fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: "askl_run",
            description: "Run an askl query for read-only code exploration and return a markdown \
report (symbols with `file:line`, call/containment edges, and source). Prefer this over \
grep/file-reading for structural questions — \"who calls X\", \"what does X call\", \"where is X \
defined\", \"find text\". **Read the `askl://syntax` and `askl://cookbook` resources first** so \
your query is well-formed. Core forms: `\"name\"` (definition), `\"x\" { }` (callees of x), \
`{ \"x\" }` (callers of x), `search(\"literal\")` (full-text), `file(\"/path\")` (scope), plus \
filters like `func`/`data` and `project(\"p\")`. Grow the query iteratively — the string is your \
accumulated context.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "query": {
                        "type": "string",
                        "description": "The askl query string, e.g. `\"vfs_read\" { func }`."
                    },
                    "projection": {
                        "type": "string",
                        "enum": ["names", "signature", "body"],
                        "description": "How much source to render per symbol: `names` (identifiers \
            only), `signature` (default — first line of each definition), or `body` (full definition + doc \
            comment). Raise it only for the few symbols you're deepening into."
                    },
                    "limit": {
                        "type": "integer",
                        "description": "Max distinct symbols in the result (0 = unlimited). Defaults to \
            the server cap; the report says when results were truncated."
                    },
                    "projects": {
                        "type": "array",
                        "items": { "type": "string" },
                        "description": "Names (from `askl_projects`) of the projects to search; omit \
            to search all of them. This is the cheap way to scope a query — the other projects stop \
            existing for this run, rather than being filtered out of the results afterwards. An \
            unknown name is an error, never a silently empty answer."
                    }
                },
                "required": ["query"]
            }),
            annotations: ToolAnnotations::read_only(),
            meta: ToolMeta {
                search_hint: "query indexed code structure (callers, callees, \
definitions, full-text search)",
            },
        },
        ToolDefinition {
            name: "askl_projects",
            description: "List the indexed projects with their scope name, root path, upload \
status, and file/symbol counts. Call this first to learn which project names exist — the name is \
what `project(\"…\")` takes to scope a query, and the counts show how large a scope is.",
            input_schema: json!({ "type": "object", "properties": {} }),
            annotations: ToolAnnotations::read_only(),
            meta: ToolMeta {
                search_hint: "list indexed code projects available to query",
            },
        },
        ToolDefinition {
            name: "askl_read",
            description:
                "Read a line range of a source file as fenced markdown with a `file:line` \
header. Use for non-symbol regions (headers, config, or context around a `search()` hit); for a \
symbol's own body prefer `askl_run` with `projection: \"body\"`. The `file` path is matched on a \
segment boundary, so `fs/read_write.c` resolves the canonical project-prefixed path; if several \
files still match it reads the best one and lists the rest.",
            input_schema: json!({
                "type": "object",
                "properties": {
                    "file": {
                        "type": "string",
                        "description": "File path (or path suffix), e.g. `fs/read_write.c`."
                    },
                    "project": {
                        "type": "string",
                        "description": "Optional project name to disambiguate when the same path \
            exists in several projects (see `askl_projects`)."
                    },
                    "start_line": {
                        "type": "integer",
                        "description": "1-based first line (default 1)."
                    },
                    "end_line": {
                        "type": "integer",
                        "description": "1-based last line, inclusive (default: end of file)."
                    }
                },
                "required": ["file"]
            }),
            annotations: ToolAnnotations::read_only(),
            meta: ToolMeta {
                search_hint: "read a line range from an indexed source file",
            },
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The plan-mode gate in an MCP client is driven entirely by `readOnlyHint`,
    /// and a deferred tool is only discoverable through its search hint, so a
    /// tool that ships without either is silently degraded rather than broken.
    #[test]
    fn every_tool_is_annotated_read_only_and_searchable() {
        let value = list().expect("tools/list");
        let tools = value["tools"].as_array().expect("tools array");
        assert_eq!(tools.len(), 3);
        for tool in tools {
            let name = &tool["name"];
            assert_eq!(
                tool["annotations"],
                json!({
                    "readOnlyHint": true,
                    "destructiveHint": false,
                    "openWorldHint": true
                }),
                "tool {} has unexpected annotations",
                name
            );
            assert!(
                tool["_meta"]["anthropic/searchHint"]
                    .as_str()
                    .is_some_and(|hint| !hint.is_empty()),
                "tool {} is missing a search hint",
                name
            );
        }
    }
}
