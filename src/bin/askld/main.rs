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
    id: String,
    from: SymbolId,
    to: SymbolId,
    from_file: Option<String>,
    from_line: Option<i32>,
}

impl Edge {
    fn new(from: SymbolId, to: SymbolId, occurence: Option<Occurence>) -> Self {
        let (filename, line) = if let Some(occ) = occurence {
            (
                Some(format!(
                    "file://{}",
                    occ.file.into_os_string().into_string().unwrap()
                )),
                Some(occ.line_start),
            )
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

    let (res_nodes, res_edges) = ast.execute_all(&data.cfg, data.sources.clone());

    info!("Symbols: {:#?}", res_nodes.0.len());
    info!("Edges: {:#?}", res_edges.0.len());

    let mut result_graph = Graph::new();

    let mut all_symbols = HashSet::new();
    for (from, to, loc) in res_edges.0 {
        all_symbols.insert(from.clone());
        all_symbols.insert(to.clone());
        result_graph.add_edge(Edge::new(from, to, loc));
    }

    for s in res_nodes.0 {
        all_symbols.insert(s.clone());
    }

    for loc in all_symbols {
        let sym = data.cfg.get_symbol(&loc).unwrap();
        let filename = sym.ranges[0].file.clone();
        let line = sym.ranges[0].line_start;
        debug!("filename {}", filename.display());
        let url = Url::from_file_path(filename).unwrap();
        result_graph.add_node(Node::new(loc, sym.name.clone(), url, format!("{}", line)));
    }

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    HttpResponse::Ok().body(json_graph)
}

#[get["/source/{path:.*}"]]
async fn file(_data: web::Data<AsklData>, path: web::Path<String>) -> impl Responder {
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

#[cfg(test)]
mod tests {
    use askl::cfg::{EdgeList, NodeList};

    use super::*;

    const INPUT_A: &str = r#"
    {
        "map": {
            "b": {
            "name": "b",
            "ranges": [
                {
                "line_start": 1,
                "line_end": 3,
                "column_start": 1,
                "column_end": 1,
                "file": "main.c"
                }
            ],
            "children": []
            },
            "a": {
            "name": "a",
            "ranges": [
                {
                "line_start": 5,
                "line_end": 7,
                "column_start": 1,
                "column_end": 1,
                "file": "main.c"
                }
            ],
            "children": [
                {
                "id": "b",
                "occurence": {
                    "line_start": 7,
                    "line_end": 7,
                    "column_start": 16,
                    "column_end": 16,
                    "file": "main.c"
                }
                },
                {
                "id": "b",
                "occurence": {
                    "line_start": 7,
                    "line_end": 7,
                    "column_start": 22,
                    "column_end": 22,
                    "file": "main.c"
                }
                }
            ]
            },
            "main": {
            "name": "main",
            "ranges": [
                {
                "line_start": 9,
                "line_end": 11,
                "column_start": 1,
                "column_end": 1,
                "file": "main.c"
                }
            ],
            "children": [
                {
                "id": "a",
                "occurence": {
                    "line_start": 11,
                    "line_end": 11,
                    "column_start": 16,
                    "column_end": 16,
                    "file": "main.c"
                }
                },
                {
                "id": "b",
                "occurence": {
                    "line_start": 11,
                    "line_end": 11,
                    "column_start": 22,
                    "column_end": 22,
                    "file": "main.c"
                }
                }
            ]
            },
            "c": {
                "name": "c",
                "ranges": [
                    {
                    "line_start": 13,
                    "line_end": 14,
                    "column_start": 1,
                    "column_end": 1,
                    "file": "main.c"
                    }
                ],
                "children": []
            }   
        }
    }
    "#;

    fn format_edges(edges: EdgeList) -> Vec<String> {
        edges
            .0
            .into_iter()
            .map(|(f, t, _)| format!("{}-{}", f, t))
            .collect()
    }

    #[test]
    fn parse_askl() {
        let _symbols: SymbolMap = serde_json::from_slice(INPUT_A.as_bytes()).unwrap();
    }

    #[test]
    fn parse_query() {
        const QUERY: &str = r#""a""#;
        let ast = parse(QUERY).unwrap();

        let statements: Vec<_> = ast.scope().statements().collect();
        assert_eq!(statements.len(), 1);
        let statement = &statements[0];

        let _verb = statement.verb();
        let scope = statement.scope();

        let statements: Vec<_> = scope.statements().collect();
        assert_eq!(statements.len(), 0);

        println!("{:?}", ast);
        assert_eq!(
            format!("{:?}", ast),
            r#"GlobalStatement { verb: CompoundVerb { verbs: [UnitVerb] }, scope: DefaultScope([DefaultStatement { verb: CompoundVerb { verbs: [ChildrenVerb, SelectVerb { name: "a" }] }, scope: EmptyScope }]) }"#
        );
    }

    #[test]
    fn parse_parent_query() {
        const QUERY: &str = r#"{"a"}"#;
        let ast = parse(QUERY).unwrap();
        println!("{:?}", ast);
        assert_eq!(
            format!("{:?}", ast),
            r#"GlobalStatement { verb: CompoundVerb { verbs: [UnitVerb] }, scope: DefaultScope([DefaultStatement { verb: CompoundVerb { verbs: [ChildrenVerb] }, scope: DefaultScope([DefaultStatement { verb: CompoundVerb { verbs: [ChildrenVerb, SelectVerb { name: "a" }] }, scope: EmptyScope }]) }]) }"#
        );
    }

    #[test]
    fn parse_child_query() {
        const QUERY: &str = r#""a"{}"#;
        let ast = parse(QUERY).unwrap();
        println!("{:?}", ast);
        assert_eq!(
            format!("{:?}", ast),
            r#"GlobalStatement { verb: CompoundVerb { verbs: [UnitVerb] }, scope: DefaultScope([DefaultStatement { verb: CompoundVerb { verbs: [ChildrenVerb, SelectVerb { name: "a" }] }, scope: DefaultScope([DefaultStatement { verb: CompoundVerb { verbs: [ChildrenVerb] }, scope: EmptyScope }]) }]) }"#
        );
    }

    fn run_query(askl_input: &str, askl_query: &str) -> (NodeList, EdgeList) {
        let symbols: SymbolMap = serde_json::from_slice(askl_input.as_bytes()).unwrap();
        let sources: Vec<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();
        let cfg = ControlFlowGraph::from_symbols(symbols);

        let ast = parse(askl_query).unwrap();
        println!("{:#?}", ast);

        ast.execute_all(&cfg, sources)
    }

    #[test]
    fn single_node_query() {
        env_logger::init();

        const QUERY: &str = r#""a""#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);
        assert_eq!(res_nodes.0, vec![SymbolId::new("a".to_string())]);
        assert_eq!(res_edges.0.len(), 0);
    }

    #[test]
    fn single_child_query() {
        const QUERY: &str = r#""a"{}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);
        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string())
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b"]);
    }

    #[test]
    fn single_parent_query() {
        const QUERY: &str = r#"{"a"}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);
        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("main".to_string())
            ]
        );
        assert_eq!(res_edges.0.len(), 1);
    }

