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
    let result_graph =
        match build_result_graph(&data, &req_body, opts.limit, &projects, opts.listing).await {
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
pub(crate) fn narrow_roots(
    all: Vec<RootLayer>,
    names: &[String],
) -> Result<Vec<RootLayer>, QueryError> {
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
    listing: Option<usize>,
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
    // Read and probe share one cap, so "Resolved" (probe ≤ cap) and "not
    // truncated" (read ≤ cap) name the same threshold.
    ctx.eph.set_read_cap(data.probe_cap);
    // Result limits. `symbols` bounds distinct nodes per command; `listing`
    // bounds every per-node listing (instances, out-edges, call-sites per
    // pair). Both have a server default here, are overridable per request,
    // and are overridden per command by the `limits()` verb (carried on each
    // node's query_statements provenance, applied below). 0 = unlimited.
    let symbols_default = limit.unwrap_or(data.max_result_symbols);
    let listing_default = listing.unwrap_or(data.listing_cap);
    // The out-edge fetch is DISCOVERY-side (a windowed SQL LIMIT set on the
    // eph before execution), so it must cover the largest `listing` any
    // command requests; render then trims each node's instances and
    // call-sites to its own listing. `0` (unlimited) on either side wins.
    let combine_unlimited = |a: usize, b: usize| if a == 0 || b == 0 { 0 } else { a.max(b) };
    let max_listing = match ast.max_listing_limit() {
        Some(v) => combine_unlimited(listing_default, v),
        None => listing_default,
    };
    ctx.eph.set_edge_cap(max_listing);
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
    // Symbols per originating COMMAND (keyed by the command's query-text
    // range, carried on every node): the result cap is per command.
    let mut stmt_symbols: HashMap<(usize, usize), HashSet<i64>> = HashMap::new();
    // Per-command `symbols` limit and per-symbol `listing` limit from the
    // `limits()` verb (None = fall back to the server default). A symbol
    // reachable from several commands takes the most generous listing.
    let mut stmt_symbols_limit: HashMap<(usize, usize), Option<usize>> = HashMap::new();
    let mut sym_listing: HashMap<i64, Option<usize>> = HashMap::new();
    for node in res.nodes.0.iter() {
        all_symbols.insert(node.symbol.clone());
        for stmt in &node.query_statements {
            let span = (stmt.start, stmt.end);
            stmt_symbols.entry(span).or_default().insert(node.symbol.id);
            stmt_symbols_limit.entry(span).or_insert(stmt.symbols_limit);
            if let Some(v) = stmt.listing_limit {
                sym_listing
                    .entry(node.symbol.id)
                    .and_modify(|cur| *cur = Some(cur.map_or(v, |c| c.max(v))))
                    .or_insert(Some(v));
            } else {
                sym_listing.entry(node.symbol.id).or_insert(None);
            }
        }
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

    // The result cap is PER STATEMENT: every selector is its own request
    // with its own budget, so a dedicated anchor can never be evicted by
    // another statement's derived fan-out. Within a statement, survivors are
    // deterministic by (path, offset, id); a symbol kept by ANY of its
    // statements survives. `limit=0` (or a 0 default) means unlimited.
    result_graph.total_symbols = all_symbols.len();
    let (kept, truncated) = select_kept_per_command(
        &sym_key,
        &stmt_symbols,
        &stmt_symbols_limit,
        symbols_default,
    );
    result_graph.truncated = truncated;

    // Truncated-node list (assembly capping): keep only symbols that survive
    // the symbol cap, same referential rule as edges below.
    result_graph.edges_truncated = kept_truncated_ids(&res.edges_truncated, &kept);

    // Objects ship only if something in the RESPONSE references them — a
    // kept edge occurrence or a kept (post-cap) instance.  Registering every
    // result instance's object shipped hundreds of entries nothing pointed
    // at once the caps trimmed the payload.
    let mut shipped_objects: HashSet<String> = HashSet::new();

    // The client draws ONE edge per (from, to) symbol pair; every entry
    // beyond the first only feeds that pair's call-site listing.  A single
    // hot pair can carry thousands of occurrences (measured: 2,454 of 2,898
    // entries on ONE amdgpu pair), so occurrences are capped PER PAIR at
    // render — assembly capping: the overflowing from-symbol joins
    // `edges_truncated` and the references endpoint pages the rest.  The
    // edge set is a HashSet, so sort first or the surviving occurrences
    // would be iteration-order lottery.
    let mut kept_edges: Vec<_> = res
        .edges
        .0
        .iter()
        .filter(|(from, to, _)| kept.contains(&from.symbol_id.0) && kept.contains(&to.symbol_id.0))
        .collect();
    kept_edges.sort_by_key(|(from, to, loc)| {
        (
            from.symbol_id.0,
            to.symbol_id.0,
            loc.as_ref().map(|o| (o.file.to_string(), o.offset_range.0)),
        )
    });
    let listing_for = |sym: i64| -> usize {
        sym_listing
            .get(&sym)
            .copied()
            .flatten()
            .unwrap_or(listing_default)
    };
    let mut pair_counts: HashMap<(i64, i64), usize> = HashMap::new();
    let mut occurrence_overflow: HashSet<i64> = HashSet::new();
    for (from, to, loc) in kept_edges {
        let n = pair_counts
            .entry((from.symbol_id.0, to.symbol_id.0))
            .or_insert(0);
        *n += 1;
        let occ_cap = listing_for(from.symbol_id.0);
        if occ_cap != 0 && *n > occ_cap {
            occurrence_overflow.insert(from.symbol_id.0);
            continue;
        }
        let from_project_id = loc.as_ref().and_then(|occurrence| {
            object_projects
                .get(&occurrence.file)
                .map(|id| id.to_string())
        });
        if let Some(occurrence) = loc {
            shipped_objects.insert(occurrence.file.to_string());
        }
        result_graph.add_edge(Edge::new(
            from.symbol_id,
            to.symbol_id,
            loc.clone(),
            from_project_id,
        ));
    }
    for sym in occurrence_overflow {
        result_graph.edges_truncated.push(sym.to_string());
    }
    result_graph.edges_truncated.sort_unstable();
    result_graph.edges_truncated.dedup();

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

        let (symbol_instances, instances_truncated, result_instances) =
            cap_instances(symbol_instances, listing_for(symbol.id));
        // The result's own instance count is bounded by the leaf budget
        // (limit x 8), so it is NOT the symbol's real total — amdgpu showed
        // "800" for a symbol with 931 instances. For truncated nodes (the
        // only ones that display a total) ask the index for the exact
        // per-symbol count: index-driven and bounded by the symbol's own
        // size. On error, fall back to the result count rather than failing
        // the whole response.
        let total_instances = if instances_truncated {
            data.cfg
                .index
                .count_symbol_instances(symbol.id, &ctx.eph)
                .await
                .unwrap_or(result_instances)
        } else {
            result_instances
        };
        for inst in &symbol_instances {
            shipped_objects.insert(inst.object_id.clone());
        }
        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            symbol_instances,
            query_stmts,
            instances_truncated,
            total_instances,
        ));
    }

    result_graph.objects = result_objects
        .into_iter()
        .filter(|(id, _)| shipped_objects.contains(&id.to_string()))
        .map(|(_, entry)| entry)
        .collect();
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
    /// Max distinct symbols PER STATEMENT (each selector gets its own
    /// budget); `0` = unlimited. Defaults to the server's
    /// `max_result_symbols`.
    limit: Option<usize>,
    /// Comma-separated project names to make visible; absent = every indexed
    /// project. Names come from `/projects`; an unknown one is a 400.
    projects: Option<String>,
    /// Per-node listing cap (instances, out-edges, call-sites per pair);
    /// `0` = uncapped for this request. Defaults to the server's
    /// `listing_cap`, and is overridden per command by the `limits()` verb.
    /// Truncated listings are surfaced via `edges_truncated` /
    /// `instances_truncated` for fetch-more, never warned about.
    listing: Option<usize>,
}

