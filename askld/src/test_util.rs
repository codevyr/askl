use crate::cfg::EdgeList;
use crate::execution_context::ExecutionContext;
use crate::statement::ExecutionResult;
use crate::{cfg::ControlFlowGraph, parser::parse};
use anyhow::Result;
use index::db_diesel::Index;
use testcontainers::{clients, core::WaitFor, GenericImage};
use tokio::{runtime::Runtime, task};

use crate::test_support::wait_for_postgres;

pub const TEST_INPUT_A: &'static str = index::db_diesel::Index::TEST_INPUT_A;
pub const TEST_INPUT_B: &'static str = index::db_diesel::Index::TEST_INPUT_B;
pub const TEST_INPUT_MODULES: &'static str = index::db_diesel::Index::TEST_INPUT_MODULES;

pub fn format_edges(edges: EdgeList) -> Vec<String> {
    edges
        .as_vec()
        .into_iter()
        .map(|(f, t, _)| format!("{}-{}", f.declaration_id, t.declaration_id))
        .collect()
}

pub async fn run_query_async_err(askl_input: &str, askl_query: &str) -> Result<ExecutionResult> {
    let docker = clients::Cli::default();
    let image = GenericImage::new("postgres", "15-alpine")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "askl")
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ));
    let node = docker.run(image);
    let port = node.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/askl", port);

    wait_for_postgres(&url).await?;

    let index_diesel = Index::connect(&url).await.unwrap();
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
