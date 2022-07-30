use clap::Parser;

mod lsp_client;
use crate::lsp_client::LSPClient;

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
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let args = Args::parse();

    let mut lsp = LSPClient::start(&args.lsp_command, &args.project_root)?;

    lsp.initialize().await?;
    lsp.initialized().await?;

    println!("Hello World! {}", args.lsp_command);
    Ok(())
}
