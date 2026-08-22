use actix_web::http::StatusCode;
use actix_web::{get, post, web, HttpResponse, Responder};
use askld::execution_context::ExecutionContext;
use askld::offset_range::range_bounds_to_offsets;
use askld::parser::parse;
use index::db_diesel::RootLayer;
use index::symbols::{FileId, InstanceType, SymbolId, SymbolInstanceId, SymbolType};
use log::{debug, info, warn};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tokio::time::timeout;

use super::render::{render_markdown, Projection, SourceMap};
use super::types::{
    AsklData, Edge, ErrorResponse, Graph, GraphObjectEntry, HasEdge, Node, NodeSymbolInstance,
    QueryStatement,
};

const MAX_RESPONSE_BYTES: usize = 1_024 * 1_024; // 1 MB

fn is_statement_timeout(err: &pest::error::Error<askld::parser::Rule>) -> bool {
    match &err.variant {
        pest::error::ErrorVariant::CustomError { message } => message.contains("statement timeout"),
        _ => false,
    }
}

/// One-line hint appended to markdown parse/exec errors so the agent can
/// self-correct rather than fall back to grep.
const SYNTAX_HINT: &str = "Check query syntax: selectors `\"name\"`, callees `\"x\" { }`, \
callers `{ \"x\" }`, filters like `func`/`file(\"p\")`, scope `project(\"p\")`.";

/// Build an error response in the caller's requested format. JSON mode is
/// unchanged (the serialized `ErrorResponse`); markdown mode fences the pest
/// error (which already renders the `^---` caret) and appends `hint` when given.
/// `hint` is `Some(SYNTAX_HINT)` only for *parse* errors — a timeout is not a
/// syntax problem, so passing `None` there avoids steering the agent to rewrite
/// correct syntax instead of narrowing scope.
fn error_response(
    want_markdown: bool,
    status: StatusCode,
    err: &pest::error::Error<askld::parser::Rule>,
    hint: Option<&str>,
) -> HttpResponse {
    if want_markdown {
        let mut md = format!("# Error\n```text\n{}\n```\n", err);
        if let Some(hint) = hint {
            md.push_str(&format!("\n{}\n", hint));
        }
        HttpResponse::build(status)
            .content_type("text/markdown; charset=utf-8")
            .body(md)
    } else {
        let json_err = serde_json::to_string(&ErrorResponse::from_pest(err)).unwrap();
        HttpResponse::build(status)
            .content_type("application/json")
            .body(json_err)
    }
}

/// Error surfaced while turning a query string into a result graph. Carries the
/// pest error (which renders its own `^---` caret) so the HTTP handler and the
/// MCP tool present the same message. `Storage` is a plain string for infra
/// failures that have no source span.
#[derive(Debug)]
pub enum QueryError {
    Parse(pest::error::Error<askld::parser::Rule>),
    Exec(pest::error::Error<askld::parser::Rule>),
    Timeout(pest::error::Error<askld::parser::Rule>),
    Storage(String),
    /// The request itself is malformed — today, a `projects` narrowing that
    /// names something the index does not have.  Distinct from `Storage`
    /// because the caller can fix it, so it answers 400 rather than 500.
    Request(String),
}

impl QueryError {
    /// Render as an HTTP response in the caller's requested format, matching the
    /// original `/query` error behavior exactly.
    fn into_http_response(self, want_markdown: bool) -> HttpResponse {
        match self {
            QueryError::Parse(err) => error_response(
                want_markdown,
                StatusCode::BAD_REQUEST,
                &err,
                Some(SYNTAX_HINT),
            ),
            QueryError::Exec(err) => error_response(
                want_markdown,
                StatusCode::BAD_REQUEST,
                &err,
                Some(SYNTAX_HINT),
            ),
            QueryError::Timeout(err) => {
                error_response(want_markdown, StatusCode::GATEWAY_TIMEOUT, &err, None)
            }
            QueryError::Storage(msg) => HttpResponse::InternalServerError().body(msg),
            QueryError::Request(msg) => HttpResponse::BadRequest().body(msg),
        }
    }

    /// Render as markdown for the MCP tool surface — the same `# Error` fence the
    /// HTTP handler emits for `format=markdown`, with the syntax hint on
    /// parse/exec errors (a timeout isn't a syntax problem, so it gets none).
    pub fn to_markdown(&self) -> String {
        match self {
            QueryError::Parse(err) | QueryError::Exec(err) => {
                format!("# Error\n```text\n{}\n```\n\n{}\n", err, SYNTAX_HINT)
            }
            QueryError::Timeout(err) => format!("# Error\n```text\n{}\n```\n", err),
            QueryError::Storage(msg) | QueryError::Request(msg) => format!("# Error\n{}\n", msg),
        }
    }
}

