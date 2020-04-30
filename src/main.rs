use std::fmt;
use std::str;
use std::io;

use log;
use log::{info};
use stderrlog;

use structopt;
use structopt::StructOpt;

use std::collections::HashMap;

use petgraph::graph::DiGraph;
use petgraph::dot::{Dot, Config};

use actix_web::{web, App, HttpResponse, HttpServer, Responder};

mod schema;
mod asker;
mod language_server;
mod search;

use std::sync::{Arc, Mutex};
use juniper::http::GraphQLRequest;
use juniper::http::graphiql::graphiql_source;

#[derive(Debug)]
struct LspError(&'static str);

#[derive(StructOpt, Debug)]
#[structopt()]
pub struct Opt {
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


fn test_run(asker: Arc<Mutex<asker::Asker>>) -> Result<(), Error> {
    let mut asker = asker.lock().unwrap();
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

#[derive(Clone)]
struct AppData {
    schema: Arc<schema::Schema>,
    asker: Arc<Mutex<asker::Asker>>,
}

async fn graphql(
    st: web::Data<AppData>,
    data: web::Json<GraphQLRequest>,
) -> Result<HttpResponse, actix_web::Error> {
    let user = web::block(move || {
        let ctx = schema::Context{
            asker: st.asker.clone(),
        };
        let res = data.execute(&st.schema, &ctx);
        Ok::<_, serde_json::error::Error>(serde_json::to_string(&res)?)
    })
    .await?;
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(user))
}

#[actix_rt::main]
async fn server_main(asker: Arc<Mutex<asker::Asker>>) -> io::Result<()> {
    let schema = std::sync::Arc::new(schema::create_schema());
    HttpServer::new(move || {
        App::new()
            .data(AppData{
                schema: schema.clone(),
                asker: asker.clone(),
            })
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

    let asker = Arc::new(Mutex::new(asker::Asker::new(&opt)?));

    test_run(asker.clone())?;

    server_main(asker)?;

    Ok(())
}
