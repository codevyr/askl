use std::{fs::File, sync::Arc};

use clap::Parser;
use index::db::Index;
use index::clang::{run_clang_ast, CompileCommand, GlobalVisitorState, Node, ParsedNode};
use indicatif::ProgressBar;
use log::debug;
use tokio::sync::Semaphore;

/// Indexer for askl
#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Path to compile command to get the list of source files
    #[clap(value_name = "FILE")]
    compile_commands: String,

    /// Command to invoke Clang
    #[clap(short, long, default_value = "clang")]
    clang: String,

    /// Maximum parallelism
    #[clap(short, long, default_value = "1")]
    parallelism: usize,

    /// Limit how many files can be processed
    #[clap(long)]
    trim: Option<usize>,

    /// Output file to store the resulting symbol map
    #[clap(short, long, default_value = "askli.db")]
    output: String,
}

async fn parse_all(
    args: &Args,
    compile_commands: Vec<CompileCommand>,
) -> Vec<anyhow::Result<ParsedNode>> {
    let sem = Arc::new(Semaphore::new(args.parallelism));
    let mut tasks = Vec::with_capacity(compile_commands.len());
    let pb = ProgressBar::new(compile_commands.len() as u64);
    for c in compile_commands {
        let permit = Arc::clone(&sem).acquire_owned().await.unwrap();
        let pb = pb.clone();
        let clang = args.clang.clone();
        tasks.push(tokio::spawn(async move {
            let res = run_clang_ast(&clang, c.clone()).await;
            if let Err(err) = &res {
                debug!("Run AST {} in {:?}", err, c);
            }
            pb.inc(1);
            drop(permit);
            res
        }));
    }

    let mut outputs = Vec::<anyhow::Result<ParsedNode>>::with_capacity(tasks.len());
    for task in tasks {
        outputs.push(task.await.unwrap());
    }

    outputs
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let index = Index::new_or_connect(&args.output).await?;

    let file = File::open(&args.compile_commands).expect("file should open read only");
    let mut compile_commands: Vec<CompileCommand> =
        serde_json::from_reader(file).expect("file should be proper JSON");

    if let Some(trim) = args.trim {
        compile_commands.truncate(trim);
    }

    let outputs = parse_all(&args, compile_commands).await;
    let nodes = outputs
        .into_iter()
        .filter(|r| r.is_ok())
        .map(|r| r.unwrap());

    let mut state = GlobalVisitorState::new(index, "main");
    for node in nodes {
        state.extract_symbol_map_root(node).await?;
    }

    Ok(())
}
