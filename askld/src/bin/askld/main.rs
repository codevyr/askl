use std::collections::{HashMap, HashSet};

use actix_web::{get, post, web, App, HttpResponse, HttpServer, Responder};
use anyhow::{anyhow, Result};
use askld::execution_context::ExecutionContext;
use askld::parser::Rule;
use askld::{cfg::ControlFlowGraph, parser::parse};
use clap::Parser;
use index::db::{self};
use index::symbols::SymbolId;
use index::symbols::{DeclarationId, FileId, Occurrence, SymbolType};
use log::{debug, info};
use serde::{Deserialize, Serialize, Serializer};
use tokio::time::{timeout, Duration};
use tracing_chrome::ChromeLayerBuilder;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

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

    /// Port to listen on
    #[clap(short, long, default_value = "80")]
    port: u16,

    /// Host to bind to
    #[clap(short, long, default_value = "127.0.0.1")]
    host: String,

    /// Enable tracing. Provide a file path to write the trace to.
    #[clap(short, long, action)]
    trace: Option<String>,
}

struct AsklData {
    cfg: ControlFlowGraph,
}

const QUERY_TIMEOUT: Duration = Duration::from_secs(1);

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
    declarations: Vec<db::Declaration>,
}

impl Node {
    fn new(id: SymbolId, label: String, declarations: Vec<db::Declaration>) -> Self {
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
    warnings: Vec<ErrorResponse>,
}

impl Graph {
    fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            files: vec![],
            warnings: vec![],
        }
    }

    fn add_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    fn add_warnings(&mut self, warnings: Vec<pest::error::Error<Rule>>) {
        for warning in warnings {
            let error_response = ErrorResponse {
                message: warning.to_string(),
                location: warning.location.clone().into(),
                line_col: warning.line_col.clone().into(),
                path: warning.path().map(|p| p.to_string()),
                line: warning.line().to_string(),
            };
            self.warnings.push(error_response);
        }
    }
}

/// Where an `Error` has occurred.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum InputLocation {
    /// `Error` was created by `Error::new_from_pos`
    Pos(usize),
    /// `Error` was created by `Error::new_from_span`
    Span((usize, usize)),
}

impl From<pest::error::InputLocation> for InputLocation {
    fn from(loc: pest::error::InputLocation) -> Self {
        match loc {
            pest::error::InputLocation::Pos(pos) => InputLocation::Pos(pos),
            pest::error::InputLocation::Span(span) => InputLocation::Span(span),
        }
    }
}

/// Line/column where an `Error` has occurred.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum LineColLocation {
    /// Line/column pair if `Error` was created by `Error::new_from_pos`
    Pos((usize, usize)),
    /// Line/column pairs if `Error` was created by `Error::new_from_span`
    Span((usize, usize), (usize, usize)),
}

impl From<pest::error::LineColLocation> for LineColLocation {
    fn from(loc: pest::error::LineColLocation) -> Self {
        match loc {
            pest::error::LineColLocation::Pos(pos) => LineColLocation::Pos(pos),
            pest::error::LineColLocation::Span(start, end) => LineColLocation::Span(start, end),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ErrorResponse {
    message: String,
    location: InputLocation,
    line_col: LineColLocation,
    path: Option<String>,
    line: String,
}

#[post("/query")]
async fn query(data: web::Data<AsklData>, req_body: String) -> impl Responder {
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
                line_start: d.declaration.line_start as i64,
                line_end: d.declaration.line_end as i64,
                col_start: d.declaration.col_start as i64,
                col_end: d.declaration.col_end as i64,
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
async fn file(data: web::Data<AsklData>, file_id: web::Path<FileId>) -> impl Responder {
    let _source = tracing::info_span!("source").entered();

    let file_id = *file_id;

    println!("Received request for file: {}", file_id);
    if let Ok(source) = data.cfg.index.get_file_contents(file_id).await {
        HttpResponse::Ok().body(source)
    } else {
        HttpResponse::NotFound().body("File not found")
    }
}

async fn read_data(args: &Args) -> Result<AsklData> {
    match args.format.as_str() {
        "sqlite" => {
            let index_diesel = index::db_diesel::Index::connect(&args.index).await?;
            let cfg = ControlFlowGraph::from_symbols(index_diesel);
            Ok(AsklData { cfg: cfg })
        }
        _ => Err(anyhow!("Unsupported index format: {}", args.format)),
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let _guard = if let Some(trace_dir) = &args.trace {
        use chrono::prelude::*;
        let trace_file = format!("trace-{}.json", Local::now().format("%Y%m%d-%H%M%S"),);
        let trace_path = std::path::Path::new(trace_dir).join(trace_file);
        if trace_path.exists() {
            std::fs::remove_file(&trace_path).expect("Failed to remove old trace file");
        }
        let (chrome_layer, _guard) = ChromeLayerBuilder::new()
            .file(trace_path)
            .include_args(true)
            .trace_style(tracing_chrome::TraceStyle::Async)
            .build();
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(chrome_layer)
            .init();

        info!("Tracing enabled, writing to {}", trace_dir);
        Some(_guard)
    } else {
        env_logger::init();

        None
    };

    let askl_data: AsklData = read_data(&args).await.expect("Failed to read data");
    let askl_data = web::Data::new(askl_data);

    info!("Starting server on {}:{}...", args.host, args.port);

    HttpServer::new(move || {
        App::new()
            .wrap(tracing_actix_web::TracingLogger::default())
            .app_data(askl_data.clone())
            .service(query)
            .service(file)
    })
    .bind((args.host, args.port))?
    .run()
    .await
}
