use index::symbols::SymbolInstanceId;

use crate::{
    cfg::ControlFlowGraph, span::Span,
    test_util::{format_edges, get_shared_index, run_query, run_query_err, VERB_TEST},
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
    // ephemeral_instance creates an instance in the overlay referencing a
    // persistent symbol; it is returned via find_symbol with SymbolInstanceIdMixin.
    // Uses persistent symbol 1 (foo) from the verb_test fixture.
    const EPH_INST_ID: i32 = i32::MAX - 1;

    let query = format!(
        r#"ephemeral_instance(symbol_id="1", instance_id="{inst}", object_id="1", start="912", end="913", instance_type="1")"#,
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
    // References persistent symbol 1 (foo).
    // Expected result: func instance 91 (foo) + the ephemeral instance itself.
    const EPH_INST_ID: i32 = i32::MAX - 10;

    let query = format!(
        r#"func has {{ ephemeral_instance(symbol_id="1", instance_id="{inst}", object_id="1", start="912", end="913", instance_type="1") }}"#,
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
    // ephemeral_instance with an instance_id outside the ephemeral range should fail.
    // symbol_id="1" is a valid persistent symbol; instance_id="1" is out of range.
    let query = r#"ephemeral_instance(symbol_id="1", instance_id="1", object_id="1", start="0", end="1", instance_type="1")"#;
    let err = run_query_err(VERB_TEST, query);
    assert!(err.is_err(), "expected error for non-ephemeral instance_id");
}

// Verify that find_edges_between actually uses the overlay (was previously
// ignoring it). Two ephemeral instances (of persistent symbols) + one ephemeral
// ref between them: the edge should appear in ExecutionResult.edges after the fix.
#[test]
fn test_ephemeral_ref_edge_discovery() {
    const EPH_INST_X: i32 = i32::MAX - 30;
    const EPH_INST_Y: i32 = i32::MAX - 31;
    const EPH_REF_R: i32 = i32::MAX - 32;

    // EPH_INST_X: symbol=1 (foo), range=[800, 810) in object_id=1
    // EPH_INST_Y: symbol=2 (foo.bar), range=[810, 820) in object_id=1
    // EPH_REF_R:  to_symbol=2, from_object=1, from_range=[802, 803)
    //             [802, 803) ⊂ [800, 810) → edge X→Y
    let query = format!(
        "ephemeral_instance(symbol_id=\"1\", instance_id=\"{inst_x}\", object_id=\"1\", start=\"800\", end=\"810\", instance_type=\"1\")\n\
         ephemeral_instance(symbol_id=\"2\", instance_id=\"{inst_y}\", object_id=\"1\", start=\"810\", end=\"820\", instance_type=\"1\")\n\
         ephemeral_ref(ref_id=\"{ref_r}\", to_symbol=\"2\", from_object=\"1\", start=\"802\", end=\"803\")\n",
        inst_x = EPH_INST_X,
        inst_y = EPH_INST_Y,
        ref_r = EPH_REF_R,
    );

    let res = run_query(VERB_TEST, &query);
    let edges = format_edges(res.edges);
    let expected_edge = format!("{}-{}", EPH_INST_X, EPH_INST_Y);
    assert!(
        edges.contains(&expected_edge),
        "expected ephemeral edge {} in edges, got {:?}",
        expected_edge,
        edges
    );
}

// ============================================================================
// loc verb tests (PR 4)
// ============================================================================

#[test]
fn test_loc_returns_synthetic_instance() {
    // loc("main.c", "3"): line 3 in verb_test.sql fixture is bytes [910, 919),
    // which exactly spans function foo's instance (id=91, range [910, 919)).
    // Verifies that loc creates an ephemeral instance visible in query results.
    let res = run_query(VERB_TEST, r#"loc("main.c", "3")"#);
    // Result should contain exactly one node (the synthetic loc instance).
    assert_eq!(
        res.nodes.as_vec().len(),
        1,
        "expected 1 loc node, got {:?}",
        res.nodes.as_vec()
    );
}

#[test]
fn test_func_has_loc() {
    // func has { loc("main.c", "3") }
    // Line 3 is [910, 919) which is exactly foo's offset range [910, 919).
    // Expected: func instance 91 (foo) appears in the result, and the loc instance too.
    let res = run_query(VERB_TEST, r#"func has { loc("main.c", "3") }"#);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "expected foo instance 91 in func has {{ loc }}, got {:?}",
        nodes
    );
}

#[test]
fn test_loc_nonexistent_file() {
    // loc for a file that doesn't exist → empty result, no panic.
    let res = run_query(VERB_TEST, r#"loc("nonexistent_xyz.c", "1")"#);
    assert!(
        res.nodes.as_vec().is_empty(),
        "expected empty result for nonexistent file, got {:?}",
        res.nodes.as_vec()
    );
}

#[test]
fn test_loc_line_out_of_range() {
    // loc with a line number beyond the file → empty result, no panic.
    let res = run_query(VERB_TEST, r#"loc("main.c", "999")"#);
    assert!(
        res.nodes.as_vec().is_empty(),
        "expected empty result for out-of-range line, got {:?}",
        res.nodes.as_vec()
    );
}

// ============================================================================
// FK validation tests — non-existent symbol/ref targets must error
// ============================================================================

#[test]
fn test_ephemeral_instance_nonexistent_symbol() {
    // symbol_id 999999 does not exist in the verb_test fixture → error.
    const EPH_INST_ID: i32 = i32::MAX - 40;
    let query = format!(
        r#"ephemeral_instance(symbol_id="999999", instance_id="{inst}", object_id="1", start="0", end="1", instance_type="1")"#,
        inst = EPH_INST_ID
    );
    let err = run_query_err(VERB_TEST, &query);
    assert!(err.is_err(), "expected error for non-existent symbol_id");
}

#[test]
fn test_ephemeral_ref_nonexistent_to_symbol() {
    // to_symbol 999999 does not exist in the verb_test fixture → error.
    const EPH_INST_X: i32 = i32::MAX - 41;
    const EPH_REF_R: i32 = i32::MAX - 42;
    let query = format!(
        "ephemeral_instance(symbol_id=\"1\", instance_id=\"{inst_x}\", object_id=\"1\", start=\"800\", end=\"810\", instance_type=\"1\")\n\
         ephemeral_ref(ref_id=\"{ref_r}\", to_symbol=\"999999\", from_object=\"1\", start=\"802\", end=\"803\")\n",
        inst_x = EPH_INST_X,
        ref_r = EPH_REF_R,
    );
    let err = run_query_err(VERB_TEST, &query);
    assert!(err.is_err(), "expected error for non-existent to_symbol");
}