    #[test]
    fn double_parent_query() {
        const QUERY: &str = r#"{{"b"}}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);
        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string()),
                SymbolId::new("main".to_string())
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b", "main-a", "main-b"]);
    }

    #[test]
    fn missing_child_query() {
        const QUERY: &str = r#""a"{{}}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);
        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string())
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b"]);
    }

    #[test]
    fn forced_query() {
        const QUERY: &str = r#"!"a""#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(res_nodes.0, vec![]);
        assert_eq!(res_edges.0.len(), 0);
    }

    #[test]
    fn forced_child_query_1() {
        const QUERY: &str = r#""b"{!"a"}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string())
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b", "b-a"]);
    }

    #[test]
    fn forced_child_query_2() {
        const QUERY: &str = r#""b"{!"c"}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("b".to_string()),
                SymbolId::new("c".to_string())
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["b-c"]);
    }

    #[test]
    fn forced_child_query_3() {
        const QUERY: &str = r#""main" {
            !"c"
        }"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("c".to_string()),
                SymbolId::new("main".to_string())
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["main-c"]);
    }

    #[test]
    fn two_selectors() {
        const QUERY: &str = r#""b" "a""#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string()),
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b"]);
    }

    #[test]
    fn two_selectors_children() {
        const QUERY: &str = r#""b" "a" {}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string()),
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b"]);
    }

    #[test]
    fn statement_after_scope() {
        const QUERY: &str = r#""a" {}; "a""#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string()),
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b"]);
    }

    #[test]
    fn statement_after_scope_newline() {
        const QUERY: &str = r#""a" {}
        "a""#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
                SymbolId::new("b".to_string()),
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, vec!["a-b", "a-b"]);
    }

    #[test]
    fn ignore_node() {
        const QUERY: &str = r#""a" {@ignore("b")}"#;
        let (res_nodes, res_edges) = run_query(INPUT_A, QUERY);

        println!("{:#?}", res_nodes);
        println!("{:#?}", res_edges);

        assert_eq!(
            res_nodes.0,
            vec![
                SymbolId::new("a".to_string()),
            ]
        );
        let edges = format_edges(res_edges);
        assert_eq!(edges, Vec::<String>::new());
    }
}
