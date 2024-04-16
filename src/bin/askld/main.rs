use std::{
    collections::{HashMap, HashSet},
    path::{Path, PathBuf},
};

use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use anyhow::{anyhow, Result};
use askl::symbols::{Occurence, Symbols};
use askl::{
    cfg::ControlFlowGraph,
    parser::parse,
    symbols::{Symbol, SymbolId, SymbolMap},
};
use clap::Parser;
use log::{debug, info};
use protobuf::Message;
use scip::types::Index;
use serde::{Deserialize, Serialize};
use url::Url;

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
    sources: Vec<SymbolId>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Node {
    id: SymbolId,
    label: String,
    uri: Url,
    loc: String,
}

impl Node {
    fn new(id: SymbolId, label: String, uri: Url, loc: String) -> Self {
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

    let (res_symbols, res_edges) =  if let Some(res) = ast.matched_symbols(&data.cfg, &data.sources) {
        res
    } else {
        return HttpResponse::BadRequest().body("Unmatched nodes or edges");
    };

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
        let filename = sym.ranges[0].file.clone();
        let line = sym.ranges[0].line_start;
        let url = Url::from_file_path(filename).unwrap();
        result_graph.add_node(Node::new(loc, sym.name.clone(), url, format!("{}", line)));
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

fn read_data(args: &Args) -> Result<AsklData> {
    match args.format.as_str() {
        "askl" => {
            let symbols: SymbolMap = serde_json::from_slice(&std::fs::read(&args.index)?)?;
            let sources: Vec<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();
            let cfg = ControlFlowGraph::from_symbols(symbols);
            Ok(AsklData {
                cfg: cfg,
                sources: sources,
            })
        }
        "scip" => {
            info!("Index format: SCIP");

            let bytes = std::fs::read(&args.index)?;
            let index = Index::parse_from_bytes(&bytes).unwrap();

            debug!(
                "Index: documents {} external symbols {}",
                index.documents.len(),
                index.external_symbols.len()
            );
            debug!("Index: metadata {:#?}", index.metadata);

            debug!("Index: documents {:#?}", index.documents);

            let mut occurence_map: HashMap<&String, Vec<Occurence>> = HashMap::new();
            index.documents.iter().for_each(|doc| {
                doc.occurrences.iter().for_each(|occ| {
                    let range = if occ.range.len() == 4 {
                        Occurence {
                            file: PathBuf::from(doc.relative_path.clone()),
                            line_start: occ.range[0],
                            column_start: occ.range[1],
                            line_end: occ.range[2],
                            column_end: occ.range[3],
                        }
                    } else {
                        Occurence {
                            file: PathBuf::from(doc.relative_path.clone()),
                            line_start: occ.range[0],
                            column_start: occ.range[1],
                            line_end: occ.range[0],
                            column_end: occ.range[2],
                        }
                    };
                    occurence_map
                        .entry(&occ.symbol)
                        .and_modify(|ranges: &mut Vec<Occurence>| ranges.push(range.clone()))
                        .or_insert_with(|| vec![range]);
                });
            });
            let mut symbols = SymbolMap::new();
            index.documents.iter().for_each(|doc| {
                doc.symbols.iter().for_each(|si| {
                    let id = SymbolId::new(si.symbol.clone());

                    let range = occurence_map.get(&si.symbol).unwrap();
                    let symbol = Symbol {
                        name: si.symbol.clone(),
                        ranges: range.clone(),
                        children: Default::default(),
                    };
                    symbols.add(id, symbol);
                });
            });

            Err(anyhow!("Unimplemented index format: {}", args.format))
        }
        _ => Err(anyhow!("Unsupported index format: {}", args.format)),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let askl_data: AsklData = read_data(&args).expect("Failed to read data");
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