/// Split a `projects=a,b` query parameter into names, dropping empty entries so
/// a trailing comma is not an unknown project called "".
pub(crate) fn parse_project_list(raw: Option<&str>) -> Vec<String> {
    raw.map(|s| {
        s.split(',')
            .map(str::trim)
            .filter(|name| !name.is_empty())
            .map(str::to_string)
            .collect()
    })
    .unwrap_or_default()
}

/// Choose which symbols survive the result cap.  The cap applies PER
/// COMMAND — each selector is its own request with its own budget, so a
/// command's nodes never compete with another command's (a dedicated anchor
/// cannot be evicted by a neighbour's fan-out; the failure mode is
/// unrepresentable rather than merely deprioritised).  Each command's cap is
/// its `limits(symbols=)` value (from `stmt_limit`) or `default` when unset.
/// Within a command, the first `cap` symbols by `(path, offset, symbol id)`
/// are kept; a symbol kept by ANY command survives.  `cap == 0` = unlimited.
/// Returns the kept id set and whether ANY command was truncated.
fn select_kept_per_command(
    sym_key: &HashMap<i64, (String, i32)>,
    stmt_symbols: &HashMap<(usize, usize), HashSet<i64>>,
    stmt_limit: &HashMap<(usize, usize), Option<usize>>,
    default: usize,
) -> (HashSet<i64>, bool) {
    let mut kept: HashSet<i64> = HashSet::new();
    let mut truncated = false;
    for (span, symbols) in stmt_symbols.iter() {
        let cap = stmt_limit.get(span).copied().flatten().unwrap_or(default);
        if cap == 0 || symbols.len() <= cap {
            kept.extend(symbols.iter().copied());
            continue;
        }
        truncated = true;
        let mut ids: Vec<i64> = symbols.iter().copied().collect();
        ids.sort_by(|a, b| {
            let ka = &sym_key[a];
            let kb = &sym_key[b];
            (ka.0.as_str(), ka.1, *a).cmp(&(kb.0.as_str(), kb.1, *b))
        });
        ids.truncate(cap);
        kept.extend(ids);
    }
    (kept, truncated)
}

