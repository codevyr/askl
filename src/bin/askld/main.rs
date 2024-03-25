use std::{collections::HashSet, path::Path};

use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use askl::{
    cfg::ControlFlowGraph,
    parser::parse,
    symbols::{SymbolId, SymbolMap},
};
use clap::Parser;
use log::{debug, info};
use serde::{Deserialize, Serialize};

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Path to the index file
    #[clap(short, long)]
    index: String,
}

struct AsklData {
    cfg: ControlFlowGraph,
    sources: Vec<SymbolId>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Node {
    id: SymbolId,
    label: String,
    uri: String,
    loc: String,
}

impl Node {
    fn new(id: SymbolId, label: String, uri: String, loc: String) -> Self {
        Self {
            id: id,
            label: label,
            uri: uri,
            loc: loc,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Edge {
    from: SymbolId,
    to: SymbolId,
    from_loc: String,
}

impl Edge {
    fn new(from: SymbolId, to: SymbolId) -> Self {
        Self {
            from: from,
            to: to,
            from_loc: "".to_string(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
}

impl Graph {
    fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
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

    let (res_symbols, res_edges) = ast.matched_symbols(&data.cfg, &data.sources, None).unwrap();

    info!("Symbols: {:#?}", res_symbols.len());
    info!("Edges: {:#?}", res_edges.0.len());

    let mut result_graph = Graph::new();

    let mut all_symbols = HashSet::new();
    for (from, to) in res_edges.0 {
        all_symbols.insert(from.clone());
        all_symbols.insert(to.clone());
        result_graph.add_edge(Edge::new(from, to));
    }

    for loc in res_symbols {
        all_symbols.insert(loc);
    }

    for loc in all_symbols {
        let sym = data.cfg.get_symbol(&loc).unwrap();
        let filename = sym.ranges[0]
            .begin
            .spelling_loc
            .as_ref()
            .unwrap()
            .file
            .clone();
        let line = sym.ranges[0].begin.spelling_loc.as_ref().unwrap().line;
        let uri = format!("file://{}", filename);
        result_graph.add_node(Node::new(loc, sym.name.clone(), uri, format!("{}", line)));
    }

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    HttpResponse::Ok().body(json_graph)
}

#[get["/source/{path:.*}"]]
async fn file(data: web::Data<AsklData>, path: web::Path<String>) -> impl Responder {
    let path = Path::new("/").join(Path::new(path.as_str()));
    debug!("Received request for file: {:#?}", path);
    debug!("XXX: This function is unsafe, because it can read any file on the system");
    if let Ok(source) = std::fs::read_to_string(&path) {
        HttpResponse::Ok().body(source)
    } else {
        HttpResponse::NotFound().body("File not found")
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let symbols: SymbolMap = serde_json::from_slice(&std::fs::read(args.index)?)?;

    let sources: Vec<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();
    let cfg = ControlFlowGraph::from_symbols(symbols);

    let askl_data = web::Data::new(AsklData {
        cfg: cfg,
        sources: sources,
    });

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
