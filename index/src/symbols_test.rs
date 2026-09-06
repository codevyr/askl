use crate::db_diesel::FamilyDemands;
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
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

/// `find_edges_between` prunes the target side to two rows per symbol; this
/// is the case that proves two is the floor, not one.
///
/// The fixture's `rec` is defined in both objects (instance 10 in /b.c, 20 in
/// /a.c) and calls itself from inside instance 10.  Instance 10 is therefore
/// BOTH the reference's enclosing declaration and the lowest-id instance of
/// the symbol referred to, so `from_inst.id != to_inst.id` eliminates exactly
/// the row `ORDER BY ..., to_inst.id` would have picked.  The edge must fall
/// through to instance 20 rather than disappear.
#[tokio::test(flavor = "current_thread")]
async fn recursive_edge_survives_when_the_lowest_target_is_the_caller() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index
        .load_test_input(Index::TEST_INPUT_RECURSIVE_MULTI_INSTANCE)
        .await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    let edges = index.find_edges_between(&[10, 20], &eph).await?.0;

    assert_eq!(
        edges.len(),
        1,
        "the self-call must produce exactly one edge, got {:?}",
        edges
            .iter()
            .map(|e| (e.from_instance_id, e.to_instance_id))
            .collect::<Vec<_>>()
    );
    assert_eq!(edges[0].from_instance_id, 10, "caller is instance 10");
    assert_eq!(
        edges[0].to_instance_id, 20,
        "instance 10 is excluded as its own target, so the edge resolves to 20"
    );
    Ok(())
}

/// A family the relationship cannot read is not fetched either.
///
/// `constrain_by_*` consults the REFS families only when the relationship
/// carries REFS and the containment families only when it carries HAS, so
/// `mod("x") { has func }` never looks at a REFS family.  On the real index
/// that family was 10.9 s and 70,202 rows for
/// `mod("amdgpu") { derive(type="has") func }`, every row discarded.
///
/// Both directions are asserted so neither half can pass vacuously: the
/// fixture's module both refers to things and contains them.
#[tokio::test(flavor = "current_thread")]
async fn a_family_the_relationship_cannot_read_is_not_fetched() -> anyhow::Result<()> {
    use crate::db_diesel::{FamilyDemand, Index};

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_CONTAINMENT).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);
    let module = CompositeFilter::leaf(CompoundNameMixin::new("testmodule"));

    let demands = |refs: bool, has: bool| FamilyDemands {
        parent: FamilyDemand { refs, has },
        children: FamilyDemand { refs, has },
    };

    let both = index
        .find_symbol(
            &module,
            ScopeContext::Unscoped,
            ScopeContext::Unscoped,
            demands(true, true),
            &eph,
        )
        .await?
        .into_inner();
    assert!(
        !both.children.is_empty() && !both.has_children.is_empty(),
        "premise: the module both refers to things and contains them"
    );

    let has_only = index
        .find_symbol(
            &module,
            ScopeContext::Unscoped,
            ScopeContext::Unscoped,
            demands(false, true),
            &eph,
        )
        .await?
        .into_inner();
    assert!(
        has_only.children.is_empty(),
        "a HAS-only relationship cannot read a REFS family; got {} rows",
        has_only.children.len()
    );
    assert!(
        !has_only.has_children.is_empty(),
        "...but the containment family it CAN read must still be fetched"
    );

    let refs_only = index
        .find_symbol(
            &module,
            ScopeContext::Unscoped,
            ScopeContext::Unscoped,
            demands(true, false),
            &eph,
        )
        .await?
        .into_inner();
    assert!(
        refs_only.has_children.is_empty(),
        "a REFS-only relationship cannot read a containment family; got {} rows",
        refs_only.has_children.len()
    );
    assert!(
        !refs_only.children.is_empty(),
        "...but the REFS family it CAN read must still be fetched"
    );

    assert_eq!(
        both.nodes.len(),
        has_only.nodes.len(),
        "gating a family must not change which symbols are selected"
    );
    Ok(())
}

