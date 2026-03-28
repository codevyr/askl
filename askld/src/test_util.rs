use crate::cfg::EdgeList;
use crate::execution_context::ExecutionContext;
use crate::statement::ExecutionResult;
use crate::test_support::{postgres_test_image, postgres_url, wait_for_postgres};
use crate::{cfg::ControlFlowGraph, parser::parse};
use anyhow::Result;
use index::db_diesel::Index;
use std::collections::HashMap;
use std::sync::{LazyLock, Mutex, Once, OnceLock};
use testcontainers::{clients, Container, GenericImage};
use tokio::{runtime::Runtime, task};

pub const TEST_INPUT_A: &'static str = index::db_diesel::Index::TEST_INPUT_A;
pub const TEST_INPUT_B: &'static str = index::db_diesel::Index::TEST_INPUT_B;
pub const TEST_INPUT_MODULES: &'static str = index::db_diesel::Index::TEST_INPUT_MODULES;
pub const TEST_INPUT_CONTAINMENT: &'static str = index::db_diesel::Index::TEST_INPUT_CONTAINMENT;
pub const TEST_INPUT_TREE_BROWSER: &'static str = index::db_diesel::Index::TEST_INPUT_TREE_BROWSER;
pub const TEST_INPUT_NESTED_FUNC: &'static str = index::db_diesel::Index::TEST_INPUT_NESTED_FUNC;
pub const VERB_TEST: &'static str = index::db_diesel::Index::VERB_TEST;

pub fn format_edges(edges: EdgeList) -> Vec<String> {
    edges
        .as_vec()
        .into_iter()
        .map(|(f, t, _)| format!("{}-{}", f.instance_id, t.instance_id))
        .collect()
}

// --- Shared fixture infrastructure ---

struct SharedFixture {
    _container: Container<'static, GenericImage>,
    url: String,
    index: Index,
}

// Safety: The Cli is leaked to 'static, so Container<'static, GenericImage> holds no
// dangling borrow. After OnceLock initialization, only shared & references are used
// (no mutation). Index wraps an Arc-based r2d2 pool which is Send+Sync.
unsafe impl Send for SharedFixture {}
unsafe impl Sync for SharedFixture {}

static DOCKER: OnceLock<&'static clients::Cli> = OnceLock::new();

fn get_docker() -> &'static clients::Cli {
    DOCKER.get_or_init(|| Box::leak(Box::new(clients::Cli::default())))
}

// --- atexit cleanup: remove Docker containers when the test process exits ---

static CONTAINER_IDS: Mutex<Vec<String>> = Mutex::new(Vec::new());
static CLEANUP_REGISTERED: Once = Once::new();

extern "C" fn cleanup_containers() {
    let ids = CONTAINER_IDS.lock().unwrap_or_else(|e| e.into_inner());
    if !ids.is_empty() {
        let _ = std::process::Command::new("docker")
            .arg("rm")
            .arg("-f")
            .args(ids.iter().map(|s| s.as_str()))
            .output();
    }
}

fn register_container(id: String) {
    CLEANUP_REGISTERED.call_once(|| {
        extern "C" {
            fn atexit(f: extern "C" fn()) -> std::os::raw::c_int;
        }
        unsafe { atexit(cleanup_containers); }
    });
    CONTAINER_IDS.lock().unwrap().push(id);
}

// ---

const ALL_FIXTURES: &[&str] = &[
    TEST_INPUT_A,
    TEST_INPUT_B,
    TEST_INPUT_MODULES,
    TEST_INPUT_CONTAINMENT,
    TEST_INPUT_TREE_BROWSER,
    TEST_INPUT_NESTED_FUNC,
    VERB_TEST,
];

static FIXTURES: LazyLock<HashMap<&'static str, OnceLock<SharedFixture>>> = LazyLock::new(|| {
    ALL_FIXTURES.iter().map(|&name| (name, OnceLock::new())).collect()
});

fn create_fixture(fixture: &str) -> SharedFixture {
    // Run in a dedicated thread to avoid "nested runtime" panics when called
    // from within an existing tokio runtime.
    let fixture = fixture.to_owned();
    std::thread::spawn(move || {
        let docker = get_docker();
        let container = docker.run(postgres_test_image());
        register_container(container.id().to_string());
        let port = container.get_host_port_ipv4(5432);
        let url = postgres_url(port);

        let rt = Runtime::new().unwrap();
        let index = rt.block_on(async {
            wait_for_postgres(&url).await.unwrap();
            Index::connect_with_test_input(&url, &fixture).await.unwrap()
        });

        SharedFixture {
            _container: container,
            url,
            index,
        }
    })
    .join()
    .expect("fixture creation thread panicked")
}

fn get_shared_fixture(fixture: &str) -> &'static SharedFixture {
    let lock = FIXTURES
        .get(fixture)
        .unwrap_or_else(|| panic!("Unknown fixture: {}", fixture));
    lock.get_or_init(|| create_fixture(fixture))
}

pub fn get_shared_index(fixture: &str) -> Index {
    get_shared_fixture(fixture).index.clone()
}

pub fn get_shared_db_url(fixture: &str) -> &'static str {
    &get_shared_fixture(fixture).url
}

pub async fn run_query_async_err(askl_input: &str, askl_query: &str) -> Result<ExecutionResult> {
    let index = get_shared_index(askl_input);
    let cfg = ControlFlowGraph::from_symbols(index);

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
