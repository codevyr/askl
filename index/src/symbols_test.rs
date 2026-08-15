use crate::db_diesel::{CompositeFilter, CompoundNameMixin, EphContext, ScopeContext};
use crate::symbols::{package_match, partial_name_match, Symbol, SymbolId};
use diesel::pg::PgConnection;
use diesel::Connection;
use testcontainers::{clients, core::WaitFor, Container, GenericImage};
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
    let (_node, url) = start_postgres(&docker);

    wait_for_postgres(&url).await?;

    let mut index = Index::connect(&url).await?;

    // Test with empty database first
    let empty_filter = CompositeFilter::leaf(CompoundNameMixin::new("nonexistent"));
    let empty_selection = index
        .find_symbol(
            &empty_filter,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    assert!(empty_selection.nodes.is_empty());
    assert!(empty_selection.parents.is_empty());
    assert!(empty_selection.children.is_empty());

    // Load test data
    index.load_test_input(Index::TEST_INPUT_A).await?;

    // Test searching for symbols - use "a" which we know exists
    let a_filter = CompositeFilter::leaf(CompoundNameMixin::new("a"));
    let selection = index
        .find_symbol(
            &a_filter,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    assert!(
        !selection.nodes.is_empty(),
        "Should find symbols with 'a' in the name"
    );

    // Test searching for symbols - use "main" which we know exists
    let main_filter = CompositeFilter::leaf(CompoundNameMixin::new("main"));
    let selection = index
        .find_symbol(
            &main_filter,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    assert!(
        !selection.nodes.is_empty(),
        "Should find symbols with 'main' in the name"
    );

    // Verify that SymbolInstanceFull is properly populated
    for s in &selection.nodes {
        assert!(!s.symbol.name.is_empty(), "Symbol name should not be empty");
        // The object field should be properly populated
        assert!(
            !s.object.filesystem_path.is_empty(),
            "Object path should not be empty"
        );
    }

    // Test compound name search
    let compound_filter = CompositeFilter::leaf(CompoundNameMixin::new("mai.n"));
    let compound_selection = index
        .find_symbol(
            &compound_filter,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    assert!(
        compound_selection.nodes.is_empty(),
        "Should find no symbols with compound name search"
    );

    Ok(())
}

#[tokio::test(flavor = "current_thread")]
async fn test_find_symbol_by_name_token_ordering() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);

    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index
        .load_test_input(Index::TEST_INPUT_SYMBOL_TOKENS)
        .await?;

    let long_name = format!("kubelet.{}.run", "a".repeat(11));
    let filter1 = CompositeFilter::leaf(CompoundNameMixin::new("kubelet.run"));
    let selection = index
        .find_symbol(
            &filter1,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    let mut found_names: Vec<String> = selection
        .nodes
        .iter()
        .map(|node| node.symbol.name.clone())
        .collect();
    found_names.sort();

    assert_eq!(
        found_names,
        vec![long_name.clone()],
        "Expected only exact-token ordered match"
    );

    let kubelet_run = "(*k8s.io/kubernetes/pkg/kubelet.Kubelet).Run".to_string();
    let filter2 = CompositeFilter::leaf(CompoundNameMixin::new("kubelet"));
    let selection = index
        .find_symbol(
            &filter2,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    let mut found_names: Vec<String> = selection
        .nodes
        .iter()
        .map(|node| node.symbol.name.clone())
        .collect();
    found_names.sort();

    assert_eq!(
        found_names,
        vec![kubelet_run, long_name.clone()],
        "Expected only exact-token ordered match"
    );

    let filter3 = CompositeFilter::leaf(CompoundNameMixin::new("run.kubelet"));
    let reverse_selection = index
        .find_symbol(
            &filter3,
            ScopeContext::Skip,
            ScopeContext::Skip,
            &EphContext::rooted(index.load_root_layers().await?),
        )
        .await?
        .into_inner();
    assert!(
        reverse_selection.nodes.is_empty(),
        "Expected token order mismatch to return no results"
    );

    Ok(())
}

pub(crate) fn start_postgres(docker: &clients::Cli) -> (Container<'_, GenericImage>, String) {
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
    (node, url)
}

pub(crate) async fn wait_for_postgres(url: &str) -> anyhow::Result<()> {
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

#[tokio::test(flavor = "current_thread")]
async fn test_probe_instance_ids() -> anyhow::Result<()> {
    use crate::db_diesel::{EphContext, Index, ProbeOutcome, ProbeRole};

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;

    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_A).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    // Fixture: function instances 91..97 + 942(main), file instance 1001
    // and directory instance 1003 both spanning [0,10000) on object 1,
    // directory self-instance 1002 on object 2 — 11 instances total.
    // Calls: main(942) -> {a(91), b(92)}; a -> b (two call sites).

    // Selective predicate, below cap: the probe IS the exact result.
    let name_a = CompositeFilter::leaf(CompoundNameMixin::new("a"));
    let outcome = index.probe_instance_ids(&name_a, vec![], 10, &eph).await?;
    assert_eq!(outcome, ProbeOutcome::Resolved(vec![91]));

    // Unconstrained predicate: exact cap boundary.  11 rows exist.
    let unconstrained = CompositeFilter::and(vec![]);
    let outcome = index
        .probe_instance_ids(&unconstrained, vec![], 11, &eph)
        .await?;
    match &outcome {
        ProbeOutcome::Resolved(ids) => assert_eq!(ids.len(), 11, "all 11 instances"),
        other => panic!("expected Resolved at cap=11, got {:?}", other),
    }
    let outcome = index
        .probe_instance_ids(&unconstrained, vec![], 10, &eph)
        .await?;
    assert_eq!(
        outcome,
        ProbeOutcome::Capped,
        "cap=10 must detect an 11th row"
    );

    // RefsParentsOf: enclosing decls of b(92)'s call sites — a(91) and
    // main(942), plus the file/directory instances whose ranges cover the
    // sites (the same containment rule the engine's parents query uses).
    let outcome = index
        .probe_instance_ids(
            &unconstrained,
            vec![ProbeRole::RefsParentsOf(vec![92])],
            100,
            &eph,
        )
        .await?;
    // Symbol-level: the directory symbol is evidenced via instance 1003,
    // so its other instance (1002) is kept too — mirroring the worklist's
    // per-symbol constrain.
    assert_eq!(
        outcome,
        ProbeOutcome::Resolved(vec![91, 942, 1001, 1002, 1003])
    );

    // RefsChildrenOf: callees referenced from within main(942).
    let outcome = index
        .probe_instance_ids(
            &unconstrained,
            vec![ProbeRole::RefsChildrenOf(vec![942])],
            100,
            &eph,
        )
        .await?;
    assert_eq!(outcome, ProbeOutcome::Resolved(vec![91, 92]));

    // HasParentsOf: containers of a(91) — file 1001 (level 3 >= 2) and
    // directory 1003 (level 5 >= 2).
    let outcome = index
        .probe_instance_ids(
            &unconstrained,
            vec![ProbeRole::HasParentsOf(vec![91])],
            100,
            &eph,
        )
        .await?;
    assert_eq!(outcome, ProbeOutcome::Resolved(vec![1001, 1002, 1003]));

    // HasChildrenOf: rows contained in file 1001 — the eight function
    // instances (directory 1003 is excluded by the level rule).
    let outcome = index
        .probe_instance_ids(
            &unconstrained,
            vec![ProbeRole::HasChildrenOf(vec![1001])],
            100,
            &eph,
        )
        .await?;
    assert_eq!(
        outcome,
        ProbeOutcome::Resolved(vec![91, 92, 93, 94, 95, 96, 97, 942])
    );

    // Predicate ∧ containment role compose.
    let outcome = index
        .probe_instance_ids(
            &name_a,
            vec![ProbeRole::HasChildrenOf(vec![1001])],
            100,
            &eph,
        )
        .await?;
    assert_eq!(outcome, ProbeOutcome::Resolved(vec![91]));

    // Repeat probe: identical result through the cache path.
    let again = index
        .probe_instance_ids(
            &name_a,
            vec![ProbeRole::HasChildrenOf(vec![1001])],
            100,
            &eph,
        )
        .await?;
    assert_eq!(again, ProbeOutcome::Resolved(vec![91]));

    Ok(())
}
