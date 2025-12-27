use crate::cfg::EdgeList;
use crate::execution_context::ExecutionContext;
use crate::statement::ExecutionResult;
use crate::{cfg::ControlFlowGraph, parser::parse};
use anyhow::Result;
use index::db_diesel::Index;
use tokio::{runtime::Runtime, task};

pub const TEST_INPUT_A: &'static str = index::db::Index::TEST_INPUT_A;
pub const TEST_INPUT_B: &'static str = index::db::Index::TEST_INPUT_B;
pub const TEST_INPUT_MODULES: &'static str = index::db::Index::TEST_INPUT_MODULES;

pub fn format_edges(edges: EdgeList) -> Vec<String> {
    edges
        .as_vec()
        .into_iter()
        .map(|(f, t, _)| format!("{}-{}", f.declaration_id, t.declaration_id))
        .collect()
}

pub async fn run_query_async_err(askl_input: &str, askl_query: &str) -> Result<ExecutionResult> {
    let index_diesel = Index::new_in_memory().await.unwrap();
    index_diesel.load_test_input(askl_input).await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(index_diesel);

    let ast = parse(askl_query)?;
    println!("{:#?}", ast);

    let mut ctx = ExecutionContext::new();
    let res = ast.execute(&mut ctx, &cfg).await?;
    Ok(res)
}

pub async fn run_query_async(askl_input: &str, askl_query: &str) -> ExecutionResult {
    run_query_async_err(askl_input, askl_query).await.unwrap()
}

pub fn run_query_err(askl_input: &str, askl_query: &str) -> Result<ExecutionResult> {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async {
        run_query_async_err(askl_input, askl_query).await
    })
}

pub fn run_query(askl_input: &str, askl_query: &str) -> ExecutionResult {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async {
        run_query_async(askl_input, askl_query).await
    })
}
