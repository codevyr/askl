use anyhow::Result;
use clap::Parser;
use env_logger;
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

fn main() -> Result<()> {
    env_logger::init();
    let args = Args::parse();

    let symbols: SymbolMap = serde_json::from_slice(&std::fs::read(args.index)?)?;

    let ast = parse(&args.query)?;

    let mut executor = Executor::new(ast)?;
    executor.add_symbols(symbols);

    let cfg = executor.run();
    println!("{:?}", Dot::with_config(&cfg, &[Config::EdgeNoLabel]));

    Ok(())
}
