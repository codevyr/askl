use clap::Parser;
use env_logger;
use indicatif::ProgressBar;
use log::{debug, warn};
use console::{style, Emoji};
mod compile_commands;
mod lsp_client;
use crate::compile_commands::{CompileCommands, FileList};
use crate::lsp_client::LSPClient;
use askl::symbols::{Location, Symbol, SymbolMap, Symbols};

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Command to invoke LSP server
    #[clap(short, long)]
    lsp_command: String,

    // Root of the project to index
    #[clap(short, long)]
    project_root: String,

    // Path to compile command to get the list of source files
    #[clap(short, long)]
    compile_commands: String,

    // Assume the following language for the source files
    #[clap(long)]
    language: String,

    // File where to save index
    #[clap(short, long)]
    output: String,

    // File where to store LSP server log
    #[clap(long)]
    lsp_log: Option<String>,
}

static LOOKING_GLASS: Emoji<'_, '_> = Emoji("🔍  ", "");
static CLIP: Emoji<'_, '_> = Emoji("🔗  ", "");
static PAPER: Emoji<'_, '_> = Emoji("📃  ", "");
static SPARKLE: Emoji<'_, '_> = Emoji("✨ ", ":-)");

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    println!("{} {}Initializing...", style("[1/4]").bold().dim(), SPARKLE);

    let mut lsp = LSPClient::start(
        &args.lsp_command,
        &args.project_root,
        args.lsp_log.as_deref(),
    )?;

    lsp.initialize().await?;
    lsp.initialized().await?;

    let compile_commands = CompileCommands::new(&args.compile_commands)?;

    println!(
        "{} {}Opening files...",
        style("[2/4]").bold().dim(),
        LOOKING_GLASS
    );

    let pb = ProgressBar::new(compile_commands.len() as u64);
    let mut symbols_db = SymbolMap::new();
    for (_i, file) in compile_commands.iter().enumerate() {
        pb.inc(1);
        let doc = lsp.open_file(file, &args.language).await?;
        debug!("open {}", doc.uri);
        let symbols = lsp.document_symbol(&doc).await?;

        match symbols {
            Some(lsp_types::DocumentSymbolResponse::Nested(symbols)) => {
                for symbol in symbols {
                    symbols_db.add(
                        Location {
                            file: doc.uri.clone(),
                            position: symbol.selection_range.start,
                        },
                        Symbol {
                            path: doc.uri.clone(),
                            name: symbol.name,
                            detail: symbol.detail,
                            kind: symbol.kind,
                            range: symbol.range.into(),
                            selection_range: symbol.selection_range.into(),
                            parents: Vec::new(),
                        },
                    );
                }
            }
            Some(lsp_types::DocumentSymbolResponse::Flat(symbols)) => {
                warn!("Flat symbols: {:#?}", symbols);
                for symbol in symbols {
                    symbols_db.add(
                        Location {
                            file: doc.uri.clone(),
                            position: symbol.location.range.start,
                        },
                        Symbol {
                            path: doc.uri.clone(),
                            name: symbol.name,
                            detail: None,
                            kind: symbol.kind,
                            range: symbol.location.range.into(),
                            selection_range: symbol.location.range.into(),
                            parents: Vec::new(),
                        },
                    );
                }
            }
            None => debug!("No symbols have been found in {}", file),
        }

        // lsp.close_file(&doc).await?;
    }
    pb.finish_and_clear();

    println!(
        "{} {}Finding references...",
        style("[3/4]").bold().dim(),
        CLIP
    );

    let symbols_vec = symbols_db.into_vec().clone();
    let pb = ProgressBar::new(symbols_vec.len() as u64);
    for loc in symbols_vec.iter() {
        pb.inc(1);
        let refs = lsp.find_references(&loc.file, loc.position).await?;
        for r in refs.iter() {
            let r_loc = Location {
                file: r.uri.clone(),
                position: lsp_types::Position {
                    line: r.range.start.line,
                    character: r.range.start.character,
                },
            };
            match symbols_db.find(&r_loc) {
                Some(parent) => symbols_db.add_parent(loc, &parent),
                None => {}
            };
        }
    }
    pb.finish_and_clear();

    println!("{} {}Writing result...", style("[4/4]").bold().dim(), PAPER);

    std::fs::write(args.output, symbols_db.to_string())?;

    Ok(())
}
