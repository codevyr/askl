use std::{thread::sleep, time::Duration};

use askl::language_server::LanguageServerLauncher;
use clap::Parser;
use log::debug;
use serde::{Deserialize, Serialize};

#[derive(Debug, Serialize, Deserialize)]
struct CompileCommand {
    arguments: Option<Vec<String>>,
    command: Option<String>,
    directory: String,
    file: String,
    output: Option<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct CompileDb {
    entries: Vec<CompileCommand>,
}

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    // Path to the compilation data base
    #[clap(short, long)]
    compiledb: String,

    // Output file
    #[clap(short, long, default_value = "askl.sqlite")]
    out: String,

    // Path to clangd
    #[clap(long, default_value = "/usr/bin/clangd")]
    clangd: String,
}

fn main() {
    env_logger::init();
    let args = Args::parse();
    println!("{:?}", args);

    let compiledb: Vec<CompileCommand> =
        serde_json::from_slice(&std::fs::read(&args.compiledb).unwrap()).unwrap();

    let first_entry = compiledb.iter().next().expect("Database is empty");
    let language_list = vec!["cc".to_owned(), "cpp".to_owned()];

    let mut lang_server = LanguageServerLauncher::new()
        .server(args.clangd.clone())
        .project(first_entry.directory.clone())
        .languages(language_list)
        .launch()
        .expect("Failed to spawn clangd");

    let initialize_result = lang_server
        .initialize()
        .expect("Failed to initialize language server");

    debug!(
        "Initialize result: {:?}",
        initialize_result.capabilities.references_provider
    );

    lang_server
        .initialized()
        .expect("Language server is not initialized");

    compiledb.iter().for_each(|entry| {
        let uri = lang_server
            .document_open(&entry.file)
            .expect("Failed to open document");
        debug!("Document open result: {:?}", uri);

        sleep(Duration::from_secs(1));

        let res = lang_server
            .document_close(&uri)
            .expect("Failed to close document");
        debug!("Document close result: {:?}", res);
    });

    lang_server
        .workspace_symbols("")
        .expect("Failed to get workspace symbols");
}