/// Deriving a statement and constraining it must compute the SAME relation.
///
/// Once a capped statement can RE-derive as its neighbours settle, the order
/// notifications arrive in stops being cosmetic: if `derive(N)` and the edge
/// evidence `constrain` narrows by disagreed, the fixpoint a query reaches
/// would depend on which neighbour spoke first. That is an intermittent wrong
/// answer, and the cheapest defence is that both go through one query builder
/// rather than two implementations that happen to match.
///
/// They do today — `find_child_instance_ids` and the `children`/`has_children`
/// families both call `build_children_query` and `build_has_children_query`.
/// This pins it, because nothing else would notice a fork.
#[tokio::test(flavor = "current_thread")]
async fn deriving_and_constraining_agree_on_the_same_relation() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_CONTAINMENT).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);
    let identity = CompositeFilter::and(vec![]);

    // The module: it both refers to things and contains them, so neither half
    // of the relation is vacuous here.
    let module = index
        .find_symbol(
            &CompositeFilter::leaf(CompoundNameMixin::new("testmodule")),
            ScopeContext::Unscoped,
            ScopeContext::Unscoped,
            FamilyDemands::default(),
            &eph,
        )
        .await?
        .into_inner();
    let source_ids: Vec<i64> = module.nodes.iter().map(|n| n.symbol_instance.id).collect();
    assert!(!source_ids.is_empty(), "premise: the module was selected");

    // What CONSTRAIN would narrow by: the child instances named in the families.
    let mut from_families: Vec<i64> = module
        .children
        .iter()
        .map(|c| c.symbol_instance.id)
        .chain(module.has_children.iter().map(|h| h.child_instance.id))
        .collect();
    from_families.sort_unstable();
    from_families.dedup();
    assert!(
        !from_families.is_empty(),
        "premise: the families are non-empty, or this compares two empty sets"
    );

    // What DERIVE would produce from the same sources.
    let (derived_ids, _capped) = index
        .find_child_instance_ids(&source_ids, true, true, &identity, &eph)
        .await?;
    let mut from_derive: Vec<i64> = derived_ids.into_iter().map(Into::into).collect();
    from_derive.sort_unstable();
    from_derive.dedup();

    assert_eq!(
        from_derive, from_families,
        "derive and constrain must see the same children, or the fixpoint \
         depends on notification order"
    );
    Ok(())
}

/// The safety cap bounds a family read to `read_cap + 1`, always.
///
/// P1: no SQL read returns too much data, at any level. The escalation fixture
/// gives `lock` ten callers; with `read_cap = 3` the derive read of "parents of
/// lock" must come back with at most four rows — the cap plus the one sentinel
/// row that tells the caller it was truncated. Without the cap it returns all
/// ten. On the real index the same missing bound was a 1,100,906-row read.
#[tokio::test(flavor = "current_thread")]
async fn a_family_read_is_bounded_by_the_safety_cap() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_ESCALATION).await?;
    let mut eph = EphContext::rooted(index.load_root_layers().await?);
    let identity = CompositeFilter::and(vec![]);

    // `lock` (symbol 2) is defined at instance 3000 and has ten callers.
    let (uncapped, uncapped_flag) = index
        .find_parent_instance_ids(&[3000], true, false, &identity, &eph)
        .await?;
    assert!(
        uncapped.len() >= 10,
        "premise: lock has >= 10 callers uncapped, got {}",
        uncapped.len()
    );
    assert!(
        !uncapped_flag,
        "an unbounded read (read_cap defaults to 1000 here) saw every caller,          so it must not report truncation"
    );

    eph.set_read_cap(3);
    let (capped, capped_flag) = index
        .find_parent_instance_ids(&[3000], true, false, &identity, &eph)
        .await?;
    assert!(
        capped.len() <= 4,
        "read_cap = 3 must bound the read to cap + 1 = 4 rows, got {}",
        capped.len()
    );
    assert!(
        capped_flag,
        "the read hit the cap (ten callers, limit four), so it must report          truncation — this is the signal the derive path carries so a filtered          re-read cannot launder the incompleteness"
    );
    Ok(())
}

