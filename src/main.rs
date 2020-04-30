use std::fmt;
use std::str;
use std::io;

use log;
use log::{info};
use stderrlog;

use structopt;
use structopt::StructOpt;

use lsp_types::{DocumentSymbolResponse, SymbolKind, TextDocumentItem, Range, DocumentSymbol};

mod language_server;
use language_server::{LanguageServerLauncher, LanguageServer};

mod search;
use search::{SearchLauncher, Search, Match};

use std::collections::HashMap;

use petgraph::graph::DiGraph;
use petgraph::dot::{Dot, Config};

use actix_web::{web, App, HttpResponse, HttpServer, Responder};

mod schema;

use std::sync::Arc;
use juniper::http::GraphQLRequest;
use juniper::http::graphiql::graphiql_source;

#[derive(Debug)]
struct LspError(&'static str);

fn parse_list(src: &str) -> Vec<String> {
    src.split(',').map(str::to_string).collect()
}

#[derive(StructOpt, Debug)]
#[structopt()]
struct Opt {
    /// Silence all output
    #[structopt(short = "q", long = "quiet")]
    quiet: bool,
    /// Verbose mode (-v, -vv, -vvv, etc)
    #[structopt(short = "v", long = "verbose", parse(from_occurrences))]
    verbose: usize,
    /// Timestamp (sec, ms, ns, none)
    #[structopt(short = "t", long = "timestamp")]
    ts: Option<stderrlog::Timestamp>,
    /// Project root directory
    #[structopt(short = "p", long = "project-root")]
    project_root: String,
    /// List of project languages
    #[structopt(short = "l", long = "languages", default_value = "cc,cpp")]
    languages: String,
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "LSP error: {}", self.0)
    }
}

impl std::error::Error for LspError {}

type Error = Box<dyn std::error::Error>;

#[derive(Debug, Clone)]
struct AskerSymbol {
    name: String,
    range: Range,
    kind: SymbolKind,
    parent: Option<usize>,
}

struct AskerDocument {
    symbols: Vec<AskerSymbol>,
    lsp_item: TextDocumentItem,
}

impl AskerDocument {
    fn new(document: TextDocumentItem) -> Self {
        AskerDocument {
            lsp_item: document,
            symbols: Vec::new(),
        }
    }

    fn append_symbol(&mut self, symbol: &DocumentSymbol, parent: Option<usize>) -> Result<(), Error> {
        self.symbols.push(AskerSymbol{
            parent: parent,
            kind: symbol.kind.clone(),
            name: symbol.name.clone(),
            range: symbol.range.clone(),
        });

        let current_id = self.symbols.len() - 1;
        if let Some(children) = &symbol.children {
            for child in children {
                self.append_symbol(&child, Some(current_id))?;
            }
        }

        Ok(())
    }
}

/// Structure that maintains metadata for the commands to run
struct Asker {
    documents: HashMap<String, AskerDocument>,
    lang_server: Box<dyn LanguageServer>,
    searcher: Box<dyn Search>,
}

impl Asker {
    pub fn new(opt: &Opt) -> Result<Asker, Error> {
        let language_list = parse_list(&opt.languages);

        let searcher = SearchLauncher::new()
            .engine("ack")
            .directory(&opt.project_root)
            .languages(&language_list)
            .launch()?;

        let mut lang_server = LanguageServerLauncher::new()
            .server("/usr/bin/clangd-9".to_owned())
            .project(opt.project_root.to_owned())
            .languages(language_list)
            .launch()
            .expect("Failed to spawn clangd");

        lang_server.initialize()?;
        lang_server.initialized()?;

        Ok(Asker {
            lang_server: lang_server,
            searcher: searcher,
            documents: HashMap::new(),
        })
    }

    fn update_symbols(&mut self, document: &mut AskerDocument) -> Result<(), Error> {
        let symbols = self.lang_server.document_symbol(&document.lsp_item)?;
        match symbols {
            Some(DocumentSymbolResponse::Flat(_)) => {
                Err(Box::new(LspError("Flat symbols are unsupported")))
            },
            Some(DocumentSymbolResponse::Nested(v)) => {
                for symbol in v.iter() {
                    document.append_symbol(symbol, None)?;
                }
                Ok(())
            },
            None => {
                Err(Box::new(LspError("No symbols found")))
            }
        }
    }