#[post("/query")]
pub async fn query(
    data: web::Data<AsklData>,
    opts: web::Query<QueryOpts>,
    req_body: String,
) -> impl Responder {
    let _query = tracing::info_span!("query").entered();

    // Output-shape options are parsed up front so a bad value fails fast,
    // before we run the query. `format=json` (default) is unchanged.
    let want_markdown = match opts.format.as_deref() {
        None | Some("json") => false,
        Some("markdown") | Some("md") => true,
        Some(other) => {
            return HttpResponse::BadRequest().body(format!(
                "unknown format '{other}'; expected 'json' or 'markdown'"
            ));
        }
    };
    let projection = match opts.projection.as_deref() {
        None => Projection::default(),
        Some(name) => match Projection::from_name(name) {
            Some(p) => p,
            None => {
                return HttpResponse::BadRequest().body(format!(
                    "unknown projection '{name}'; expected 'names', 'signature', or 'body'"
                ));
            }
        },
    };

    let projects = parse_project_list(opts.projects.as_deref());
    let result_graph = match build_result_graph(&data, &req_body, opts.limit, &projects).await {
        Ok(graph) => graph,
        Err(err) => return err.into_http_response(want_markdown),
    };

    if want_markdown {
        let md = render_graph_markdown(&data, &req_body, &result_graph, projection).await;
        return HttpResponse::Ok()
            .content_type("text/markdown; charset=utf-8")
            .body(md);
    }

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    if json_graph.len() > MAX_RESPONSE_BYTES {
        return HttpResponse::PayloadTooLarge().body("Response too large");
    }
    HttpResponse::Ok().body(json_graph)
}

/// Narrow the visible root set to the named projects.
///
/// Visibility is a property of the REQUEST, not of the query text: the roots
/// decide which projects exist at all for this run, while `project("…")`
/// inside a query is a predicate over the rows those roots make visible. The
/// two compose — narrowing to `a` and then writing `project("b")` yields
/// nothing, because `b` is not visible to begin with.
///
/// An empty `names` slice means "no narrowing requested" and keeps every root.
/// Naming a project the index does not have is a client error rather than an
/// empty result: silence there is indistinguishable from "that project has no
/// matches", which is the failure mode this whole entry point exists to avoid.
///
/// Cache note: root shards are keyed on ONE root's identity
/// (`root_shard_hash` folds `root.hash`), so a narrowed run REUSES the shards
/// a broader run populated — never mints a parallel set.
/// `per_root_root_shard_reused_under_narrowed_roots` fences that property.
fn narrow_roots(all: Vec<RootLayer>, names: &[String]) -> Result<Vec<RootLayer>, QueryError> {
    if names.is_empty() {
        return Ok(all);
    }

    let unknown: Vec<&str> = names
        .iter()
        .filter(|wanted| !all.iter().any(|root| &root.name == *wanted))
        .map(|s| s.as_str())
        .collect();
    if !unknown.is_empty() {
        let mut known: Vec<&str> = all.iter().map(|r| r.name.as_str()).collect();
        known.sort_unstable();
        return Err(QueryError::Request(format!(
            "unknown project(s): {}. Indexed projects: {}",
            unknown.join(", "),
            if known.is_empty() {
                "(none)".to_string()
            } else {
                known.join(", ")
            }
        )));
    }

    Ok(all
        .into_iter()
        .filter(|root| names.iter().any(|wanted| wanted == &root.name))
        .collect())
}

