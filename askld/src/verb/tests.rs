use index::symbols::SymbolInstanceId;

use crate::{
    cfg::ControlFlowGraph, span::Span,
    test_util::{get_shared_index, run_query, run_query_err, VERB_TEST},
    verb::*,
};

use std::collections::HashMap;

#[tokio::test(flavor = "current_thread")]
async fn test_select_matching_name() {
    let index = get_shared_index(VERB_TEST).await;
    let cfg = ControlFlowGraph::from_symbols(index);

    let test_cases = vec![
        ("sort.Sort", vec![96]),
        ("sort.IsSorted", vec![95]),
        ("foo", vec![91]),
        ("bar", vec![92]),
        ("foo.bar", vec![92]),
        ("FOO.bar", vec![]),
        ("FOO", vec![]),
    ];

    for (name, expected_ids) in test_cases {
        let fake_span = Span::synthetic(name);
        let named_args = HashMap::from([("name".to_string(), name.to_string())]);
        let selector = NameSelector::new(fake_span, &vec![], &named_args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select_from_all_impl(&cfg, index::db_diesel::CompositeFilter::And(vec![]), index::db_diesel::ScopeContext::Skip, index::db_diesel::ScopeContext::Skip)
            .await
            .unwrap();

        let mut got_symbol_instances: Vec<SymbolInstanceId> = result
            .0.unwrap()
            .nodes
            .into_iter()
            .map(|s| SymbolInstanceId::new(s.symbol_instance.id))
            .collect();
        got_symbol_instances.sort();

        let expected_symbol_instances: Vec<SymbolInstanceId> = expected_ids
            .into_iter()
            .map(|i| SymbolInstanceId::new(i))
            .collect();

        assert_eq!(
            got_symbol_instances, expected_symbol_instances,
            "Failed for name: {}",
            name
        );
    }
}

#[test]
fn test_ignore_package_filter() {
    let query = r#"preamble {
    ignore(package="foo")
}
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_data_verb() {
    let res = run_query("verb_test.sql", r#"data "Debug";"#);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(97)]
    );
}

#[test]
fn test_data_verb_full_name() {
    let res = run_query("verb_test.sql", r#"data "config.Debug";"#);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(97)]
    );
}

#[test]
fn test_ignore_package_filter_inline() {
    // Single-line preamble still works (backward compat)
    let query = r#"preamble ignore(package="foo")
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_preamble_scope_multiple_ignores() {
    // Multiple ignore verbs in preamble scope
    let query = r#"preamble {
    ignore(package="foo")
    ignore(package="bar")
}
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_preamble_scope_with_semicolons() {
    // Semicolons still work as separators inside preamble scope
    let query = r#"preamble { ignore(package="foo") }
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_preamble_empty_scope() {
    // preamble with empty scope is a no-op — should not panic
    let query = r#"preamble {
}
"foo"
"tar"
"#;

    let res = run_query("verb_test.sql", query);
    assert!(!res.nodes.as_vec().is_empty());
}

// ============================================================================
// Ephemeral verb tests (PR 3)
// ============================================================================

#[test]
fn test_ephemeral_instance_roundtrip() {
    // ephemeral_instance creates a synthetic symbol + instance in the overlay;
    // it is returned via find_symbol with SymbolInstanceIdMixin.
    const EPH_SYM_ID: i64 = i64::MAX - 1;
    const EPH_INST_ID: i32 = i32::MAX - 1;

    let query = format!(
        r#"ephemeral_instance(symbol_id="{sym}", instance_id="{inst}", object_id="1", start="912", end="913", instance_type="1", name="eph_func", path="eph_func", project_id="1", symbol_type="1")"#,
        sym = EPH_SYM_ID,
        inst = EPH_INST_ID
    );
    let res = run_query(VERB_TEST, &query);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(EPH_INST_ID)]);
}

#[test]
fn test_ephemeral_symbol_no_instance() {
    // ephemeral_symbol adds a symbol to the overlay but no instance → empty selection.
    const EPH_SYM_ID: i64 = i64::MAX - 2;

    let query = format!(
        r#"ephemeral_symbol(symbol_id="{}", name="eph_sym", path="eph_sym", project_id="1", symbol_type="1")"#,
        EPH_SYM_ID
    );
    let res = run_query(VERB_TEST, &query);
    assert!(res.nodes.as_vec().is_empty());
}

#[test]
fn test_ephemeral_ref_no_selection() {
    // ephemeral_ref adds a ref to the overlay but no instance → empty selection.
    const EPH_REF_ID: i32 = i32::MAX - 2;

    let query = format!(
        r#"ephemeral_ref(ref_id="{}", to_symbol="1", from_object="1", start="912", end="913")"#,
        EPH_REF_ID
    );
    let res = run_query(VERB_TEST, &query);
    assert!(res.nodes.as_vec().is_empty());
}

#[test]
fn test_ephemeral_instance_containment_in_func() {
    // func has { ephemeral_instance(...) }
    // Ephemeral instance at [912, 913) is inside foo at [910, 919) in object_id=1.
    // Expected result: func instance 91 (foo) + the ephemeral instance itself.
    const EPH_SYM_ID: i64 = i64::MAX - 10;
    const EPH_INST_ID: i32 = i32::MAX - 10;

    let query = format!(
        r#"func has {{ ephemeral_instance(symbol_id="{sym}", instance_id="{inst}", object_id="1", start="912", end="913", instance_type="1", name="eph_func2", path="eph_func2", project_id="1", symbol_type="1") }}"#,
        sym = EPH_SYM_ID,
        inst = EPH_INST_ID
    );
    let res = run_query(VERB_TEST, &query);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "expected foo instance 91, got {:?}",
        nodes
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(EPH_INST_ID)),
        "expected ephemeral instance {EPH_INST_ID}, got {:?}",
        nodes
    );
}

#[test]
fn test_ephemeral_instance_id_out_of_range() {
    // ephemeral_instance with an ID outside the ephemeral range should fail at parse/construction.
    let query = r#"ephemeral_instance(symbol_id="1", instance_id="1", object_id="1", start="0", end="1", instance_type="1", name="x", path="x", project_id="1", symbol_type="1")"#;
    let err = run_query_err(VERB_TEST, query);
    assert!(err.is_err(), "expected error for non-ephemeral IDs");
}
