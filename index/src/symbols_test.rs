use crate::symbols::{package_match, partial_name_match, Symbol, SymbolId};
use diesel::pg::PgConnection;
use diesel::Connection;
use testcontainers::{clients, core::WaitFor, GenericImage};
use tokio::time::{sleep, Duration};

#[test]
fn test_partial_name_matcher() {
    let test_cases = vec![
        // (symbol_name, search_term, should_match)
        ("foo", "foo", true),
        ("bar.foo", "foo", true),
        ("bar.foo", "bar.foo", true),
        ("zar/bar.foo", "bar.foo", true),
        ("foo.bar", "bar.foo", false),
        ("barfoo", "foo", false),
        (
            "(*k8s.io/kubelet/pkg/apis/deviceplugin/v1beta1.devicePluginClient).Allocate",
            "devicePluginClient.Allocate",
            true,
        ),
    ];

    for (symbol_name, search_term, expected_match) in test_cases {
        let id = SymbolId::new(1);
        let sym = Symbol::new(id, symbol_name.to_string());

        let matcher = partial_name_match(search_term);
        let matched_symbol = matcher((&id, &sym));

        assert_eq!(
            matched_symbol.is_some(),
            expected_match,
            "Symbol '{}' with search term '{}' should{} match",
            symbol_name,
            search_term,
            if expected_match { "" } else { " not" }
        );
    }
}

#[test]
fn test_package_matcher_with_multiple_patterns() {
    let symbols = vec![
        Symbol::new(SymbolId::new(1), "foo.bar.Component".to_string()),
        Symbol::new(SymbolId::new(2), "foo.bar.baz.Component".to_string()),
        Symbol::new(SymbolId::new(3), "foo.qux.Utility".to_string()),
        Symbol::new(SymbolId::new(4), "pkg/apis/core/v1.Pod".to_string()),
    ];

    let test_cases: Vec<(&str, Vec<&str>)> = vec![
        (
            "foo.bar",
            vec!["foo.bar.Component", "foo.bar.baz.Component"],
        ),
        ("foo.bar.baz", vec!["foo.bar.baz.Component"]),
        ("foo.qux", vec!["foo.qux.Utility"]),
        ("pkg/apis/core/v1", vec!["pkg/apis/core/v1.Pod"]),
        ("pkg/apis/core", vec!["pkg/apis/core/v1.Pod"]),
    ];

    for (pattern, expected_names) in test_cases {
        let matcher = package_match(pattern);
        let mut matched_names = Vec::new();

        for symbol in &symbols {
            if let Some(matched) = matcher((&symbol.id, symbol)) {
                matched_names.push(matched.name.clone());
            }
        }

        let expected: Vec<String> = expected_names.iter().map(|name| name.to_string()).collect();

        assert_eq!(
            matched_names, expected,
            "Pattern '{}' should match symbols {:?}",
            pattern, expected_names
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn test_find_symbol_by_name() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

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

    let index = Index::connect(&url).await?;

    // Test with empty database first
    let empty_selection = index.find_symbol_by_name("nonexistent").await?;
    assert!(empty_selection.nodes.is_empty());
    assert!(empty_selection.parents.is_empty());
    assert!(empty_selection.children.is_empty());

    // Load test data
    index.load_test_input(Index::TEST_INPUT_A).await?;

    // Test searching for symbols - use "a" which we know exists
    let selection = index.find_symbol_by_name("a").await?;
    assert!(
        !selection.nodes.is_empty(),
        "Should find symbols with 'a' in the name"
    );

    // Test searching for symbols - use "main" which we know exists
    let selection = index.find_symbol_by_name("main").await?;
    assert!(
        !selection.nodes.is_empty(),
        "Should find symbols with 'main' in the name"
    );
    assert_eq!(
        selection.children.len(),
        2,
        "Should have two children for 'main'"
    );
    assert_eq!(
        selection.parents.len(),
        0,
        "Should have no parents for 'main'"
    );

    // Verify that DeclarationFull is properly populated
    for s in &selection.nodes {
        assert!(!s.symbol.name.is_empty(), "Symbol name should not be empty");
        // The file field should be properly populated
        assert!(
            !s.file.filesystem_path.is_empty(),
            "File path should not be empty"
        );
        assert!(
            !s.module.module_name.is_empty(),
            "Module name should not be empty"
        );
    }

    // Test compound name search
    let compound_selection = index.find_symbol_by_name("mai.n").await?;
    assert!(
        compound_selection.nodes.is_empty(),
        "Should find no symbols with compound name search"
    );

    Ok(())
}

async fn wait_for_postgres(url: &str) -> anyhow::Result<()> {
    let mut delay = Duration::from_millis(50);
    for attempt in 1..=10 {
        match PgConnection::establish(url) {
            Ok(_) => return Ok(()),
            Err(err) => {
                if attempt == 10 {
                    return Err(anyhow::anyhow!(
                        "Postgres not ready after {} attempts: {}",
                        attempt,
                        err
                    ));
                }
            }
        }
        sleep(delay).await;
        delay = std::cmp::min(delay * 2, Duration::from_secs(1));
    }
    Ok(())
}