/// Parse, execute, and assemble the capped result graph for `query_text`. Shared
/// by the `/query` HTTP handler and the MCP `askl_run` tool so both produce the
/// same graph (and thus the same markdown). `limit` overrides the server's
/// default symbol cap (`None` → `data.max_result_symbols`; `0` → unlimited).
/// `projects` narrows the visible roots (empty = every indexed project).
pub async fn build_result_graph(
    data: &AsklData,
    query_text: &str,
    limit: Option<usize>,
    projects: &[String],
) -> Result<Graph, QueryError> {
    debug!("Received query: {}", query_text);
    let ast = parse(query_text).map_err(|err| {
        info!("Parse error: {}", err);
        QueryError::Parse(err)
    })?;
    debug!("Global scope: {:#?}", ast);

    // Resolve the visible root layers, then narrow them to the projects this
    // request asked for. RAM-cached via the SQL result cache, so the narrowing
    // costs one filter, not one query per root set.
    let roots = data.cfg.index.load_root_layers().await.map_err(|err| {
        warn!("Failed to resolve root layers: {}", err);
        QueryError::Storage("Failed to resolve root layers".to_string())
    })?;
    let roots = narrow_roots(roots, projects)?;
    let mut ctx = ExecutionContext::new(roots);
    ctx.probe_cap = data.probe_cap;
    // Push the result cap into the leaf SQL (the budget facet of fusion): the
    // renderer below still applies the exact per-symbol cap, but bounding the
    // leaves stops bare selectors from materialising far more rows than the cap
    // can ever keep.  Uses the same `cap` the renderer does (see below).
    ctx.eph
        .set_result_budget(index::db_diesel::ResultBudget::symbols(
            limit.unwrap_or(data.max_result_symbols),
        ));

    let res = {
        let _query_execute = tracing::info_span!("query_execute").entered();
        let execute_future = ast.execute(&mut ctx, &data.cfg);
        match timeout(data.query_timeout, execute_future).await {
            Ok(Err(err)) => {
                if is_statement_timeout(&err) {
                    warn!("Query timed out (PG statement_timeout)");
                    return Err(QueryError::Timeout(err));
                }
                // A usage error (unknown verb, bad params …).
                return Err(QueryError::Exec(err));
            }
            Ok(Ok(res)) => res,
            Err(_) => {
                warn!(
                    "Query timed out (tokio timeout after {:?})",
                    data.query_timeout
                );
                let span = ctx
                    .current_statement_span
                    .clone()
                    .unwrap_or_else(|| askld::span::Span::synthetic(query_text));
                let err = pest::error::Error::<askld::parser::Rule>::new_from_span(
                    pest::error::ErrorVariant::CustomError {
                        message: format!(
                            "Query exceeded the {:?} time limit while executing this statement",
                            data.query_timeout
                        ),
                    },
                    span.as_pest_span(),
                );
                return Err(QueryError::Timeout(err));
            }
        }
    };

    info!("Symbols: {:#?}", res.nodes.as_vec().len());
    info!("Edges: {:#?}", res.edges.0.len());
    info!("Has edges: {:#?}", res.has_edges.0.len());

    let _build_response = tracing::debug_span!("build_response").entered();
    let mut result_graph = Graph::new();

    // Pass 1: distinct symbols with a representative (path, offset) sort key,
    // plus the object→project map used for edge attribution.
    let mut all_symbols = HashSet::new();
    let mut object_projects = HashMap::new();
    let mut sym_key: HashMap<i64, (String, i32)> = HashMap::new();
    for node in res.nodes.0.iter() {
        all_symbols.insert(node.symbol.clone());
        let object_id = FileId::new(node.object.id);
        object_projects
            .entry(object_id)
            .or_insert(node.object.project_id);
        let start = range_bounds_to_offsets(&node.symbol_instance.offset_range)
            .map(|(s, _)| s)
            .unwrap_or(0);
        let key = (node.object.filesystem_path.clone(), start);
        sym_key
            .entry(node.symbol.id)
            .and_modify(|k| {
                if key < *k {
                    *k = key.clone();
                }
            })
            .or_insert(key);
    }

    // E1 cap: keep the first `cap` distinct symbols (by path, offset, id) and
    // prune edges/has_edges to that set so the subgraph stays referentially
    // valid. `limit=0` (or a 0 default) means unlimited.
    let cap = limit.unwrap_or(data.max_result_symbols);
    result_graph.total_symbols = all_symbols.len();
    let (kept, truncated) = select_kept(&sym_key, cap);
    result_graph.truncated = truncated;

    for (from, to, loc) in res.edges.0 {
        if !(kept.contains(&from.symbol_id.0) && kept.contains(&to.symbol_id.0)) {
            continue;
        }
        let from_project_id = loc.as_ref().and_then(|occurrence| {
            object_projects
                .get(&occurrence.file)
                .map(|id| id.to_string())
        });
        result_graph.add_edge(Edge::new(
            from.symbol_id,
            to.symbol_id,
            loc,
            from_project_id,
        ));
    }

    for has_edge in res.has_edges.0 {
        if !(kept.contains(&has_edge.parent.0) && kept.contains(&has_edge.child.0)) {
            continue;
        }
        result_graph.add_has_edge(HasEdge::new(
            has_edge.parent,
            has_edge.child,
            has_edge.parent_instance,
            has_edge.child_instance,
        ));
    }

    let mut result_objects = HashMap::new();
    for symbol in all_symbols {
        if !kept.contains(&symbol.id) {
            continue;
        }
        let mut seen_stmts = HashSet::new();
        let mut query_stmts = Vec::new();
        let mut symbol_instances = Vec::new();

        for n in res
            .nodes
            .0
            .iter()
            .filter(|n| n.symbol_instance.symbol == symbol.id)
        {
            for stmt in &n.query_statements {
                if seen_stmts.insert((stmt.start, stmt.end)) {
                    query_stmts.push(QueryStatement {
                        start: stmt.start,
                        end: stmt.end,
                        text: stmt.text.clone(),
                    });
                }
            }
            let object_id = FileId::new(n.object.id);
            result_objects.entry(object_id).or_insert(GraphObjectEntry {
                object_id: object_id.to_string(),
                path: n.object.filesystem_path.clone(),
                project_id: n.object.project_id.to_string(),
            });
            let (start_offset, end_offset) =
                range_bounds_to_offsets(&n.symbol_instance.offset_range).unwrap();
            symbol_instances.push(NodeSymbolInstance {
                id: SymbolInstanceId::new(n.symbol_instance.id).to_string(),
                symbol: SymbolId(n.symbol_instance.symbol).to_string(),
                object_id: FileId::new(n.object.id).to_string(),
                project_id: n.object.project_id.to_string(),
                symbol_type: SymbolType::from(symbol.symbol_type),
                instance_type: InstanceType::from(n.symbol_instance.instance_type),
                start_offset,
                end_offset,
            });
        }

        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            symbol_instances,
            query_stmts,
        ));
    }

    result_graph.objects = result_objects.into_values().collect();
    result_graph.add_warnings(res.warnings);

    Ok(result_graph)
}

