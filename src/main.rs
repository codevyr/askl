use std::fmt;
use std::str;

use log;
use log::{info};
use stderrlog;

use structopt;
use structopt::StructOpt;

use std::collections::HashMap;

use petgraph::graph::DiGraph;
use petgraph::dot::{Dot, Config};

mod schema;
mod asker;
mod language_server;
mod search;
mod web_server;

use std::sync::{Arc, Mutex};

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

    web_server::server_main(asker)?;

    Ok(())
}
