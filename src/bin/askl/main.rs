use anyhow::Result;
use clap::Parser;
use env_logger;
use console::{style, Emoji};
use petgraph::dot::{Config, Dot};

use askl::executor::Executor;
use askl::parser::parse;
use askl::symbols::SymbolMap;

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Path to the index file
    #[clap(short, long)]
    index: String,

    // Query to process
    #[clap(value_name = "QUERY", index = 1)]
    query: String,
}

static LOOKING_GLASS: Emoji<'_, '_> = Emoji("🔍  ", "");
static SPARKLE: Emoji<'_, '_> = Emoji("✨ ", ":-)");
static CLIP: Emoji<'_, '_> = Emoji("🔗  ", "");
static PAPER: Emoji<'_, '_> = Emoji("📃  ", "");

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    println!(
        "{} {}Loading index...",
        style("[1/4]").bold().dim(),
        LOOKING_GLASS
    );
    let symbols: SymbolMap = serde_json::from_slice(&std::fs::read(args.index)?)?;

    println!(
        "{} {}Parsing query...",
        style("[2/4]").bold().dim(),
        SPARKLE
    );
    let ast = parse(&args.query)?;

    println!(
        "{} {}Adding symbols...",
        style("[3/4]").bold().dim(),
        CLIP
    );
    let mut executor = Executor::new(ast)?;
    executor.add_symbols(symbols);

    println!(
        "{} {}Running query...",
        style("[4/4]").bold().dim(),
        PAPER
    );
    let cfg = executor.run();
    println!("{:?}", Dot::with_config(&cfg, &[Config::EdgeNoLabel]));

    Ok(())
}
