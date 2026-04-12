use actix_web::{get, post, web, HttpResponse, Responder};
use askld::execution_context::ExecutionContext;
use askld::offset_range::range_bounds_to_offsets;
use askld::parser::parse;
use index::symbols::{InstanceType, SymbolInstanceId, FileId, SymbolId, SymbolType};
use log::{debug, info, warn};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tokio::time::timeout;

use super::types::{AsklData, Edge, ErrorResponse, Graph, GraphObjectEntry, HasEdge, Node, NodeSymbolInstance, QueryStatement};

const MAX_RESPONSE_BYTES: usize = 1_024 * 1_024; // 1 MB

fn is_statement_timeout(err: &pest::error::Error<askld::parser::Rule>) -> bool {
    match &err.variant {
        pest::error::ErrorVariant::CustomError { message } => {
            message.contains("statement timeout")
        }
        _ => false,
    }
}

#[post("/query")]
pub async fn query(data: web::Data<AsklData>, req_body: String) -> impl Responder {
    let _query = tracing::info_span!("query").entered();

    println!("Received query: {}", req_body);
    let ast = match parse(&req_body) {
        Ok(ast) => ast,
        Err(err) => {
            println!("Parse error: {}", err);
            let json_err = serde_json::to_string(&ErrorResponse::from_pest(&err)).unwrap();
            return HttpResponse::BadRequest().body(json_err);
        }
    };
    debug!("Global scope: {:#?}", ast);

    let mut ctx = ExecutionContext::new();

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
                warn!("Query timed out (tokio timeout after {:?})", data.query_timeout);
                if let Some(span) = &ctx.current_statement_span {
                    let err = pest::error::Error::<askld::parser::Rule>::new_from_span(
                        pest::error::ErrorVariant::CustomError {
                            message: format!("Query exceeded the {:?} time limit while executing this statement", data.query_timeout),
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
        let from_project_id = loc
            .as_ref()
            .and_then(|occurrence| object_projects.get(&occurrence.file).map(|id| id.to_string()));
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

        for n in res.nodes.0.iter().filter(|n| n.symbol_instance.symbol == symbol.id) {
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

        println!("Symbol instances for symbol {}: {:?}", symbol.id, symbol_instances);
        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            symbol_instances,
            query_stmts,
        ));
    }

    result_graph.objects = result_objects
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    result_graph.add_warnings(res.warnings);

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    if json_graph.len() > MAX_RESPONSE_BYTES {
        return HttpResponse::PayloadTooLarge().body("Response too large");
    }
    HttpResponse::Ok().body(json_graph)
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

    println!("Received request for file: {}", file_id);
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
