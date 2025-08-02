use std::collections::{HashMap, HashSet};

use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use anyhow::{anyhow, Result};
use askld::execution_context::ExecutionContext;
use askld::{cfg::ControlFlowGraph, parser::parse};
use clap::Parser;
use index::db::{Declaration, Index};
use index::symbols::{FileId, Occurrence};
use index::symbols::{SymbolId, SymbolMap};
use log::{debug, info};
use serde::{Deserialize, Serialize, Serializer};

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Path to the index file
    #[clap(short, long)]
    index: String,

    // Format of the index file
    #[clap(short, long, default_value = "askl")]
    format: String,
}

struct AsklData {
    cfg: ControlFlowGraph,
}

fn symbolid_as_string<S>(x: &SymbolId, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

#[derive(Debug, Serialize, Deserialize)]
struct Node {
    #[serde(serialize_with = "symbolid_as_string")]
    id: SymbolId,
    label: String,
    declarations: Vec<Declaration>,
}

impl Node {
    fn new(id: SymbolId, label: String, declarations: Vec<Declaration>) -> Self {
        Self {
            id,
            label,
            declarations,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Edge {
    id: String,
    #[serde(serialize_with = "symbolid_as_string")]
    from: SymbolId,
    #[serde(serialize_with = "symbolid_as_string")]
    to: SymbolId,
    from_file: Option<FileId>,
    from_line: Option<i32>,
}

impl Edge {
    fn new(from: SymbolId, to: SymbolId, occurence: Option<Occurrence>) -> Self {
        let (filename, line) = if let Some(occ) = occurence {
            (Some(occ.file), Some(occ.line_start))
        } else {
            (None, None)
        };
        Self {
            id: format!("{}-{}", from, to),
            from: from,
            to: to,
            from_file: filename,
            from_line: line,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    files: Vec<(FileId, String)>,
}

impl Graph {
    fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            files: vec![],
        }
    }

    fn add_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }
}

#[post("/query")]
async fn query(data: web::Data<AsklData>, req_body: String) -> impl Responder {
    println!("Received query: {}", req_body);
    let ast = if let Ok(ast) = parse(&req_body) {
        ast
    } else {
        return HttpResponse::BadRequest().body("Invalid query");
    };
    debug!("Global scope: {:#?}", ast);

    let mut ctx = ExecutionContext::new();
    let res = ast
        .execute(&mut ctx, &data.cfg, None, &HashSet::new())
        .await;
    if res.is_none() {
        return HttpResponse::NotFound().body("Did not resolve any symbols");
    }
    let (_, res_nodes, res_edges) = res.unwrap();

    info!("Symbols: {:#?}", res_nodes.as_vec().len());
    info!("Edges: {:#?}", res_edges.0.len());

    let mut result_graph = Graph::new();

    let mut all_symbols = HashSet::new();
    for (from, to, loc) in res_edges.0 {
        let from_declaration = data.cfg.symbols.declarations.get(&from).unwrap();
        let to_declaration = data.cfg.symbols.declarations.get(&to).unwrap();
        all_symbols.insert(from_declaration.symbol);
        all_symbols.insert(to_declaration.symbol);
        result_graph.add_edge(Edge::new(
            from_declaration.symbol,
            to_declaration.symbol,
            loc,
        ));
    }

    for declaration_id in res_nodes.as_vec() {
        let declaration = data.cfg.symbols.declarations.get(&declaration_id).unwrap();
        all_symbols.insert(declaration.symbol);
    }

    let mut result_files = HashMap::new();
    for symbol_id in all_symbols {
        let sym = data.cfg.get_symbol(symbol_id).unwrap();
        let declarations =
            if let Ok(declarations) = data.cfg.index.symbol_declarations(symbol_id).await {
                declarations
            } else {
                return HttpResponse::BadRequest().body("SymbolId not found");
            };

        for declaration in declarations.iter() {
            if !result_files.contains_key(&declaration.file_id) {
                let f = data.cfg.index.get_file(declaration.file_id).await.unwrap();
                result_files.insert(declaration.file_id, f.filesystem_path);
            }
        }
        result_graph.add_node(Node::new(symbol_id, sym.name.clone(), declarations));
    }

    result_graph.files = result_files.into_iter().collect();

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    HttpResponse::Ok().body(json_graph)
}

#[get["/source/{file_id}"]]
async fn file(data: web::Data<AsklData>, file_id: web::Path<FileId>) -> impl Responder {
    let path = if let Some(file) = data.cfg.symbols.files.get(&file_id) {
        &file.filesystem_path
    } else {
        return HttpResponse::NotFound().body("File not found");
    };

    if let Ok(source) = std::fs::read_to_string(path) {
        HttpResponse::Ok().body(source)
    } else {
        HttpResponse::NotFound().body("File not found")
    }
}

async fn read_data(args: &Args) -> Result<AsklData> {
    match args.format.as_str() {
        "sqlite" => {
            let index = Index::connect(&args.index).await?;
            let symbols = SymbolMap::from_index(&index).await?;
            let cfg = ControlFlowGraph::from_symbols(symbols, index);
            Ok(AsklData { cfg: cfg })
        }
        _ => Err(anyhow!("Unsupported index format: {}", args.format)),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let askl_data: AsklData = read_data(&args).await.expect("Failed to read data");
    let askl_data = web::Data::new(askl_data);
    info!("Starting server...");

    HttpServer::new(move || {
        App::new()
            .app_data(askl_data.clone())
            .service(query)
            .service(file)
    })
    .bind(("127.0.0.1", 8080))?
    .run()
    .await
}