/// A containment family with no neighbour statement to consume it is not
/// fetched at all.
///
/// `Absent` says there is no statement on that side, so nothing can read the
/// family: `constrain_by_*` needs a neighbour to constrain against, and the
/// graph's containment now comes from `find_has_edges_between` over the
/// answer's own nodes.  On the real index this was 2.5 s and 178,807 rows for
/// bare `mod("amdgpu")`, all of it discarded.
///
/// The same query under `Unscoped` still returns the rows, so this pins the
/// gate rather than the absence of containment in the fixture.
#[tokio::test(flavor = "current_thread")]
async fn containment_family_is_not_fetched_without_a_neighbour() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_CONTAINMENT).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);
    let module = CompositeFilter::leaf(CompoundNameMixin::new("testmodule"));

    let unscoped = index
        .find_symbol(
            &module,
            ScopeContext::Unscoped,
            ScopeContext::Unscoped,
            FamilyDemands::default(),
            &eph,
        )
        .await?
        .into_inner();
    assert!(
        !unscoped.has_children.is_empty(),
        "premise: the module does contain things, so `Unscoped` must fetch them"
    );

    let absent = index
        .find_symbol(
            &module,
            ScopeContext::Absent,
            ScopeContext::Absent,
            FamilyDemands::default(),
            &eph,
        )
        .await?
        .into_inner();
    assert_eq!(
        absent.nodes.len(),
        unscoped.nodes.len(),
        "gating a family must not change which symbols are selected"
    );
    assert!(
        absent.has_children.is_empty(),
        "no neighbour statement on the children side, so the family has no \
         reader and must not be fetched; got {} rows",
        absent.has_children.len()
    );
    assert!(
        absent.has_parents.is_empty(),
        "likewise on the parents side; got {} rows",
        absent.has_parents.len()
    );
    Ok(())
}

/// The target-side pruning must keep the LOWEST ids, because that is the
/// winner `ORDER BY ..., to_inst.id` picks.
///
/// `rec` has three instances (10, 20, 30) and `caller` — which is not a `rec`
/// — refers to it, so nothing is removed by the self-exclusion and the edge
/// must resolve to instance 10.  Pruning that kept the two HIGHEST instead
/// would answer 20 and still look plausible; with only two instances of the
/// target symbol the order is unobservable, which is why the fixture has
/// three.  This is the second leg of the `rn <= 2` argument (the first,
/// self-exclusion, is `recursive_edge_survives_when_the_lowest_target_is_the_caller`).
#[tokio::test(flavor = "current_thread")]
async fn target_pruning_keeps_the_lowest_instances() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index
        .load_test_input(Index::TEST_INPUT_RECURSIVE_MULTI_INSTANCE)
        .await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    let edges = index.find_edges_between(&[10, 20, 30, 40], &eph).await?.0;

    let from_caller: Vec<i64> = edges
        .iter()
        .filter(|e| e.from_instance_id == 40)
        .map(|e| e.to_instance_id)
        .collect();
    assert_eq!(
        from_caller,
        vec![10],
        "caller is not a `rec`, so nothing is excluded and the edge takes the          lowest instance; got {:?}",
        edges
            .iter()
            .map(|e| (e.from_instance_id, e.to_instance_id))
            .collect::<Vec<_>>()
    );

    // The self-call still resolves past its own instance, now with a third
    // candidate present.
    let from_rec: Vec<i64> = edges
        .iter()
        .filter(|e| e.from_instance_id == 10)
        .map(|e| e.to_instance_id)
        .collect();
    assert_eq!(
        from_rec,
        vec![20],
        "instance 10 is excluded as its own target, so the next lowest wins"
    );
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
            ScopeContext::Absent,
            FamilyDemands::default(),
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
                ScopeContext::Absent,
                ScopeContext::Absent,
                FamilyDemands::default(),
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

