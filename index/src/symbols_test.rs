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

/// The parents family carries one row per *(reference site, enclosing
/// declaration)* — never one per instance of the referenced symbol.
///
/// `test.f` in the modules fixture is defined twice (instance 86 in /bar.c,
/// 96 in /main.c) and referenced twice, so the old query — which joined the
/// referenced symbol's instances to obtain a candidate `to_instance` —
/// returned two rows for every (site, enclosing) pair.  On the real index the
/// same fan-out turned 508 pairs into 2540 rows for `i915_ggtt`, enough to
/// fill the result budget's row limit with duplicates.
#[tokio::test(flavor = "current_thread")]
async fn multi_instance_edges_do_not_fan_out() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_MODULES).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    let selection = index
        .find_symbol(
            &CompositeFilter::leaf(CompoundNameMixin::new("test.f")),
            ScopeContext::Unscoped,
            ScopeContext::Skip,
            &eph,
        )
        .await?
        .into_inner();

    // Both definitions are selected...
    let mut instances: Vec<i64> = selection
        .nodes
        .iter()
        .map(|n| n.symbol_instance.id)
        .collect();
    instances.sort_unstable();
    assert_eq!(instances, vec![86, 96], "test.f is defined twice");

    // ...but each (reference, enclosing declaration) appears exactly once.
    let mut pairs: Vec<(i64, i64)> = selection
        .parents
        .iter()
        .map(|p| (p.symbol_ref.id, p.from_instance.id))
        .collect();
    let total = pairs.len();
    pairs.sort_unstable();
    pairs.dedup();
    assert_eq!(
        pairs.len(),
        total,
        "a (reference, enclosing declaration) pair must not repeat per target instance"
    );

    // And every row resolves to the same, deliberately chosen instance.
    let targets: std::collections::HashSet<i64> =
        selection.parents.iter().map(|p| p.to_instance.id).collect();
    assert_eq!(
        targets,
        std::collections::HashSet::from([86]),
        "the target instance is chosen once (definition, then lowest id), not per row"
    );

    Ok(())
}

/// The token containment `CompoundNameMixin` emits beside its lquery is a
/// *pre-filter*: it must narrow the candidate set without ever removing a
/// match.  Soundness rests on one implication -- an ordered-subset match on
/// labels `a..b` means both are labels of the path, so
/// `{a,b} subset labels(path)` -- and that in turn rests on the query's
/// tokens appearing literally in `symbol_path`.
///
/// This test is the guard for exactly that.  It compares the engine's answer
/// against the bare lquery run directly, so it fails if
/// `index.symbol_name_to_ltree`'s normalization ever diverges from
/// `normalize_symbol_tokens` -- which is what would silently start dropping
/// matches.  `(*k8s.io/kubernetes/pkg/kubelet.Kubelet).Run` in the fixture is
/// the case that matters: its labels are *not* substrings of its `name`, so a
/// `name`-based pre-filter would fail here.
#[tokio::test(flavor = "current_thread")]
async fn test_compound_name_prefilter_never_drops_a_match() -> anyhow::Result<()> {
    use crate::db_diesel::Index;
    use crate::symbols::build_lquery;
    use diesel::RunQueryDsl;

    #[derive(diesel::QueryableByName)]
    struct NameRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        name: String,
    }

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index
        .load_test_input(Index::TEST_INPUT_SYMBOL_TOKENS)
        .await?;

    let conn = &mut <PgConnection as Connection>::establish(&url)
        .map_err(|e| anyhow::anyhow!("connect: {e}"))?;

    // Deep paths, shared prefixes, near-misses, and a name whose labels are
    // not substrings of it.  Single tokens included: CompoundNameMixin is
    // constructed with them too, and containment on one label is still sound.
    let patterns = [
        "kubelet.run",
        "kubelet",
        "run",
        "kubernetes.run",
        "kubelet.Kubelet.Run",
        "k8s.kubelet.Run",
        "pkg.run",
        "kubelet.aaaaaaaaaaa.run",
        "kubeleter.run",
        "nosuchtoken.run",
    ];

    for pattern in patterns {
        let filter = CompositeFilter::leaf(CompoundNameMixin::new(pattern));
        let mut engine: Vec<String> = index
            .find_symbol(
                &filter,
                ScopeContext::Skip,
                ScopeContext::Skip,
                &EphContext::rooted(index.load_root_layers().await?),
            )
            .await?
            .into_inner()
            .nodes
            .iter()
            .map(|n| n.symbol.name.clone())
            .collect();
        engine.sort();
        engine.dedup();

        // Reference: the lquery alone, no pre-filter, joined to instances the
        // way the engine's current-query is.
        let lquery = build_lquery(pattern, false, true)
            .unwrap_or_else(|| panic!("{pattern:?} produced no lquery"));
        let mut reference: Vec<String> = diesel::sql_query(format!(
            "SELECT DISTINCT s.name FROM index.symbols s \
             JOIN index.symbol_instances si ON si.symbol = s.id \
             WHERE s.symbol_path ~ '{lquery}'::lquery"
        ))
        .load::<NameRow>(conn)
        .map_err(|e| anyhow::anyhow!("reference query for {pattern:?}: {e}"))?
        .into_iter()
        .map(|r| r.name)
        .collect();
        reference.sort();
        reference.dedup();

        assert_eq!(
            engine, reference,
            "pre-filtered result set diverged from the bare lquery for {pattern:?}"
        );
    }

    Ok(())
}