/// Filter the truncated-node report to symbols that survived the symbol cap
/// (same referential rule as edges: never name a node the response dropped),
/// serialized in `Node.id`'s string encoding.
fn kept_truncated_ids(edges_truncated: &[SymbolId], kept: &HashSet<i64>) -> Vec<String> {
    edges_truncated
        .iter()
        .filter(|sym| kept.contains(&sym.0))
        .map(|sym| sym.0.to_string())
        .collect()
}

/// Render-side instance cap (assembly capping): keep at most `cap` instances
/// per node, REAL ones first.  Sorted by (directory-instance?, object,
/// offset, id): sentinel/containment instances sort last so a truncated list
/// can never misrepresent a code symbol as a directory node, and the
/// definition a renderer looks for stays in the kept prefix.  `cap == 0`
/// disables.  Returns `(kept, truncated, total)` — the total is the result's
/// exact count, not a corpus total.
fn cap_instances(
    mut instances: Vec<NodeSymbolInstance>,
    cap: usize,
) -> (Vec<NodeSymbolInstance>, bool, usize) {
    let total = instances.len();
    if cap == 0 || total <= cap {
        return (instances, false, total);
    }
    let is_directory = |i: &NodeSymbolInstance| {
        matches!(
            i.instance_type,
            index::symbols::InstanceType::Sentinel | index::symbols::InstanceType::Containment
        )
    };
    instances.sort_by(|a, b| {
        (is_directory(a), &a.object_id, a.start_offset, &a.id).cmp(&(
            is_directory(b),
            &b.object_id,
            b.start_offset,
            &b.id,
        ))
    });
    instances.truncate(cap);
    (instances, true, total)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(path: &str, off: i32) -> (String, i32) {
        (path.to_string(), off)
    }

    fn inst(id: &str, itype: index::symbols::InstanceType, off: i32) -> NodeSymbolInstance {
        NodeSymbolInstance {
            id: id.to_string(),
            symbol: "1".to_string(),
            object_id: "1".to_string(),
            project_id: "p".to_string(),
            symbol_type: index::symbols::SymbolType::Function,
            instance_type: itype,
            start_offset: off,
            end_offset: off + 1,
        }
    }

    #[test]
    fn cap_instances_keeps_real_instances_first_and_reports_the_total() {
        use index::symbols::InstanceType::{Containment, Definition, Sentinel};
        let all = || {
            vec![
                inst("a", Containment, 0),
                inst("b", Sentinel, 5),
                inst("c", Definition, 10),
                inst("d", Definition, 20),
            ]
        };
        // Under the cap: untouched, unsorted, not truncated.
        let (kept, trunc, total) = cap_instances(all(), 10);
        assert!(!trunc);
        assert_eq!((kept.len(), total), (4, 4));
        assert_eq!(kept[0].id, "a", "no cap hit => original order preserved");

        // Capped: the DEFINITIONS survive, the directory instances drop —
        // a truncated list must never turn a code symbol into a directory
        // node or lose the definition the renderer looks for.
        let (kept, trunc, total) = cap_instances(all(), 2);
        assert!(trunc);
        assert_eq!(total, 4);
        let ids: Vec<&str> = kept.iter().map(|i| i.id.as_str()).collect();
        assert_eq!(ids, vec!["c", "d"]);

        // cap == 0 disables.
        let many = vec![inst("x", Definition, 0), inst("y", Definition, 1)];
        let (kept, trunc, _) = cap_instances(many, 0);
        assert!(!trunc);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn kept_truncated_ids_drops_symbols_the_cap_dropped() {
        let truncated = vec![SymbolId::new(5), SymbolId::new(7)];
        let kept: HashSet<i64> = [5i64, 6].into_iter().collect();
        assert_eq!(
            kept_truncated_ids(&truncated, &kept),
            vec!["5".to_string()],
            "a truncated node the symbol cap dropped must not be reported"
        );
    }

    fn stmts(groups: &[(usize, &[i64])]) -> HashMap<(usize, usize), HashSet<i64>> {
        groups
            .iter()
            .map(|(i, ids)| ((*i, *i + 1), ids.iter().copied().collect()))
            .collect()
    }

    /// No per-command `limits()` overrides — every group uses the default.
    fn no_limits() -> HashMap<(usize, usize), Option<usize>> {
        HashMap::new()
    }

    #[test]
    fn select_kept_unlimited_and_under_cap() {
        let mut m = HashMap::new();
        m.insert(1i64, key("/a", 0));
        m.insert(2, key("/b", 0));
        let st = stmts(&[(0, &[1, 2])]);

        let (kept, trunc) = select_kept_per_command(&m, &st, &no_limits(), 0); // 0 = unlimited
        assert!(!trunc);
        assert_eq!(kept.len(), 2);

        let (kept, trunc) = select_kept_per_command(&m, &st, &no_limits(), 5); // total <= cap
        assert!(!trunc);
        assert_eq!(kept.len(), 2);
    }

    #[test]
    fn the_cap_is_per_command_so_a_dedicated_selector_always_shows() {
        // The live scenario: statement 0 fans out over symbols whose paths
        // sort FIRST and overflow the cap; statement 1 anchors one symbol
        // whose path sorts LAST.  Under a global cap the anchor loses the
        // lottery; per statement, it cannot even compete.
        let mut m = HashMap::new();
        m.insert(1i64, key("/drivers/a", 0));
        m.insert(2, key("/drivers/b", 0));
        m.insert(3, key("/drivers/c", 0));
        m.insert(4, key("/fs/sysfs", 0));
        let st = stmts(&[(0, &[1, 2, 3]), (10, &[4])]);

        let (kept, trunc) = select_kept_per_command(&m, &st, &no_limits(), 2);
        assert!(trunc, "statement 0 overflowed its budget");
        assert!(kept.contains(&4), "the dedicated selector keeps its node");
        assert!(
            kept.contains(&1) && kept.contains(&2) && !kept.contains(&3),
            "statement 0 keeps its own first `cap` by (path, offset, id)"
        );
    }

    #[test]
    fn a_shared_symbol_survives_if_any_statement_keeps_it() {
        // Symbol 3 sorts past statement 0's budget but is also statement 1's
        // only node — kept.
        let mut m = HashMap::new();
        m.insert(1i64, key("/a", 0));
        m.insert(2, key("/b", 0));
        m.insert(3, key("/c", 0));
        let st = stmts(&[(0, &[1, 2, 3]), (10, &[3])]);

        let (kept, trunc) = select_kept_per_command(&m, &st, &no_limits(), 2);
        assert!(trunc);
        assert!(
            kept.contains(&3),
            "statement 1 keeps what statement 0 dropped"
        );
        assert_eq!(kept.len(), 3);
    }

    #[test]
    fn a_per_command_limits_verb_raises_that_commands_budget() {
        // Command at span (0,1) has 4 symbols; its `limits(symbols=3)` lets 3
        // through while command (10,11) uses the (smaller) default of 1.
        let mut m = HashMap::new();
        m.insert(1i64, key("/a", 0));
        m.insert(2, key("/b", 0));
        m.insert(3, key("/c", 0));
        m.insert(4, key("/d", 0));
        m.insert(5, key("/e", 0));
        let st = stmts(&[(0, &[1, 2, 3, 4]), (10, &[5])]);
        let mut limits = HashMap::new();
        limits.insert((0usize, 1usize), Some(3usize));

        let (kept, trunc) = select_kept_per_command(&m, &st, &limits, 1);
        assert!(trunc);
        assert!(
            kept.contains(&1) && kept.contains(&2) && kept.contains(&3) && !kept.contains(&4),
            "the verb command keeps its own first 3"
        );
        assert!(
            kept.contains(&5),
            "the other command keeps its 1 (the default)"
        );
    }

    #[test]
    fn select_kept_caps_by_path_offset_id() {
        // A single statement behaves exactly as the old global cap did.
        let mut m = HashMap::new();
        m.insert(10i64, key("/b", 0)); // 3rd by (path, offset)
        m.insert(11, key("/a", 50)); // 2nd
        m.insert(12, key("/a", 10)); // 1st
        let st = stmts(&[(0, &[10, 11, 12])]);

        let (kept, trunc) = select_kept_per_command(&m, &st, &no_limits(), 2);
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