/// E1: the per-node out-edge cap bounds each source node's edge list to
/// `edge_cap`, records the truncated node, and is deterministic — assembly
/// capping, so it never touches soundness.  `hub` (4001) references four
/// targets, `spoke` (4002) one.
#[tokio::test(flavor = "current_thread")]
async fn a_per_node_edge_cap_bounds_out_edges_and_records_truncation() -> anyhow::Result<()> {
    use crate::db_diesel::ImplicitEdge;
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_ESCALATION).await?;
    let mut eph = EphContext::rooted(index.load_root_layers().await?);

    let candidates = [4001i64, 4002, 4101, 4102, 4103, 4104];
    let out_edges = |edges: &[ImplicitEdge], from: i64| -> usize {
        edges.iter().filter(|e| e.from_instance_id == from).count()
    };

    // Disabled (the default): hub keeps all four out-edges, spoke its one,
    // nothing truncated.
    let (full, trunc_off) = index.find_edges_between(&candidates, &eph).await?;
    assert_eq!(out_edges(&full, 4001), 4, "hub has four out-edges uncapped");
    assert_eq!(out_edges(&full, 4002), 1, "spoke has one");
    assert!(trunc_off.is_empty(), "cap disabled => nothing truncated");

    // Cap at two: hub keeps exactly two and is recorded truncated; spoke is
    // under the cap and is not.
    eph.set_edge_cap(2);
    let (capped, mut trunc) = index.find_edges_between(&candidates, &eph).await?;
    assert_eq!(out_edges(&capped, 4001), 2, "hub bounded to edge_cap = 2");
    assert_eq!(out_edges(&capped, 4002), 1, "spoke kept (under the cap)");
    trunc.sort_unstable();
    assert_eq!(trunc, vec![4001], "only hub hit the cap");

    // Deterministic: the same two edges survive on a second run (ORDER BY
    // ref_id under the window), so pagination is stable.
    let (capped2, _) = index.find_edges_between(&candidates, &eph).await?;
    let kept = |edges: &[ImplicitEdge], from: i64| -> Vec<i64> {
        let mut v: Vec<i64> = edges
            .iter()
            .filter(|e| e.from_instance_id == from)
            .map(|e| e.ref_id)
            .collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        kept(&capped, 4001),
        kept(&capped2, 4001),
        "the surviving edge set is deterministic across runs"
    );
    Ok(())
}

/// F2: the per-symbol references listing pages deterministically with a
/// keyset cursor, mirrors the engine's (from_instance, ref) row unit, and
/// includes references made from EXPANSION instances (macro sites are
/// ordinary instances).  `hub` (symbol 5) references t_1..t_4 from one
/// instance; `mac` (symbol 8) references t_1 from a definition AND an
/// expansion instance.
#[tokio::test(flavor = "current_thread")]
async fn symbol_references_page_with_a_cursor_and_cover_expansions() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_ESCALATION).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    // Page 1 of hub's references: two rows, more remain.
    let (p1, more1) = index.find_symbol_references(5, None, 2, &eph).await?;
    assert_eq!(p1.len(), 2, "limit bounds the page");
    assert!(more1, "hub has four references, so page one reports more");

    // Page 2 via the cursor: the remaining two, disjoint from page 1, done.
    let last = p1.last().unwrap();
    let cursor = Some((last.from_instance_id, last.ref_id));
    let (p2, more2) = index.find_symbol_references(5, cursor, 2, &eph).await?;
    assert_eq!(p2.len(), 2, "second page holds the rest");
    assert!(!more2, "four references, two pages, nothing after");
    let ids1: Vec<i64> = p1.iter().map(|r| r.ref_id).collect();
    let ids2: Vec<i64> = p2.iter().map(|r| r.ref_id).collect();
    assert!(
        ids1.iter().all(|id| !ids2.contains(id)),
        "pages must be disjoint; got {:?} then {:?}",
        ids1,
        ids2
    );
    let mut targets: Vec<i64> = p1.iter().chain(&p2).map(|r| r.to_symbol).collect();
    targets.sort_unstable();
    assert_eq!(
        targets,
        vec![501, 502, 503, 504],
        "all four targets, once each"
    );
    assert!(
        p1[0].path == "/hub.c" && p1[0].to_symbol_name.starts_with("t_"),
        "rows carry the call-site path and the target's name for rendering"
    );

    // Expansion coverage: mac's two references come from two different
    // instances (definition 5001 and expansion 5002) of the same symbol.
    let (mac, mac_more) = index.find_symbol_references(8, None, 10, &eph).await?;
    assert!(!mac_more);
    let mut from: Vec<i64> = mac.iter().map(|r| r.from_instance_id).collect();
    from.sort_unstable();
    assert_eq!(
        from,
        vec![5001, 5002],
        "one reference from the definition, one from the EXPANSION instance"
    );

    // Capped count: exact under the cap, `cap+` above it.
    assert_eq!(
        index.count_symbol_references(5, 10, &eph).await?,
        (4, false),
        "hub's four references, exactly"
    );
    assert_eq!(
        index.count_symbol_references(5, 2, &eph).await?,
        (2, true),
        "capped at two means '2+', not a total"
    );
    Ok(())
}

