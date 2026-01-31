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

fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();
    println!(
        "create-index is deprecated. Use the protobuf upload API instead (requested output: {}).",
        args.output
    );

    Ok(())
}
