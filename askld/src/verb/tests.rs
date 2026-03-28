use index::symbols::SymbolInstanceId;

use crate::{
    cfg::ControlFlowGraph, execution_context::ExecutionContext, span::Span,
    test_util::{get_shared_index, run_query, VERB_TEST},
    verb::*,
};

use std::collections::HashMap;

#[tokio::test(flavor = "current_thread")]
async fn test_select_matching_name() {
    let index = get_shared_index(VERB_TEST);
    let cfg = ControlFlowGraph::from_symbols(index);

    let test_cases = vec![
        ("sort.Sort", vec![96]),
        ("sort.IsSorted", vec![95]),
        ("foo", vec![91, 92]),
        ("bar", vec![92]),
        ("foo.bar", vec![92]),
        ("FOO.bar", vec![]),
        ("FOO", vec![]),
    ];

    let mut ctx = ExecutionContext::new(); // Assuming there's a default constructor

    for (name, expected_ids) in test_cases {
        let fake_span = Span::synthetic(name);
        let named_args = HashMap::from([("name".to_string(), name.to_string())]);
        let selector = NameSelector::new(fake_span, &vec![], &named_args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select_from_all(&mut ctx, &cfg, vec![])
            .await
            .unwrap();

        let mut got_symbol_instances: Vec<SymbolInstanceId> = result
            .unwrap()
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
