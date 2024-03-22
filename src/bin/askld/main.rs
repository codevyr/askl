use actix_web::{post, web, App, HttpResponse, HttpServer, Responder};
use askl::{
    cfg::ControlFlowGraph,
    parser::parse,
    symbols::{SymbolId, SymbolMap},
};
use clap::Parser;
use log::{debug, info};
use petgraph::graphmap::DiGraphMap;

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

    let mut result_graph: DiGraphMap<&str, ()> = DiGraphMap::new();

    for (from, to) in res_edges.0 {
        let sym_from = data.cfg.get_symbol(&from).unwrap();
        let sym_to = data.cfg.get_symbol(&to).unwrap();

        result_graph.add_edge(&sym_from.name, &sym_to.name, ());
    }

    for loc in res_symbols {
        let sym = data.cfg.get_symbol(&loc).unwrap();
        result_graph.add_node(&sym.name);
    }

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    debug!("Result graph: {}", json_graph);
    HttpResponse::Ok().body(json_graph)
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

    HttpServer::new(move || App::new().app_data(askl_data.clone()).service(query))
        .bind(("127.0.0.1", 8080))?
        .run()
        .await
}
