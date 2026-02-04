use actix_web::{get, post, web, HttpResponse, Responder};
use askld::execution_context::ExecutionContext;
use askld::offset_range::range_bounds_to_offsets;
use askld::parser::parse;
use index::db;
use index::symbols::{DeclarationId, FileId, SymbolId, SymbolType};
use log::{debug, info};
use std::collections::{HashMap, HashSet};
use tokio::time::{timeout, Duration};

use super::types::{AsklData, Edge, ErrorResponse, Graph, Node};

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

    for (from, to, loc) in res.edges.0 {
        result_graph.add_edge(Edge::new(from.symbol_id, to.symbol_id, loc));
    }

    let mut all_symbols = HashSet::new();
    for declaration in res.nodes.0.iter() {
        all_symbols.insert(declaration.symbol.clone());
    }

    let mut result_files = HashMap::new();
    for symbol in all_symbols {
        for declaration in res.nodes.0.iter() {
            if !result_files.contains_key(&FileId::new(declaration.file.id)) {
                result_files.insert(
                    FileId::new(declaration.file.id),
                    declaration.file.filesystem_path.clone(),
                );
            }
        }

        let declarations: Vec<db::Declaration> = res
            .nodes
            .0
            .iter()
            .filter(|d| d.declaration.symbol == symbol.id)
            .map(|d| db::Declaration {
                id: DeclarationId::new(d.declaration.id),
                symbol: SymbolId(d.declaration.symbol),
                file_id: FileId::new(d.file.id),
                symbol_type: SymbolType::from(d.declaration.symbol_type),
                offset_range: range_bounds_to_offsets(&d.declaration.offset_range).unwrap(),
            })
            .collect();

        println!("Declarations for symbol {}: {:?}", symbol.id, declarations);
        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            declarations,
        ));
    }

    result_graph.files = result_files.into_iter().collect();
    result_graph.add_warnings(res.warnings);

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    HttpResponse::Ok().body(json_graph)
}

#[get["/source/{file_id}"]]
pub async fn file(data: web::Data<AsklData>, file_id: web::Path<FileId>) -> impl Responder {
    let _source = tracing::info_span!("source").entered();

    let file_id = *file_id;

    println!("Received request for file: {}", file_id);
    if let Ok(source) = data.cfg.index.get_file_contents(file_id).await {
        HttpResponse::Ok().body(source)
    } else {
        HttpResponse::NotFound().body("File not found")
    }
}
