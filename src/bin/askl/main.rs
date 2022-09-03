use anyhow::Result;
use clap::Parser;
use env_logger;
use log::{debug, warn};

use askl::parser::Askl;
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

    let symbols = SymbolMap::from_slice(&std::fs::read(args.index)?)?;

    let ask = Askl::new(&args.query)?;
    // println!("S {:#?}", symbols);

    Ok(())
}
