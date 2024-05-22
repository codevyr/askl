use std::{path::Path, fs};

use anyhow::bail;
use askl::index::Index;
use clap::Parser;

/// Indexer for askl
#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Force recreation of an index
    #[clap(short, long, action)]
    force: bool,

    /// Output file to store the resulting symbol map
    #[clap(short, long, default_value = "askli.db")]
    output: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let output = Path::new(&args.output);
    if args.force && output.exists() {
        // Delete old database
        fs::remove_file(output)?
    } else if output.exists() {
        bail!("File exists");
    }

    Index::new_or_connect(&args.output).await?;

    Ok(())
}
