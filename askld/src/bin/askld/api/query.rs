use actix_web::{get, post, web, HttpResponse, Responder};
use askld::execution_context::ExecutionContext;
use askld::offset_range::range_bounds_to_offsets;
use askld::parser::parse;
use index::symbols::{DeclarationId, FileId, SymbolId, SymbolType};
use log::{debug, info};
use serde::Deserialize;
use std::collections::{HashMap, HashSet};
use tokio::time::{timeout, Duration};

use super::types::{AsklData, Edge, ErrorResponse, Graph, GraphFileEntry, Node, NodeDeclaration};

const QUERY_TIMEOUT: Duration = Duration::from_secs(1);

#[post("/query")]
pub async fn query(data: web::Data<AsklData>, req_body: String) -> impl Responder {
    let _query = tracing::info_span!("query").entered();

    println!("Received query: {}", req_body);
    let ast = match parse(&req_body) {
        Ok(ast) => ast,
        Err(err) => {
            println!("Parse error: {}", err);
            let json_err = serde_json::to_string(&ErrorResponse {
                message: err.to_string(),
                location: err.location.clone().into(),
                line_col: err.line_col.clone().into(),
                path: err.path().map(|p| p.to_string()),
                line: err.line().to_string(),
            })
            .unwrap();
            return HttpResponse::BadRequest().body(json_err);
        }
    };
    debug!("Global scope: {:#?}", ast);

    let mut ctx = ExecutionContext::new();

    let res = {
        let _query_execute = tracing::info_span!("query_execute").entered();
        let execute_future = ast.execute(&mut ctx, &data.cfg);
        match timeout(QUERY_TIMEOUT, execute_future).await {
            Ok(Err(err)) => {
                let json_err = serde_json::to_string(&ErrorResponse {
                    message: err.to_string(),
                    location: err.location.clone().into(),
                    line_col: err.line_col.clone().into(),
                    path: err.path().map(|p| p.to_string()),
                    line: err.line().to_string(),
                });
                return HttpResponse::BadRequest().body(json_err.unwrap());
            }
            Ok(Ok(res)) => res,
            Err(_) => {
                return HttpResponse::RequestTimeout().body("Query timed out");
            }
        }
    };

    info!("Symbols: {:#?}", res.nodes.as_vec().len());
    info!("Edges: {:#?}", res.edges.0.len());

    let mut result_graph = Graph::new();

    let mut all_symbols = HashSet::new();
    let mut file_projects = HashMap::new();
    let mut result_files = HashMap::new();
    for declaration in res.nodes.0.iter() {
        all_symbols.insert(declaration.symbol.clone());
        let file_id = FileId::new(declaration.file.id);
        file_projects
            .entry(file_id)
            .or_insert(declaration.file.project_id);
        result_files.entry(file_id).or_insert(GraphFileEntry {
            file_id: file_id.to_string(),
            path: declaration.file.filesystem_path.clone(),
            project_id: declaration.file.project_id.to_string(),
        });
    }

    for (from, to, loc) in res.edges.0 {
        let from_project_id = loc
            .as_ref()
            .and_then(|occurrence| file_projects.get(&occurrence.file).map(|id| id.to_string()));
        result_graph.add_edge(Edge::new(
            from.symbol_id,
            to.symbol_id,
            loc,
            from_project_id,
        ));
    }

    for symbol in all_symbols {
        let declarations: Vec<NodeDeclaration> = res
            .nodes
            .0
            .iter()
            .filter(|d| d.declaration.symbol == symbol.id)
            .map(|d| {
                let (start_offset, end_offset) =
                    range_bounds_to_offsets(&d.declaration.offset_range).unwrap();
                NodeDeclaration {
                    id: DeclarationId::new(d.declaration.id).to_string(),
                    symbol: SymbolId(d.declaration.symbol).to_string(),
                    file_id: FileId::new(d.file.id).to_string(),
                    project_id: d.file.project_id.to_string(),
                    symbol_type: SymbolType::from(d.declaration.symbol_type),
                    start_offset,
                    end_offset,
                }
            })
            .collect();

        println!("Declarations for symbol {}: {:?}", symbol.id, declarations);
        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            declarations,
        ));
    }

    result_graph.files = result_files
        .into_iter()
        .map(|(_, value)| value)
        .collect();
    result_graph.add_warnings(res.warnings);

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
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
