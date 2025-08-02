use std::collections::HashSet;

use crate::cfg::{EdgeList, NodeList};
use crate::execution_context::ExecutionContext;
use crate::{cfg::ControlFlowGraph, parser::parse};
use anyhow::{anyhow, Result};
use index::db::Index;
use index::symbols::SymbolMap;
use tokio::{runtime::Runtime, task};

pub const TEST_INPUT_A: &'static str = index::db::Index::TEST_INPUT_A;
pub const TEST_INPUT_B: &'static str = index::db::Index::TEST_INPUT_B;

pub fn format_edges(edges: EdgeList) -> Vec<String> {
    edges
        .as_vec()
        .into_iter()
        .map(|(f, t, _)| format!("{}-{}", f, t))
        .collect()
}

pub async fn run_query_async_err(
    askl_input: &str,
    askl_query: &str,
) -> Result<(NodeList, EdgeList)> {
    let index = Index::new_in_memory().await.unwrap();
    index.load_test_input(askl_input).await.unwrap();
    let symbols: SymbolMap = SymbolMap::from_index(&index).await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(symbols, index);

    let ast = parse(askl_query)?;
    println!("{:#?}", ast);

    let mut ctx = ExecutionContext::new();
    let res = ast.execute(&mut ctx, &cfg, None, &HashSet::new()).await;
    if res.is_none() {
        return Err(anyhow!("Did not resolve any symbols"));
    }
    let (_, nodes, edges) = res.unwrap();
    Ok((nodes, edges))
}

pub async fn run_query_async(askl_input: &str, askl_query: &str) -> (NodeList, EdgeList) {
    run_query_async_err(askl_input, askl_query).await.unwrap()
}

pub fn run_query_err(askl_input: &str, askl_query: &str) -> Result<(NodeList, EdgeList)> {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async {
        run_query_async_err(askl_input, askl_query).await
    })
}

pub fn run_query(askl_input: &str, askl_query: &str) -> (NodeList, EdgeList) {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async {
        run_query_async(askl_input, askl_query).await
    })
}
