use actix_web::{get, post, web, HttpResponse, Responder};
use askld::execution_context::ExecutionContext;
use askld::offset_range::range_bounds_to_offsets;
use askld::parser::parse;
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

    debug!("Received query: {}", req_body);
    let ast = match parse(&req_body) {
        Ok(ast) => ast,
        Err(err) => {
            info!("Parse error: {}", err);
            let json_err = serde_json::to_string(&ErrorResponse::from_pest(&err)).unwrap();
            return HttpResponse::BadRequest().body(json_err);
        }
    };
    debug!("Global scope: {:#?}", ast);

    // Resolve the visible root layers (all projects today; a narrower,
    // per-tenant set later).  RAM-cached via the SQL result cache.
    let roots = match data.cfg.index.load_root_layers().await {
        Ok(roots) => roots,
        Err(err) => {
            warn!("Failed to resolve root layers: {}", err);
            return HttpResponse::InternalServerError().body("Failed to resolve root layers");
        }
    };
    let mut ctx = ExecutionContext::new(roots);

    let res = {
        let _query_execute = tracing::info_span!("query_execute").entered();
        let execute_future = ast.execute(&mut ctx, &data.cfg);
        match timeout(data.query_timeout, execute_future).await {
            Ok(Err(err)) => {
                let json_err = serde_json::to_string(&ErrorResponse::from_pest(&err)).unwrap();
                if is_statement_timeout(&err) {
                    warn!("Query timed out (PG statement_timeout)");
                    return HttpResponse::GatewayTimeout().body(json_err);
                }
                return HttpResponse::BadRequest().body(json_err);
            }
            Ok(Ok(res)) => res,
            Err(_) => {
                warn!(
                    "Query timed out (tokio timeout after {:?})",
                    data.query_timeout
                );
                if let Some(span) = &ctx.current_statement_span {
                    let err = pest::error::Error::<askld::parser::Rule>::new_from_span(
                        pest::error::ErrorVariant::CustomError {
                            message: format!(
                                "Query exceeded the {:?} time limit while executing this statement",
                                data.query_timeout
                            ),
                        },
                        span.as_pest_span(),
                    );
                    let json_err = serde_json::to_string(&ErrorResponse::from_pest(&err)).unwrap();
                    return HttpResponse::GatewayTimeout().body(json_err);
                }
                return HttpResponse::GatewayTimeout().body("Query timed out");
            }
        }
    };

    info!("Symbols: {:#?}", res.nodes.as_vec().len());
    info!("Edges: {:#?}", res.edges.0.len());
    info!("Has edges: {:#?}", res.has_edges.0.len());

    let _build_response = tracing::debug_span!("build_response").entered();
    let mut result_graph = Graph::new();

    let mut all_symbols = HashSet::new();
    let mut object_projects = HashMap::new();
    let mut result_objects = HashMap::new();
    for node in res.nodes.0.iter() {
        all_symbols.insert(node.symbol.clone());
        let object_id = FileId::new(node.object.id);
        object_projects
            .entry(object_id)
            .or_insert(node.object.project_id);
        result_objects.entry(object_id).or_insert(GraphObjectEntry {
            object_id: object_id.to_string(),
            path: node.object.filesystem_path.clone(),
            project_id: node.object.project_id.to_string(),
        });
    }

    for (from, to, loc) in res.edges.0 {
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
        result_graph.add_has_edge(HasEdge::new(
            has_edge.parent,
            has_edge.child,
            has_edge.parent_instance,
            has_edge.child_instance,
        ));
    }

    for symbol in all_symbols {
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

        debug!(
            "Symbol instances for symbol {}: {:?}",
            symbol.id, symbol_instances
        );
        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            symbol_instances,
            query_stmts,
        ));
    }

    result_graph.objects = result_objects.into_iter().map(|(_, value)| value).collect();
    result_graph.add_warnings(res.warnings);

    if want_markdown {
        return render_markdown_response(&data, &req_body, &result_graph, projection).await;
    }

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    if json_graph.len() > MAX_RESPONSE_BYTES {
        return HttpResponse::PayloadTooLarge().body("Response too large");
    }
    HttpResponse::Ok().body(json_graph)
}

/// Fetch the raw bytes of every file the graph references, then render the
/// graph as markdown. Raw bytes (not the lossy `String` accessor) keep byte
/// offsets aligned so `file:line` locations are exact.
async fn render_markdown_response(
    data: &web::Data<AsklData>,
    query_text: &str,
    graph: &Graph,
    projection: Projection,
) -> HttpResponse {
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

    let md = render_markdown(query_text, graph, &sources, projection);
    HttpResponse::Ok()
        .content_type("text/markdown; charset=utf-8")
        .body(md)
}

#[derive(Debug, Deserialize)]
pub struct QueryOpts {
    /// `json` (default) or `markdown`.
    format: Option<String>,
    /// `names` | `signature` | `body` (default `signature`), markdown only.
    projection: Option<String>,
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