/// F2 has-kind: the contained-instances listing pages the same way.  `modx`
/// (symbol 1) contains the ten in_* function instances.
#[tokio::test(flavor = "current_thread")]
async fn symbol_contained_pages_with_a_cursor() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_ESCALATION).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    let (p1, more1) = index.find_symbol_contained(1, None, 6, &eph).await?;
    assert_eq!(p1.len(), 6);
    assert!(more1, "modx contains ten instances");
    let last = p1.last().unwrap();
    let (p2, more2) = index
        .find_symbol_contained(
            1,
            Some((last.parent_instance_id, last.child_instance_id)),
            6,
            &eph,
        )
        .await?;
    assert_eq!(p2.len(), 4, "the remaining four");
    assert!(!more2);
    let mut children: Vec<i64> = p1.iter().chain(&p2).map(|r| r.child_instance_id).collect();
    children.sort_unstable();
    children.dedup();
    assert_eq!(
        children.len(),
        10,
        "ten distinct contained instances, no repeats"
    );
    assert!(
        p2[0].child_symbol_name.starts_with("in_"),
        "rows carry the contained symbol's name"
    );
    Ok(())
}

/// A self-reference must not lie to pagination.  `selfy`'s first reference is
/// to itself — no other instance exists, so the row carries no target and is
/// excluded from any rendering — but it still occupies a scan position: with
/// `limit = 1` the first page must say more remains, and the cursor must walk
/// through to the real reference.  (Production bug: the target lookup was an
/// inner join, the self-ref row vanished BEFORE the `limit + 1` sentinel was
/// counted, and a page boundary landing on one reported `has_more = false`
/// with a thousand rows remaining.)
#[tokio::test(flavor = "current_thread")]
async fn a_self_reference_keeps_its_scan_position_in_pagination() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_ESCALATION).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    // Page 1 (limit 1) lands on the self-ref: target-less, but has_more MUST
    // be true — the real reference is still ahead.
    let (p1, more1) = index.find_symbol_references(9, None, 1, &eph).await?;
    assert_eq!(p1.len(), 1);
    assert_eq!(
        p1[0].to_symbol, 9,
        "first scan position is the self-reference"
    );
    assert_eq!(
        p1[0].to_instance_id, None,
        "a self-reference has no other target instance"
    );
    assert!(
        more1,
        "the real reference is behind the self-ref — has_more must say so"
    );

    // The cursor from the RAW row walks past it to the real reference.
    let cursor = Some((p1[0].from_instance_id, p1[0].ref_id));
    let (p2, more2) = index.find_symbol_references(9, cursor, 1, &eph).await?;
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].to_symbol, 501, "page two reaches t_1");
    assert!(p2[0].to_instance_id.is_some());
    assert!(!more2);
    Ok(())
}

/// F3: the per-symbol instance listing pages by instance-id keyset and the
/// count is EXACT (a per-symbol count is bounded by that symbol's own size —
/// unlike edge counts, which must stay capped).  `mac` has two instances
/// (definition 5001, expansion 5002).
#[tokio::test(flavor = "current_thread")]
async fn symbol_instances_page_with_a_cursor_and_count_exactly() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let docker = clients::Cli::default();
    let (_node, url) = start_postgres(&docker);
    wait_for_postgres(&url).await?;
    let mut index = Index::connect(&url).await?;
    index.load_test_input(Index::TEST_INPUT_ESCALATION).await?;
    let eph = EphContext::rooted(index.load_root_layers().await?);

    let (p1, more1) = index.list_symbol_instances(8, None, 1, &eph).await?;
    assert_eq!(p1.len(), 1);
    assert_eq!(p1[0].instance_id, 5001, "id order: the definition first");
    assert!(more1, "the expansion instance is still ahead");
    assert_eq!(p1[0].path, "/mac.c", "rows carry the path for rendering");

    let (p2, more2) = index
        .list_symbol_instances(8, Some(p1[0].instance_id), 1, &eph)
        .await?;
    assert_eq!(p2.len(), 1);
    assert_eq!(p2[0].instance_id, 5002);
    assert_eq!(p2[0].instance_type, 3, "the expansion instance, typed");
    assert!(!more2);

    assert_eq!(index.count_symbol_instances(8, &eph).await?, 2);
    // A symbol with no instances counts zero, pages empty.
    let (empty, more) = index.list_symbol_instances(999_999, None, 5, &eph).await?;
    assert!(empty.is_empty() && !more);
    assert_eq!(index.count_symbol_instances(999_999, &eph).await?, 0);
    Ok(())
}
