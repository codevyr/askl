use anyhow::Result;
use askl::cfg::ControlFlowGraph;
use clap::Parser;
use env_logger;
use console::{style, Emoji};
use log::debug;
use petgraph::dot::{Config, Dot};

use askl::parser::parse;
use askl::symbols::{SymbolMap, SymbolId};
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

    let cfg = ControlFlowGraph::from_symbols(&symbols);

    let sources : Vec<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();

    debug!("Global scope: {:#?}", ast);
    println!("Sources: {:#?}", sources.len());

    let (res_symbols, res_edges) = cfg
        .matched_symbols(sources, ast.as_ref(), true)
        .unwrap();

    println!("Symbols: {:#?}", res_symbols.len());
    println!("Edges: {:#?}", res_edges.0.len());
    
    // let (outer, inner) = ast.run(&cfg);
    
    let mut result_graph : DiGraphMap<&str, ()> = DiGraphMap::new();
    
    println!(
        "{} {}Making graph...",
        style("[5/5]").bold().dim(),
        PAPER
    );
    
    for (from, to) in res_edges.0 {
        let sym_from = symbols.map.get(&from).unwrap();
        let sym_to = symbols.map.get(&to).unwrap();

        result_graph.add_edge(&sym_from.name, &sym_to.name, ());
    }

    for loc in res_symbols {
        let sym= symbols.map.get(&loc).unwrap();
        result_graph.add_node(&sym.name);
    }

    // println!("{:?}", Dot::with_config(&result_graph, &[Config::EdgeNoLabel]));
    std::fs::write("res.gv", format!("{:?}", Dot::with_config(&result_graph, &[Config::EdgeNoLabel]))).expect("Unable to write file");
    Ok(())
}