/// Fetch the raw bytes of every file the graph references, then render the graph
/// as markdown. Raw bytes (not the lossy `String` accessor) keep byte offsets
/// aligned so `file:line` locations are exact. Shared by `/query?format=markdown`
/// and the MCP `askl_run` tool.
pub async fn render_graph_markdown(
    data: &AsklData,
    query_text: &str,
    graph: &Graph,
    projection: Projection,
) -> String {
    let mut sources = SourceMap::new();
    for obj in &graph.objects {
        let Ok(id) = obj.object_id.parse::<i32>() else {
            continue;
        };
        match data
            .cfg
            .index
            .get_file_contents_bytes(FileId::new(id))
            .await
        {
            Ok(bytes) => {
                sources.insert(obj.object_id.clone(), bytes);
            }
            Err(err) => debug!("markdown: no content for object {}: {}", obj.object_id, err),
        }
    }

    render_markdown(query_text, graph, &sources, projection)
}

#[derive(Debug, Deserialize)]
pub struct QueryOpts {
    /// `json` (default) or `markdown`.
    format: Option<String>,
    /// `names` | `signature` | `body` (default `signature`), markdown only.
    projection: Option<String>,
    /// Max distinct symbols in the result; `0` = unlimited. Defaults to the
    /// server's `max_result_symbols`.
    limit: Option<usize>,
    /// Comma-separated project names to make visible; absent = every indexed
    /// project. Names come from `/projects`; an unknown one is a 400.
    projects: Option<String>,
}

