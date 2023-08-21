use anyhow::Result;
use askl::cfg::ControlFlowGraph;
use clap::Parser;
use env_logger;
use console::{style, Emoji};
use log::debug;
use petgraph::dot::{Config, Dot};

use askl::parser::parse;
use askl::symbols::SymbolMap;
use petgraph::prelude::DiGraphMap;

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

    println!(
        "{} {}Running query...",
        style("[4/5]").bold().dim(),
        PAPER
    );

    let cfg_in = ControlFlowGraph::from_symbols(&symbols);

    debug!("Global scope: {:#?}", ast);

    let (outer, inner) = ast.run(&cfg_in);

    let mut result_graph : DiGraphMap<&str, ()> = DiGraphMap::new();

    println!(
        "{} {}Making graph...",
        style("[5/5]").bold().dim(),
        PAPER
    );

    for (from, to) in inner.0 {
        let sym_from = symbols.map.get(from).unwrap();
        let sym_to = symbols.map.get(to).unwrap();

        result_graph.add_edge(&sym_from.name, &sym_to.name, ());
    }

    for loc in outer.0 {
        let sym= symbols.map.get(loc).unwrap();
        result_graph.add_node(&sym.name);
    }

    println!("{:?}", Dot::with_config(&result_graph, &[Config::EdgeNoLabel]));

    Ok(())
}