    fn update_documents(&mut self, matches: &Vec<Match>) -> Result<(), Error> {
        for m in matches {
            if let Some(_) = self.documents.get(&m.filename) {
                continue
            }

            let mut document = AskerDocument::new(self.lang_server.document_open(m.filename.as_str())?);

            self.update_symbols(&mut document)?;
            self.documents.insert(m.filename.clone(), document);
        }

        Ok(())
    }

    pub fn search(&mut self, pattern_string: &str) -> Result<Vec<Match>, Error> {
        let matches = self.searcher.search(pattern_string.to_owned())?;

        self.update_documents(&matches)?;

        Ok(matches)
    }

    pub fn find_symbols(&mut self, matches: &Vec<Match>) -> Vec<AskerSymbol> {
        matches
            .iter()
            .map(|search_match| {
                let document = self.documents.get(&search_match.filename).unwrap();
                let symbol = document.symbols
                    .iter()
                    .rev()
                    .skip_while(|s| s.range.start.line > search_match.line_number)
                    .nth(0);
                info!("Symbol: {:#?} Search: {:#?}", symbol, search_match);
                if let Some(symbol) = symbol {
                    if symbol.range.start.line == search_match.line_number {
                        return Some(symbol.clone());
                    }
                }
                None
            })
            .filter_map(|s| s)
            .collect()
    }

    pub fn find_parent(&mut self, search_match: Match) -> Option<AskerSymbol> {
        let document = self.documents.get(&search_match.filename).unwrap();

        let symbol = document.symbols.iter().rev().skip_while(|s| s.range.start.line > search_match.line_number).nth(0);

        match symbol {
            Some(symbol) => {
                if symbol.range.start.line == search_match.line_number {
                    // Found oneself
                    None
                } else {
                    Some(symbol.clone())
                }
            },
            None => None,
        }
    }
}

impl Drop for Asker {
    fn drop(&mut self) {
        self.lang_server.shutdown().expect("Shutdown message failed");
        self.lang_server.exit().expect("Exit failed");
    }
}

fn test_run(asker: &mut Asker) -> Result<(), Error> {
    let pattern_string = "restore_wait_other_tasks";

    let matches = asker.search(pattern_string)?;

    let mut graph = DiGraph::<String, &str>::new();
    let mut node_map = HashMap::new();

    let child = {
        let mut children = asker.find_symbols(&matches);
        if children.len() != 1 {
            info!("Children: {:#?}", children);
            panic!("Child not found or too many children");
        }
        let child_symbol = children.pop().unwrap();
        let child = graph.add_node(child_symbol.name.clone());
        node_map.insert(child_symbol.name, child);
        child
    };

    for m in matches {
        if let Some(s) = asker.find_parent(m) {
            let parent = {
                if node_map.contains_key(&s.name) == false {
                    let parent = graph.add_node(s.name.clone());
                    node_map.insert(s.name.clone(), parent);
                }
                node_map.get(&s.name).unwrap()
            };
            graph.update_edge(*parent, child, "");
        }
    }

    println!("{}", Dot::with_config(&graph, &[Config::EdgeNoLabel]));

    Ok(())
}

async fn graphiql() -> HttpResponse {
    let html = graphiql_source("http://127.0.0.1:8080/graphql");
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html)
}

async fn graphql(
    st: web::Data<Arc<schema::Schema>>,
    data: web::Json<GraphQLRequest>,
) -> Result<HttpResponse, actix_web::Error> {
    let user = web::block(move || {
        let res = data.execute(&st, &());
        Ok::<_, serde_json::error::Error>(serde_json::to_string(&res)?)
    })
    .await?;
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(user))
}

#[actix_rt::main]
async fn server_main(asker: Asker) -> io::Result<()> {
    let schema = std::sync::Arc::new(schema::create_schema());
    HttpServer::new(move || {
        App::new()
            .data(schema.clone())
            .service(web::resource("/graphql").route(web::post().to(graphql)))
            .service(web::resource("/graphiql").route(web::get().to(graphiql)))
    })
        .bind("127.0.0.1:8080")?
        .run()
        .await
}

fn main() -> Result<(), Error> {
    let opt = Opt::from_args();

    stderrlog::new()
        .module(module_path!())
        .quiet(opt.quiet)
        .verbosity(opt.verbose)
        .timestamp(opt.ts.unwrap_or(stderrlog::Timestamp::Off))
        .init()
        .unwrap();

    let mut asker = Asker::new(&opt)?;

    test_run(&mut asker)?;

    server_main(asker)?;

    Ok(())
}