/// Split a `projects=a,b` query parameter into names, dropping empty entries so
/// a trailing comma is not an unknown project called "".
fn parse_project_list(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Choose which symbols survive the E1 cap, deterministically keeping the first
/// `cap` by `(path, offset, symbol id)`. `cap == 0` means unlimited. Returns the
/// kept id set and whether truncation occurred.
fn select_kept(sym_key: &HashMap<i64, (String, i32)>, cap: usize) -> (HashSet<i64>, bool) {
    if cap == 0 || sym_key.len() <= cap {
        return (sym_key.keys().copied().collect(), false);
    }
    let mut ids: Vec<i64> = sym_key.keys().copied().collect();
    ids.sort_by(|a, b| {
        let ka = &sym_key[a];
        let kb = &sym_key[b];
        (ka.0.as_str(), ka.1, *a).cmp(&(kb.0.as_str(), kb.1, *b))
    });
    ids.truncate(cap);
    (ids.into_iter().collect(), true)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str, off: i32) -> (String, i32) {
        (path.to_string(), off)
    }

    #[test]
    fn select_kept_unlimited_and_under_cap() {
        let mut m = HashMap::new();
        m.insert(1i64, key("/a", 0));
        m.insert(2, key("/b", 0));

        let (kept, trunc) = select_kept(&m, 0); // 0 = unlimited
        assert!(!trunc);
        assert_eq!(kept.len(), 2);

        let (kept, trunc) = select_kept(&m, 5); // total <= cap
        assert!(!trunc);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn select_kept_caps_by_path_offset_id() {
        let mut m = HashMap::new();
        m.insert(10i64, key("/b", 0)); // 3rd by (path, offset)
        m.insert(11, key("/a", 50)); // 2nd
        m.insert(12, key("/a", 10)); // 1st

        let (kept, trunc) = select_kept(&m, 2);
        assert!(trunc);
        assert_eq!(kept.len(), 2);
        assert!(kept.contains(&12) && kept.contains(&11));
        assert!(!kept.contains(&10));
    }

    fn root(id: i64, name: &str) -> RootLayer {
        RootLayer {
            id,
            project_id: id as i32,
            name: name.to_string(),
            hash: vec![id as u8; 32],
        }
    }

    fn roots() -> Vec<RootLayer> {
        vec![root(1, "linux"), root(2, "rdma-core"), root(3, "ucx")]
    }

    #[test]
    fn no_narrowing_keeps_every_root() {
        let kept = narrow_roots(roots(), &[]).unwrap();
        assert_eq!(kept, roots(), "an absent `projects` must not narrow");
    }

    #[test]
    fn narrowing_keeps_the_named_roots_in_load_order() {
        let kept = narrow_roots(roots(), &["ucx".to_string(), "linux".to_string()]).unwrap();
        let names: Vec<&str> = kept.iter().map(|r| r.name.as_str()).collect();
        // Load order, not argument order: roots are sorted by layer id, and
        // `EphContext::rooted` sorts them again — the request cannot reorder
        // visibility.
        assert_eq!(names, vec!["linux", "ucx"]);
    }

    #[test]
    fn an_unknown_project_is_an_error_naming_what_exists() {
        let err = narrow_roots(roots(), &["lnux".to_string()]).unwrap_err();
        let QueryError::Request(msg) = err else {
            panic!("an unknown project must be a client error");
        };
        assert!(msg.contains("lnux"), "must name the unknown project: {msg}");
        assert!(
            msg.contains("linux") && msg.contains("rdma-core") && msg.contains("ucx"),
            "must list what is indexed: {msg}"
        );
    }

    #[test]
    fn one_bad_name_among_good_ones_still_errors() {
        // Narrowing to {linux, nope} must not quietly answer over linux
        // alone: the caller asked a question about a project that is not
        // there, and an answer would look like "no matches".
        let err = narrow_roots(roots(), &["linux".to_string(), "nope".to_string()]).unwrap_err();
        assert!(matches!(err, QueryError::Request(_)));
    }

    #[test]
    fn project_list_splits_trims_and_drops_blanks() {
        assert_eq!(parse_project_list(None), Vec::<String>::new());
        assert_eq!(parse_project_list(Some("")), Vec::<String>::new());
        assert_eq!(
            parse_project_list(Some("linux, rdma-core ,")),
            vec!["linux".to_string(), "rdma-core".to_string()],
            "a trailing comma must not become a project named \"\""
        );
    }
}

#[derive(Debug, Deserialize)]
pub struct SourceRangeQuery {
    start_offset: Option<i64>,
    end_offset: Option<i64>,
}

#[get["/source/{file_id}"]]
pub async fn file(
    data: web::Data<AsklData>,
    file_id: web::Path<FileId>,
    range: web::Query<SourceRangeQuery>,
) -> impl Responder {
    let _source = tracing::info_span!("source").entered();

    let file_id = *file_id;

    debug!("Received request for file: {}", file_id);
    if let Ok(source) = data.cfg.index.get_file_contents(file_id).await {
        let content = source.into_bytes();
        match slice_content(content, range.start_offset, range.end_offset) {
            Ok(slice) => HttpResponse::Ok().body(slice),
            Err(response) => response,
        }
    } else {
        HttpResponse::NotFound().body("File not found")
    }
}

fn slice_content(
    content: Vec<u8>,
    start_offset: Option<i64>,
    end_offset: Option<i64>,
) -> Result<Vec<u8>, HttpResponse> {
    let len = content.len();
    let start = start_offset.unwrap_or(0);
    let end = end_offset.unwrap_or(len as i64);
    if start < 0 || end < 0 {
        return Err(HttpResponse::BadRequest().body("Offsets must be non-negative"));
    }
    let start = start as usize;
    let end = end as usize;
    if start > end || end > len {
        return Err(HttpResponse::BadRequest().body("Invalid offset range"));
    }
    Ok(content[start..end].to_vec())
}
