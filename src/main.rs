use std::fmt;
use std::str;

use log;
use log::{info};
use stderrlog;

use structopt;
use structopt::StructOpt;

use lsp_types::DocumentSymbolResponse;

mod language_server;
use language_server::LanguageServerLauncher;

mod search;
use search::{SearchLauncher};

#[derive(Debug)]
struct LspError(&'static str);

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
}

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "LSP error: {}", self.0)
    }
}

impl std::error::Error for LspError {}

type Error = Box<dyn std::error::Error>;

fn print_symbols(symbols: Option<DocumentSymbolResponse>) -> Result<(), LspError> {
    match symbols {
        Some(DocumentSymbolResponse::Flat(_)) => {
            info!("Skipping flat symbols");
            Err(LspError("Flat symbols are unsupported"))
        },
        Some(DocumentSymbolResponse::Nested(v)) => {
            for symbol in v.iter() {
                info!("Found nested symbol: {:#?}", symbol);
            }
            Ok(())
        },
        None => {
            Err(LspError("No symbols found"))
        }
    }


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

    let project_home = "/home/desertfox/research/projects/ffmk/criu/";
    let languages: Vec<String> = vec!["cpp".to_owned(), "cc".to_owned()];

    let searcher = SearchLauncher::new()
        .engine("ack")
        .directory(project_home)
        .languages(&languages)
        .launch()?;

    let results = searcher.search("restore_wait_other_tasks".to_owned())?;
    println!("Matches: {:#?}", results);

    let mut lang_server = LanguageServerLauncher::new()
        .server("/usr/bin/clangd-9".to_owned())
        .project(project_home.to_owned())
        .launch()
        .expect("Failed to spawn clangd");


    lang_server.initialize()?;
    lang_server.initialized()?;

    let document = lang_server.document_open("criu/cr-restore.c", languages[0].as_str())?;
    print_symbols(lang_server.document_symbol(&document)?)?;
    lang_server.shutdown()?;
    lang_server.exit()?;

    println!("Hello, world!");

    Ok(())
}
