use std::fmt;
use std::str;

use log;
use log::{info};
use stderrlog;

use structopt;
use structopt::StructOpt;

use lsp_types::{DocumentSymbolResponse, SymbolKind, TextDocumentItem, Range, DocumentSymbol, Url};

mod language_server;
use language_server::{LanguageServerLauncher, LanguageServer};

mod search;
use search::{SearchLauncher, Search, Match};

use std::collections::HashMap;

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

#[derive(Debug)]
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
    pub fn new(searcher: Box<dyn Search>, mut lang_server: Box<dyn LanguageServer>) -> Result<Asker, Error> {
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
                    info!("Found nested symbol: {:#?}", symbol);
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

            println!("Document: {:#?}", m.filename);
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

    pub fn find_parents(&mut self, matches: Vec<Match>) -> Result<(), Error> {
        for m in matches {
            println!("Searching parents: {:#?}", m);
        }
        Ok(())
    }
}

impl Drop for Asker {
    fn drop(&mut self) {
        self.lang_server.shutdown().expect("Shutdown message failed");
        self.lang_server.exit().expect("Exit failed");
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
    let pattern_string = "restore_wait_other_tasks";
    let languages: Vec<String> = vec!["cpp".to_owned(), "cc".to_owned()];

    let searcher = SearchLauncher::new()
        .engine("ack")
        .directory(project_home)
        .languages(&languages)
        .launch()?;

    let lang_server = LanguageServerLauncher::new()
        .server("/usr/bin/clangd-9".to_owned())
        .project(project_home.to_owned())
        .languages(languages)
        .launch()
        .expect("Failed to spawn clangd");

    let mut asker = Asker::new(searcher, lang_server)?;

    let matches = asker.search(pattern_string)?;

    asker.find_parents(matches)?;

    Ok(())
}
