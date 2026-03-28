use crate::test_util::{format_edges, run_query, run_query_err, TEST_INPUT_A};
use index::symbols::SymbolInstanceId;

#[test]
fn label_use_syntax_check() {
    const QUERY: &str = r#""b" "a" {label("foo")}; use("foo")"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92),]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn label_use() {
    const QUERY: &str = r#"label("foo") "a"; use("foo") {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92),]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn label_use_shortcut_syntax() {
    // Shortcut syntax: @foo = label("foo"), #foo = use("foo")
    const QUERY: &str = r#"@foo "a"; #foo {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should behave identically to label("foo") "a"; use("foo") {}
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92),]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn label_use_with_selector() {
    const QUERY: &str = r#"label("foo") "a"; use("foo") "d" {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(94),
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(96),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "94-95", "94-96"]);
}

#[test]
fn label_use_with_selector_2() {
    const QUERY: &str = r#"label("foo") "a"; "d" use("foo")  {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(94),
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(96),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "94-95", "94-96"]);
}

#[test]
fn multiple_label_use() {
    const QUERY: &str = r#"label("main") "main"; label("b") "b"; use("main"){{use("b")}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(942),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "942-91", "942-92"]);
}

#[test]
fn label_use_forced() {
    const QUERY: &str = r#""main" label("foo") {}; "b" {use("foo", forced="true")}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(942)
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "92-942", "942-91", "942-92"]);
}

#[test]
fn three_statement_label_use_loop_returns_empty() {
    const QUERY: &str = r#"
        "a" label("alpha") use("gamma");
        "b" label("beta") use("alpha");
        "c" label("gamma") use("beta")
    "#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn mutual_label_use_loop_returns_empty() {
    const QUERY: &str = r#"
        "a" label("alpha") use("beta");
        "b" label("beta") use("alpha")
    "#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn nested_label_use_loop_returns_no_results() {
    const QUERY: &str = r#"
        "a" label("outer") {
            "b" label("branch") use("leaf");
            "c" label("leaf") use("outer");
            use("branch")
        }
    "#;
    let result = run_query_err(TEST_INPUT_A, QUERY);

    assert!(result.is_err());
}

#[test]
fn sibling_label_use_loop_returns_no_results() {
    const QUERY: &str = r#"
        "root" {
            "a" label("left") use("right");
            "b" label("right") use("left")
        }
    "#;
    let result = run_query_err(TEST_INPUT_A, QUERY);

    assert!(result.is_err());
}

#[test]
fn label_use_reports_error_instead_of_panic() {
    const QUERY: &str = r#"
        label("A") "main" {
        };
        {use("a")}
    "#;

    let result = run_query_err(TEST_INPUT_A, QUERY);

    assert!(result.is_err());
}

#[test]
fn forced_label_use_loop_returns_empty() {
    const QUERY: &str = r#"
        "a" label("foo") use("bar", forced="true");
        "b" label("bar") use("foo", forced="true")
    "#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert!(res.nodes.as_vec().is_empty());
    let edges = format_edges(res.edges);
    assert!(edges.is_empty());
}
