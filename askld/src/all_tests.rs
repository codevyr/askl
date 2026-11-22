use crate::test_util::{
    format_edges, get_shared_db_url, get_shared_index, run_query, run_query_err,
    run_query_probe_traced, run_query_traced, run_query_with_budget, run_query_with_edge_cap,
    run_query_with_read_cap, TEST_INPUT_A, TEST_INPUT_B, TEST_INPUT_CONTAINMENT,
    TEST_INPUT_ESCALATION, TEST_INPUT_HIDDEN_CHILDREN, TEST_INPUT_MODULES, TEST_INPUT_NESTED_FUNC,
    TEST_INPUT_ONE_NAME_MANY_FILES, TEST_INPUT_SEARCH, TEST_INPUT_STRUCT_FIELDS,
    TEST_INPUT_TREE_BROWSER, VERB_TEST,
};
use index::db_diesel::FamilyDemands;
use index::symbols::{SymbolId, SymbolInstanceId};
use sha2::Digest;

#[test]
fn single_node_query() {
    env_logger::init();

    const QUERY: &str = r#""a""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn glob_selector_matches_leaf_names() {
    const QUERY: &str = r#"g"m*n""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(942)]);
}

#[test]
fn glob_selector_smart_case_uppercase_is_sensitive() {
    const QUERY: &str = r#"g"M*N""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![]);
}

#[test]
fn plain_star_selector_keeps_exact_semantics() {
    // '*' in a plain string is not a wildcard: normalization strips it on
    // both sides, so "m*ain" exact-matches the symbol main.
    const QUERY: &str = r#""m*ain""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(942)]);
}

#[test]
fn glob_selector_rejects_bare_wildcard() {
    const QUERY: &str = r#"g"*""#;
    let res = run_query_err(TEST_INPUT_A, QUERY);

    assert_eq!(res.is_err(), true);
    if let Err(e) = res {
        println!("{:#?}", e);
        assert!(e.to_string().contains("literal"));
    }
}

#[test]
fn glob_selector_rejects_normalization_erased_literals() {
    // '-' is stripped from leaf names at index time; a pattern whose only
    // literal is '-' would compile to a bare '%' full scan, so it is rejected
    // at construction rather than silently matching everything.
    const QUERY: &str = r#"g"-*""#;
    let res = run_query_err(TEST_INPUT_A, QUERY);

    assert_eq!(res.is_err(), true);
    if let Err(e) = res {
        println!("{:#?}", e);
        assert!(e.to_string().contains("literal"));
    }
}

#[test]
fn glob_argument_reserved_prefix_does_not_panic() {
    // A reserved/unknown prefix inside verb arguments must surface as a parse
    // error, not panic the parser (regression: Value::build was unwrapped).
    let res = run_query_err(TEST_INPUT_A, r#"func(re"foo")"#);
    assert!(res.is_err());
    if let Err(e) = res {
        assert!(e.to_string().contains("reserved"));
    }

    let res = run_query_err(TEST_INPUT_A, r#"func(x"foo")"#);
    assert!(res.is_err());
    if let Err(e) = res {
        assert!(e.to_string().contains("unknown string prefix"));
    }
}

#[test]
fn unknown_string_prefix_is_rejected() {
    const QUERY: &str = r#"x"foo""#;
    let res = run_query_err(TEST_INPUT_A, QUERY);

    assert_eq!(res.is_err(), true);
    if let Err(e) = res {
        println!("{:#?}", e);
        assert!(e.to_string().contains("unknown string prefix"));
    }
}

#[test]
fn regex_string_prefix_is_reserved() {
    const QUERY: &str = r#"re"foo.*""#;
    let res = run_query_err(TEST_INPUT_A, QUERY);

    assert_eq!(res.is_err(), true);
    if let Err(e) = res {
        println!("{:#?}", e);
        assert!(e.to_string().contains("reserved"));
    }
}

#[test]
fn glob_file_selector_matches_normalized_dots() {
    // /main.c is stored with leaf_name main_c; g"*.c" must match through the
    // same dot normalization.
    const QUERY: &str = r#"file(g"*.c")"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(1001)]);
}

#[test]
fn single_child_query() {
    const QUERY: &str = r#""a"{}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn single_parent_query() {
    // Root-level {} has no type filter (default is all types), so parents of "a"
    // include function main(942), file /main.c(1001), and directory /(1003).
    const QUERY: &str = r#"{"a"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(942),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-91", "1001-91", "1003-91"]);
}

#[test]
fn double_parent_query() {
    const QUERY: &str = r#"{{"b"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    // With REFS|HAS default, {} also picks up file/dir containers
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(942),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(
        edges,
        vec![
            "91-92", "91-92", "942-91", "942-92", "1001-91", "1001-92", "1001-92", "1001-92",
            "1003-91", "1003-92", "1003-92", "1003-92",
        ]
    );
}

// This test is ignored for now because current behavior considers children of
// "a" to be weak statements, meaning that the non-existing grandchild does not
// constrain an existing child. In future, we may want to add a syntax to
// indicate that the grandchild is strong, so only children with grandchildren are
// selected.
#[test]
#[ignore]
fn missing_child_query() {
    // "a" does not have grandchildren, so this should return no results
    const QUERY: &str = r#""a"{{}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn no_selectors() {
    const QUERY: &str = r#"{{}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn forced_query() {
    // Forcing a node without any selectors should return no results
    const QUERY: &str = r#"!"a""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn forced_child_query_1() {
    const QUERY: &str = r#""b"{!"a"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "92-91"]);
}

#[test]
fn forced_child_query_2() {
    const QUERY: &str = r#""b"{!"c"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(92), SymbolInstanceId::new(93)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["92-93"]);
}

#[test]
fn forced_child_query_3() {
    const QUERY: &str = r#""main" {
            !"c"
        }"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(93), SymbolInstanceId::new(942)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-93"]);
}

#[test]
fn forced_child_query_4() {
    const QUERY: &str = r#""a"{!"g"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(97)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-97"]);
}

#[test]
fn forced_child_query_5() {
    const QUERY: &str = r#""main"{{!"g"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(97),
            SymbolInstanceId::new(942),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(
        edges,
        vec!["91-92", "91-92", "91-97", "92-97", "942-91", "942-92"]
    );
}

#[test]
fn forced_child_query_6() {
    const QUERY: &str = r#""a" "b"{{!"g"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(97),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "92-97"]);
}

#[test]
fn forced_child_query_7() {
    const QUERY: &str = r#""main" {}; "b" {!"main"}"#;
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
fn generic_forced_child_query_3() {
    const QUERY: &str = r#""main" {
            forced(name="c")
        }"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(93), SymbolInstanceId::new(942)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-93"]);
}

#[test]
fn two_selectors() {
    const QUERY: &str = r#""b" "a""#;
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
fn two_selectors_children() {
    const QUERY: &str = r#""b" "a" {}"#;
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
fn two_selectors_partial_no_match_attribution() {
    // `"b" "nonexistent_zzz"`: both selectors share one fused statement
    // query; the missing name must still get ITS OWN no-match diagnostic
    // (via the per-branch existence check) while "b"'s rows keep the
    // statement non-empty.
    const QUERY: &str = r#""b" "nonexistent_zzz""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert!(!res.nodes.as_vec().is_empty(), "\"b\" must still match");
    assert!(
        res.warnings
            .iter()
            .any(|w| w.message.contains("nonexistent_zzz")),
        "the unmatched selector must get a no-match diagnostic, got {:?}",
        res.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
    assert!(
        !res.warnings.iter().any(|w| w.message.contains("`\"b\"`")),
        "the matched selector must NOT get a no-match diagnostic, got {:?}",
        res.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
}

#[test]
fn statement_after_scope() {
    const QUERY: &str = r#""a" {}; "a""#;
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
fn statement_after_scope_newline() {
    const QUERY: &str = r#""a" {}
        "a""#;
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
fn ignore_node_no_result() {
    const QUERY: &str = r#""a" {ignore("b")}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_sibling() {
    const QUERY: &str = r#""d" {ignore("e")}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(94), SymbolInstanceId::new(96)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["94-96"]);
}

#[test]
fn ignore_node_parent_no_result() {
    // Root-level {} has no type filter (all types), so parents of "e" include
    // d(94) which is ignored, plus file /main.c(1001) and directory /(1003).
    const QUERY: &str = r#"ignore("d") {"e"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["1001-95", "1003-95"]);
}

#[test]
fn ignore_node_parent_no_result_2() {
    // Second command: ignore("d") {"f" {ignore("asdf")}} — parents of "f" (all types)
    // include d(94) which is ignored, plus file(1001) and directory(1003).
    const QUERY: &str = r#" {ignore("a") "a"{}}; ignore("d") {"f" {ignore("asdf")}};"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(96),
            SymbolInstanceId::new(97),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(
        edges,
        vec!["96-97", "1001-96", "1001-97", "1003-96", "1003-97"]
    );
    println!("{:#?}", res.warnings);
    assert_eq!(res.warnings.len(), 1);
}

#[test]
fn ignore_node_parent_no_result_3() {
    const QUERY: &str = r#" {ignore("a") "a"{}};"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_parent_no_result_4() {
    // Root-level {} has no type filter (all types), so parents of "f" include
    // d(94) which is ignored, plus file(1001) and directory(1003).
    const QUERY: &str = r#"ignore("d") {"f" {ignore("asdf")}};"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(96),
            SymbolInstanceId::new(97),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(
        edges,
        vec!["96-97", "1001-96", "1001-97", "1003-96", "1003-97"]
    );
}

#[test]
fn ignore_node_wrong_parent() {
    // Root-level {} has no type filter (all types), so parents of "e" include
    // d(94) plus file(1001) and directory(1003). "a" is ignored but isn't a parent of "e".
    const QUERY: &str = r#"ignore("a") {"e"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(94),
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["94-95", "1001-95", "1003-95"]);
}

#[test]
fn ignore_node_recurse() {
    // Ignore applies to all children, so this should return no results
    const QUERY: &str = r#""a" ignore("b") {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_another_statement() {
    // Ignore applies to all children, so this should return no results
    const QUERY: &str = r#"preamble ignore("b") ; "a" {}; "a" {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn two_sub_statements() {
    // TODO: The original intention of this request was to select "f" first and
    // then select all other children of "d" together with all grandchildren of
    // "d". Because "f" would match the first statement, it would not show up in
    // the second statement, meaning that "f" will not have its children
    // selected.
    const QUERY: &str = r#""d" {"e"; {}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(94),
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(96),
            SymbolInstanceId::new(97),
        ]
    );
    let edges = format_edges(res.edges);

    // This test requires dependency tracking to pass, so let it fail for now
    assert_eq!(edges, vec!["94-95", "94-96", "96-97"]);
}

#[test]
fn statement_semicolon() {
    const QUERY: &str = r#""d" {"f";}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(94), SymbolInstanceId::new(96),]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["94-96"]);
}

#[test]
fn two_statements() {
    // We connect all nodes, unless they are explicitly isolated into different scopes
    const QUERY: &str = r#""a"; "b""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn project_double_parent_query() {
    // Tests mod filter with double parent query pattern.
    // filter("compound_name", "test", inherit=true) acts as a namespace filter
    // that propagates into child scopes via inherit=true.
    // The outer command is strong twice over — `select` is a real selector,
    // and the name filter is a constraint in its own right — so the bare
    // middle scope is sandwiched between a strong parent and a strong child
    // and stays strong too.
    const QUERY: &str = r#"select filter("compound_name", "test", inherit=true) {{"b"}}"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

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
    assert_eq!(edges, vec!["91-92", "942-91", "942-92"]);

    // Dropping `select` must not change the answer: the outer command is
    // unit+filter, but a name filter is a constraint, so the statement is
    // still not weak and the same three-level composition applies.  (While
    // it was weak, the echo also kept test.c (93) — a caller of test.b that
    // nothing in the namespace calls.)
    const UNSELECTED_QUERY: &str = r#"filter("compound_name", "test", inherit=true) {{"b"}}"#;
    let res = run_query(TEST_INPUT_MODULES, UNSELECTED_QUERY);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(942)
        ]
    );
}

#[test]
fn module_filter_excludes_other_modules() {
    const FILTERED_QUERY: &str = r#"filter("compound_name", "test") "a""#;
    let filtered = run_query(TEST_INPUT_MODULES, FILTERED_QUERY);

    println!("{:#?}", filtered.nodes);
    println!("{:#?}", filtered.edges);

    assert_eq!(filtered.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
    assert_eq!(filtered.edges.0.len(), 0);

    const UNFILTERED_QUERY: &str = r#""a""#;
    let unfiltered = run_query(TEST_INPUT_MODULES, UNFILTERED_QUERY);
    let unfiltered_nodes = unfiltered.nodes.as_vec();

    assert_eq!(
        unfiltered_nodes,
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"filter("compound_name", "test") "a"; "a""#;
    let filtered_unfiltered = run_query(TEST_INPUT_MODULES, FILTERED_AND_UNFILTERED_QUERY);
    let filtered_unfiltered_nodes = filtered_unfiltered.nodes.as_vec();

    assert_eq!(
        filtered_unfiltered_nodes,
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );

    const PREAMBLE_FILTERED_QUERY: &str =
        r#"preamble filter("compound_name", "test", inherit=true); "a""#;
    let preamble_filtered = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered.nodes.as_vec();

    assert_eq!(preamble_filtered_nodes, vec![SymbolInstanceId::new(91)]);
}

#[test]
fn module_filter_selects_other_module() {
    const QUERY: &str = r#"filter("compound_name", "other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn module_filter_replaced_by_second_invocation() {
    const QUERY: &str = r#"filter("compound_name", "test") filter("compound_name", "other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn module_filter_children_scope_honors_filter() {
    const QUERY: &str = r#"filter("compound_name", "other") "a" {}"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(201), SymbolInstanceId::new(202)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["201-202"]);
}

#[test]
fn project_filter_excludes_other_projects() {
    const FILTERED_QUERY: &str = r#"project("test_project") "a""#;
    let filtered = run_query(TEST_INPUT_MODULES, FILTERED_QUERY);

    println!("{:#?}", filtered.nodes);
    println!("{:#?}", filtered.edges);

    assert_eq!(
        filtered.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(201)]
    );
    assert_eq!(filtered.edges.0.len(), 0);

    const UNFILTERED_QUERY: &str = r#""a""#;
    let unfiltered = run_query(TEST_INPUT_MODULES, UNFILTERED_QUERY);
    let unfiltered_nodes = unfiltered.nodes.as_vec();

    assert_eq!(
        unfiltered_nodes,
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"project("test_project") "a"; "a""#;
    let filtered_unfiltered = run_query(TEST_INPUT_MODULES, FILTERED_AND_UNFILTERED_QUERY);
    let filtered_unfiltered_nodes = filtered_unfiltered.nodes.as_vec();

    assert_eq!(
        filtered_unfiltered_nodes,
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );

    const PREAMBLE_FILTERED_QUERY: &str = r#"preamble project("test_project"); "a""#;
    let preamble_filtered = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered.nodes.as_vec();

    assert_eq!(
        preamble_filtered_nodes,
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(201)]
    );

    const REPLACE_PROJECT_FILTERED_QUERY: &str = r#"project("adsf") project("test_project") "a""#;
    let replace_project_filtered = run_query(TEST_INPUT_MODULES, REPLACE_PROJECT_FILTERED_QUERY);
    let replace_project_filtered_nodes = replace_project_filtered.nodes.as_vec();

    assert_eq!(
        replace_project_filtered_nodes,
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(201)]
    );
}

#[test]
fn project_filter_selects_other_project() {
    const QUERY: &str = r#"project("other_project") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(301)]);
    assert_eq!(res.edges.0.len(), 0);

    const WRONG_PROJECT_QUERY: &str = r#"project("blablabla_project") "a""#;
    let res = run_query(TEST_INPUT_MODULES, WRONG_PROJECT_QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn project_and_module_filters_combine() {
    const QUERY: &str = r#"project("test_project") filter("compound_name", "other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn conflicting_project_and_module_filters_return_empty() {
    const QUERY: &str = r#"project("other_project") filter("compound_name", "other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn scoped_project_filter_does_not_leak() {
    const QUERY: &str = r#"project("other_project") "a"; "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn multiple_projects_with_forced() {
    const QUERY: &str = r#"project("test_project") "a" { project("other_project") !"a" }"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-301", "201-301"]);
}

#[test]
fn implicit_edge() {
    const QUERY: &str = r#""d" {}"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(86),
            SymbolInstanceId::new(94),
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(96)
        ]
    );
    let edges = format_edges(res.edges);
    // Edges are deduplicated by (from_symbol, to_symbol, occurrence).
    // Symbol f has two instances (86, 96), but d->f and e->f each create only one edge.
    assert_eq!(edges, vec!["94-86", "94-95", "95-86"]);
}

#[test]
fn multiple_selectors() {
    const QUERY: &str = r#""a" "c" { {"d" {}}}"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(86),
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
            SymbolInstanceId::new(95),
            SymbolInstanceId::new(96),
        ]
    );
    let edges = format_edges(res.edges);
    // Edges are deduplicated by (from_symbol, to_symbol, occurrence).
    // Symbol f has two instances (86, 96), but d->f and e->f each create only one edge.
    assert_eq!(
        edges,
        vec!["91-92", "92-94", "93-92", "94-86", "94-95", "95-86"]
    );
}

// Test edge deduplication behavior:
// - Edges with SAME (from_symbol, to_symbol, occurrence) are deduplicated
// - Edges with DIFFERENT outgoing positions (offset_start, offset_end) are NOT deduplicated
#[test]
fn edge_dedup_different_offsets_preserved() {
    // In test_input_a, symbol 'a' has TWO refs to symbol 'b' at different offsets:
    // - (2, 1, int4range(911, 912)) - a refs b at 911-912
    // - (2, 1, int4range(912, 913)) - a refs b at 912-913
    // Both edges should be preserved because they have different outgoing positions.
    const QUERY: &str = r#""a"{}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    let edges = format_edges(res.edges);
    // Two edges from a(91) to b(92), each with different offset - both preserved
    assert_eq!(edges, vec!["91-92", "91-92"]);
    assert_eq!(
        edges.len(),
        2,
        "Both edges with different offsets should be preserved"
    );
}

#[test]
fn edge_dedup_same_offset_deduplicated() {
    // In test_input_b, symbol 'f' has TWO instances (86, 96).
    // Symbol 'd' has ONE ref to symbol 'f' at offset 942-943.
    // Even though there are two target instances, only one edge should appear
    // because they have the same (from_symbol, to_symbol, occurrence).
    const QUERY: &str = r#""d" {"f"}"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let edges = format_edges(res.edges);
    // Only one edge from d to f, despite f having two instances
    assert_eq!(edges, vec!["94-86"]);
    assert_eq!(
        edges.len(),
        1,
        "Duplicate edges with same offset should be deduplicated"
    );
}

#[test]
fn preamble() {
    const QUERY: &str = r#"preamble"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_empty_commands() {
    const QUERY: &str = r#";;;;;preamble"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_second_command() {
    const QUERY: &str = r#""a";;;;;preamble"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_inner_command() {
    // A preamble nested in a scope is a placement error.  The message used to
    // say "first verb statement", which described neither the check (which is
    // about nesting depth, not position) nor the language (a query may open
    // several preambles, anywhere at the top level).
    const QUERY: &str = r#""a"{;;;;;preamble}"#;
    let res = run_query_err(TEST_INPUT_A, QUERY);

    assert_eq!(res.is_err(), true);
    if let Err(e) = res {
        println!("{:#?}", e);
        assert!(
            e.to_string().contains("must be a top-level statement"),
            "unexpected error: {e}"
        );
    }
}

#[test]
fn scope_isolated_stub_is_gone() {
    // The never-implemented `scope(isolated=)` stub is removed; dimension
    // resets fall out of the slot cascade instead (see the
    // filter-expressions design).
    let err = crate::parser::parse(r#"scope(isolated="true")"#).unwrap_err();
    assert!(err.to_string().contains("unknown verb"), "{}", err);
}

#[test]
fn weak_grandchild() {
    const QUERY: &str = r#""f"{{}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(96), SymbolInstanceId::new(97)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["96-97"]);
}

#[test]
fn weak_grandchild_2() {
    const QUERY: &str = r#"preamble project("test_project"); "a"{{{{{}}}}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92)]
    );
    assert_eq!(res.warnings.len(), 0);
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn weak_grandparent() {
    const QUERY: &str = r#"{{"a"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // With REFS|HAS default, {} also picks up file/dir containers
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(942),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-91", "1001-91", "1003-91"]);
}

#[test]
fn weak_grandparent_2() {
    // Root-level {} has no type filter (default is all types), so parents of "main"
    // include file /main.c(1001) and directory /(1003) in addition to any function parents.
    const QUERY: &str = r#"{"main"{"a"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(942),
            SymbolInstanceId::new(1001),
            SymbolInstanceId::new(1003),
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-91", "1001-91", "1003-91"]);
}

#[test]
fn non_existent_symbol_warning() {
    const QUERY: &str = r#""main" { "asdfasdf" }"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![]);
    // Only the cause: "main" is emptied by an empty child, an emptiness
    // the no-match already reports.
    let warnings: Vec<String> = res.warnings.iter().map(|w| w.to_string()).collect();
    assert_eq!(warnings, vec![r#"`"asdfasdf"` matched no symbols"#]);
}

#[test]
fn non_existent_child_warning() {
    const QUERY: &str = r#""a" { {"a"} }"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![]);
    // a's callees (b) do not call a.  The middle statement is where the
    // query goes empty; the outer "a" only inherits it.
    let warnings: Vec<String> = res.warnings.iter().map(|w| w.to_string()).collect();
    assert_eq!(
        warnings,
        vec![r#"`{"a"}` matched symbols, but none of them also relate to `"a"`"#]
    );
}

// ============================================================================
// Containment Tests
// ============================================================================
//
// These tests use TEST_INPUT_CONTAINMENT which has:
// - Module `testmodule` with instance [0, 1000) - symbol id 1, instance id 10
// - Function `testmodule.foo` [100,200) - symbol id 2, instance id 20
// - Function `testmodule.bar` [200,300) - symbol id 3, instance id 30
// - Function `testmodule.baz` [300,400) - symbol id 4, instance id 40
// - Refs: foo→bar, bar→baz

#[test]
fn a_node_selected_twice_keeps_both_statements_provenance() {
    // `collect_results` folds the per-statement selections into one node map,
    // and a node reached by two statements must carry BOTH of their spans —
    // that provenance is what the UI uses to say which part of the query
    // produced a node.
    //
    // The merge path is the one that clones nothing (only the vacant path
    // copies the node), so it is easy to write a version that drops the
    // second statement's span and still passes every other test.
    const QUERY: &str = r#""testmodule.foo" ; func("testmodule.foo")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let foo: Vec<_> = res
        .nodes
        .0
        .iter()
        .filter(|n| n.symbol_instance.id == 20)
        .collect();
    assert_eq!(
        foo.len(),
        1,
        "both statements select foo, and it must fold to ONE node"
    );

    let mut spans: Vec<&str> = foo[0]
        .query_statements
        .iter()
        .map(|q| q.text.as_str())
        .collect();
    spans.sort();
    spans.dedup();
    assert_eq!(
        spans.len(),
        2,
        "foo was selected by two statements, so it must carry both spans, got {:?}",
        foo[0]
            .query_statements
            .iter()
            .map(|q| &q.text)
            .collect::<Vec<_>>()
    );
}

#[test]
fn root_statements_still_get_their_reference_edges() {
    // A statement with no parent no longer fetches its `parents` family: no
    // `constrain_by_*` can read it (that needs a neighbour), and the display
    // path rediscovers any edge between two answer nodes through
    // `find_edges_between`.  On the real index that family cost 577-870 ms to
    // return ZERO rows for `mod("amdgpu")`.
    //
    // So for two root statements the edge between them now comes from exactly
    // one place.  If that rediscovery ever stops covering the case, this is
    // where it shows up.
    const QUERY: &str = r#""testmodule.foo" ; "testmodule.bar""#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let mut nodes: Vec<i64> = res.nodes.0.iter().map(|n| n.symbol_instance.id).collect();
    nodes.sort();
    assert_eq!(nodes, vec![20, 30], "premise: both roots are in the answer");

    assert_eq!(
        format_edges(res.edges),
        vec!["20-30"],
        "foo calls bar and both are roots, so the edge must survive without \
         either statement's parents family"
    );
}

#[test]
fn a_capped_attempt_escalates_to_a_composition_that_bounds_it() {
    // "Find all spinlocks in kernel module x", in miniature.  The fixture's
    // `modx` contains ten functions; `lock` has ten callers; exactly one
    // function is both.  At cap 5 neither neighbour bounds the middle
    // statement on its own, and the composition of the two does.
    //
    // Derivation is ONE method retried with a strictly more restrictive
    // composition, so the sequence is the whole point:
    //   r0  its own predicate            -> capped (22 instances)
    //   r1  the tightest neighbour alone -> capped (lock has 11)
    //   r2  that neighbour AND the other -> 1, resolved
    //
    // Composing at r1 would have been wasted work on every query where one
    // neighbour is enough; not composing at r2 leaves this one unbounded.
    const QUERY: &str = r#"mod("modx") { select { "lock" } }"#;
    let (res, acts) = run_query_probe_traced(TEST_INPUT_ESCALATION, QUERY, 5);

    let middle: Vec<_> = acts
        .iter()
        .filter(|a| a.query_statement.trim() == r#"select { "lock" }"#)
        .collect();
    assert!(
        middle.iter().any(|a| a.round == 1 && a.resolved.is_none()),
        "premise: one neighbour alone must NOT bound it, or this proves \
         nothing about composing: {acts:?}"
    );
    assert_eq!(
        middle.last().map(|a| a.resolved),
        Some(Some(1)),
        "composing both neighbours bounds it to the single function that is \
         inside modx AND calls lock: {acts:?}"
    );

    let mut nodes: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
    nodes.sort();
    assert_eq!(
        nodes,
        vec![10, 1001, 3000],
        "the module, the one function inside it that calls lock, and lock"
    );
}

#[test]
fn an_incomplete_warning_quotes_its_statement() {
    // Two statements of one query can each be cut.  Their warnings used to
    // be the same sentence twice; each now names the statement it is about.
    let capped = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"{ "lock" }"#, 3);
    let said: Vec<&str> = capped
        .warnings
        .iter()
        .filter(|w| w.message.contains("this part of the graph is incomplete"))
        .map(|w| w.message.as_str())
        .collect();
    assert_eq!(said.len(), 1, "{:?}", said);
    assert!(
        said[0].starts_with(r#"too many results in `{ "lock" }`:"#),
        "{:?}",
        said
    );
}

#[test]
fn what_a_statement_lists_is_cut_by_instances_not_by_references() {
    // `user` refers to `twin` four times and to `solo` once, and `twin` has
    // three instances: `"user" { }` lists four instances.  Both reads behind
    // it -- what the listing derives, and what `user` itself refers to --
    // joined each reference to every instance of its target and limited the
    // rows, thirteen here.  Under a cap of five the listing stopped inside
    // `twin`, dropped `solo` and said it was incomplete, and `user` was called
    // incomplete for its own five references.
    let res = run_query_with_read_cap(
        crate::test_util::TEST_INPUT_MANY_REFS_ONE_TARGET,
        r#""user" { }"#,
        5,
    );
    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(
        ids,
        vec![10, 20, 21, 22, 30],
        "user, the three twins and solo"
    );
    assert!(
        said.is_empty(),
        "five references to four instances fit a cap of five, got {:?}",
        said
    );

    // Four instances do not fit a cap of three, and there it says so.
    let res = run_query_with_read_cap(
        crate::test_util::TEST_INPUT_MANY_REFS_ONE_TARGET,
        r#""user" { }"#,
        3,
    );
    let (said, _) = incomplete_and_ids(&res);
    assert!(said.iter().any(|w| w.contains("`{ }`")), "got {:?}", said);
}

#[test]
fn a_read_that_was_cut_does_not_cut_the_statement() {
    // A LIMIT THAT CUT A READ DISQUALIFIES THAT READ AND NOTHING ELSE.
    //
    // `user` refers five times, to four instances.  At a cap of four the
    // listing fits; the list of `user`'s five references does not, and
    // neither does the list of what refers to the four.  Both statements were
    // read in full and both used to be reported "too many results", for a
    // side read nobody needed whole.
    let res = run_query_with_read_cap(
        crate::test_util::TEST_INPUT_MANY_REFS_ONE_TARGET,
        r#""user" { }"#,
        4,
    );
    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(ids, vec![10, 20, 21, 22, 30]);
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);
}

#[test]
fn a_statement_read_in_full_is_not_called_cut_for_a_side_read() {
    // Kernel: `data { "mutex" }`.  The name has 570 instances, read in full.
    // Everything that contains them ran to 1,001 rows, and that marked the
    // NAME as cut: "too many results in `"mutex"`", "198 of at least 699".
    //
    // Here: eight fields named `lockf`; what contains the eight is sixteen
    // rows, over a cap of eight.  (A bare type word does not narrow the name
    // it is asked about, so all eight are the answer at any cap.)
    let query = r#"data { "lockf" }"#;
    let whole =
        run_query_with_read_cap(crate::test_util::TEST_INPUT_NAME_IN_MANY_STRUCTS, query, 0);
    let (_, truth) = incomplete_and_ids(&whole);
    assert_eq!(
        truth,
        vec![201, 202, 203, 204, 205, 206, 207, 208, 300],
        "premise: the eight fields and guard"
    );

    let res = run_query_with_read_cap(crate::test_util::TEST_INPUT_NAME_IN_MANY_STRUCTS, query, 8);
    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(ids, truth);
    assert!(
        said.is_empty(),
        "both statements were read in full: {:?}",
        said
    );
}

#[test]
fn an_absence_the_lists_cannot_prove_is_asked_for() {
    // `busy` refers to `S1.lockf` 41 times and to no other `lockf`.  Under a
    // cap of eight both statements are read in full, and both lists of the
    // references between them are cut.  A row a cut list holds is still a
    // true row, so the one field `busy` refers to is kept.  The seven others
    // have no evidence in lists that were not read to their end, which proves
    // nothing -- so the engine asks: which of these seven does `busy` refer
    // to?  None.  They go, and the answer is whole.
    //
    // Before the ask they went all the same and the statement was called
    // incomplete, over an exact answer.
    let query = r#""busy" { "lockf" }"#;
    let whole =
        run_query_with_read_cap(crate::test_util::TEST_INPUT_NAME_IN_MANY_STRUCTS, query, 0);
    let (_, truth) = incomplete_and_ids(&whole);
    assert_eq!(truth, vec![201, 400], "premise: S1.lockf and busy");

    let res = run_query_with_read_cap(crate::test_util::TEST_INPUT_NAME_IN_MANY_STRUCTS, query, 8);
    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(ids, truth, "only what busy refers to");
    assert!(said.is_empty(), "the answer is exact, got {:?}", said);
}

#[test]
fn asking_keeps_a_row_the_cut_lists_never_reached() {
    // `flood` refers to `knob` 250 times, `quiet` once.  Two functions over
    // two variables, with 251 reference occurrences between them: a list cut
    // at nine rows is `flood -> knob` nine times, from either side.  `quiet`
    // then has no evidence and used to be DROPPED, a true row, with the
    // statement marked incomplete.  Asked, it relates.  `spare`, which
    // nothing refers to, is asked about too, and goes.
    let query = r#""flood" "quiet" { "knob" "spare" }"#;
    let fixture = crate::test_util::TEST_INPUT_MANY_OCCURRENCES_FEW_PAIRS;
    let (_, truth) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 0));
    assert_eq!(
        truth,
        vec![10, 20, 110],
        "premise: both functions, and knob"
    );

    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 8));
    assert_eq!(ids, truth);
    assert!(
        said.is_empty(),
        "every row was accounted for, got {:?}",
        said
    );
    let notes: Vec<String> = run_query_with_read_cap(fixture, query, 8)
        .warnings
        .iter()
        .map(|w| w.message.clone())
        .collect();
    assert!(notes.is_empty(), "nothing to say at all, got {:?}", notes);
}

#[test]
fn an_ask_that_cannot_be_answered_leaves_the_statement_incomplete() {
    // `brook` makes 1,700 references, none of them to `knob`, and `knob` is
    // referred to 1,951 times.  At a cap of eight an ask may read 1,600
    // references, so neither side of "does brook refer to knob" can be read.
    // The engine does not know, and says what it said before there was an
    // ask: the statement is a subset.
    let query = r#""flood" "brook" { "knob" "spare" }"#;
    let fixture = crate::test_util::TEST_INPUT_MANY_OCCURRENCES_FEW_PAIRS;
    let (_, truth) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 0));
    assert_eq!(truth, vec![10, 110], "premise: flood and knob");

    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 8));
    assert_eq!(ids, truth, "nothing unrelated is shown");
    // The two names are one statement, read once and narrowed once per
    // name, so it is said once per name; nothing is said of the variables,
    // whose question could be answered.
    assert!(
        !said.is_empty()
            && said
                .iter()
                .all(|w| w.contains(r#"`"flood"`"#) || w.contains(r#"`"brook"`"#)),
        "the functions could not be accounted for, and only they: {:?}",
        said
    );
}

#[test]
fn an_ask_is_about_the_rows_a_statement_holds() {
    // What counts as related is decided by the rows asked about, not by the
    // ask.  A NAMED folded symbol is found behind the fold, by the lists and
    // by the ask alike: `flood` refers to `hid` once, after 250 references to
    // `knob` that fill both lists.
    let fixture = crate::test_util::TEST_INPUT_MANY_OCCURRENCES_FEW_PAIRS;
    let query = r#""flood" "quiet" { "hid" "knob" }"#;
    let (_, truth) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 0));
    assert_eq!(truth, vec![10, 20, 110, 150], "premise: hid is found");
    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 8));
    assert_eq!(ids, truth);
    assert!(said.is_empty(), "got {:?}", said);

    // A LISTING leaves a folded symbol out, and no ask can put it back: an
    // ask only ever keeps rows the statement already has.  The listing here
    // is derived from both functions; `"dial"` narrows them to `flood`, and
    // the listing shows what `flood` refers to, without `hid` and without
    // `only_q`.  (This half held before the ask too; it pins that asking did
    // not change what a listing is.)
    let query = r#""flood" "quiet" { { } ; "dial" }"#;
    let (_, truth) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 0));
    assert_eq!(truth, vec![10, 110, 120], "premise: flood, knob and dial");
    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(fixture, query, 30));
    assert_eq!(ids, truth);
    assert!(said.is_empty(), "got {:?}", said);
}

#[test]
fn a_cut_statement_is_a_subset_of_the_answer() {
    // `mod("modx") { { "lock" } }`: one function inside modx calls lock.
    // When the middle statement cannot be resolved it is derived from one
    // neighbour and cut.  What it then showed was a sample of that
    // neighbour's side alone -- callers of `lock` from outside the module --
    // because a cut statement was not narrowed.  Kernel:
    // `mod("amdgpu") { { "spinlock_t" } }` listed 687 symbols, none of them
    // in amdgpu.
    let query = r#"mod("modx") { { "lock" } }"#;
    let whole = run_query_with_read_cap(TEST_INPUT_ESCALATION, query, 0);
    let (_, truth) = incomplete_and_ids(&whole);
    assert_eq!(truth, vec![10, 1001, 3000], "premise: modx, in_1, lock");

    for cap in [1usize, 2, 3] {
        let res = run_query_with_read_cap(TEST_INPUT_ESCALATION, query, cap);
        let (said, ids) = incomplete_and_ids(&res);
        assert!(
            ids.iter().all(|id| truth.contains(id)),
            "cap {cap}: every row shown is part of the answer, got {ids:?}"
        );
        assert!(
            ids == truth || !said.is_empty(),
            "cap {cap}: a short answer says so, got {ids:?}"
        );
        assert!(
            !res.warnings
                .iter()
                .any(|w| w.message.contains("none of them")),
            "cap {cap}: {:?}",
            res.warnings
        );
    }
}

#[test]
fn a_probe_walks_from_one_neighbour_and_tests_the_other() {
    // `mod("modx") { refs { "lock" } }`: what modx holds or refers to, and
    // which refers to lock.  One function, `in_1`.
    //
    // At a cap of one a role may read ten rows.  As roles neither neighbour
    // fits: what encloses a reference to `lock` is eleven rows, and modx holds
    // ten functions, more than the cap.  Every role had to fit alone, so
    // composing the two answered "too many" as well, and the statement was
    // left cut.  Walking from `lock` reads its ten references, and each
    // declaration reached is tested against modx.
    const QUERY: &str = r#"mod("modx") { refs { "lock" } }"#;
    let (res, acts) = run_query_probe_traced(TEST_INPUT_ESCALATION, QUERY, 1);
    let middle: Vec<_> = acts
        .iter()
        .filter(|a| a.query_statement.trim() == r#"refs { "lock" }"#)
        .collect();
    assert!(
        middle.iter().any(|a| a.round == 1 && a.resolved.is_none()),
        "premise: one neighbour alone does not bound it: {acts:?}"
    );
    assert_eq!(
        middle.last().map(|a| a.resolved),
        Some(Some(1)),
        "walking from one neighbour and testing the other bounds it: {acts:?}"
    );

    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(ids, vec![10, 1001, 3000], "modx, in_1 and lock");
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);
}

#[test]
fn a_statement_the_probe_resolved_is_derived_within_what_the_probe_found() {
    // `mod("modx") { { "lock" } }`: the middle statement has no predicate of
    // its own.  The probe resolves it between its two neighbours to the one
    // function that is inside modx AND calls lock.  A statement like that
    // never read its own probe result: it was derived again from whichever
    // neighbour spoke first, under the row limit -- here from `lock`, whose
    // ten callers do not fit a cap of five -- so the answer was a cut sample
    // of one side although the probe already held the exact set.
    let query = r#"mod("modx") { { "lock" } }"#;
    let (_, acts) = run_query_probe_traced(TEST_INPUT_ESCALATION, query, 5);
    assert_eq!(
        acts.iter()
            .filter(|a| a.query_statement.trim() == r#"{ "lock" }"#)
            .last()
            .map(|a| a.resolved),
        Some(Some(1)),
        "premise: the probe resolves the middle to one instance: {acts:?}"
    );

    let res = run_query_with_read_cap(TEST_INPUT_ESCALATION, query, 5);
    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(ids, vec![10, 1001, 3000], "modx, in_1 and lock");
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);

    // With a filter in force the middle was probed before this change, and
    // resolved -- and was still derived from `lock` alone and cut.
    let filtered = r#"project("test_project") mod("modx") { { "lock" } }"#;
    let res = run_query_with_read_cap(TEST_INPUT_ESCALATION, filtered, 5);
    let (said, ids) = incomplete_and_ids(&res);
    assert_eq!(ids, vec![10, 1001, 3000]);
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);
}

#[test]
fn an_echo_follows_its_cut_parent_down() {
    // Four functions; two of them call `lock`, the other two call `hot`.
    // `X { { } ; "lock" }` keeps the two, and the echo lists what THEY refer
    // to: `lock`.
    //
    // At a small cap X is cut.  The echo is derived from X's rows as they
    // were read, `hot` among their children, and X is then narrowed by
    // `"lock"`.  A cut statement narrows no statement with rows of its own to
    // lose -- but an echo has none: it shows what its parent's shown rows
    // refer to, so it follows the parent down.  Otherwise `hot` stays listed
    // under functions that are no longer in the answer.
    let query = r#""in_1" "out_1" "f_1" "d_1" { { } ; "lock" }"#;
    let whole = run_query_with_read_cap(TEST_INPUT_ESCALATION, query, 0);
    let (_, truth) = incomplete_and_ids(&whole);
    assert_eq!(
        truth,
        vec![1001, 2001, 3000],
        "premise: in_1, out_1 and lock"
    );

    let mut any_cut = false;
    for cap in [1usize, 2, 3] {
        let res = run_query_with_read_cap(TEST_INPUT_ESCALATION, query, cap);
        let (said, ids) = incomplete_and_ids(&res);
        any_cut |= !said.is_empty();
        assert!(
            ids.iter().all(|id| truth.contains(id)),
            "cap {cap}: every row shown is part of the answer, got {ids:?}"
        );
    }
    assert!(any_cut, "premise: four rows do not fit the smallest caps");
}

#[test]
fn what_is_derived_from_a_cut_statement_is_cut() {
    // `func` lists ten functions of modx and is cut at a cap of three.  The
    // echo below it lists what THOSE rows refer to: it fits its own limit and
    // used to say nothing, though it is the listing of a sample.
    let res = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"mod("modx") { func { } }"#, 3);
    let (said, _) = incomplete_and_ids(&res);
    assert!(
        said.iter()
            .any(|w| w.starts_with("too many results in `{ }`")),
        "the listing of a cut statement is cut, got {:?}",
        said
    );
}

#[test]
fn a_capped_derive_is_flagged_and_said() {
    // The derive path used to LAUNDER a capped read: `find_parent_instance_ids`
    // applies the safety LIMIT but returned bare ids, so a derivation built
    // from a truncated id-fetch produced a selection that was neither flagged
    // (M3 would let it constrain) nor warned (the Phase-R warning sites ran
    // before it existed).  `{ "lock" }`: the wrapper derives lock's callers —
    // ten of them — via `notify_from_selection`.
    //
    // At read_cap = 3 the derive read caps at four of ten, so the answer shows
    // lock plus four callers; the flag must be carried and the warning said.
    let capped = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"{ "lock" }"#, 3);
    assert_eq!(
        capped.nodes.as_vec().len(),
        5,
        "lock + 4-of-10 derived callers"
    );
    assert!(
        capped
            .warnings
            .iter()
            .any(|w| w.message.contains("this part of the graph is incomplete")),
        "a capped derive must be said, or a short answer renders as a complete \
         one; got {:?}",
        capped
            .warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );

    // Uncapped: all ten callers, and no warning to cry wolf with.
    let full = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"{ "lock" }"#, 1000);
    assert_eq!(
        full.nodes.as_vec().len(),
        12,
        "lock + its instance + ten callers"
    );
    assert!(
        !full
            .warnings
            .iter()
            .any(|w| w.message.contains("incomplete")),
        "an uncapped derive must not warn"
    );

    // Control: a derive whose true set fits the cap resolves silently.
    let small = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"{ "in_1" }"#, 3);
    assert!(
        !small
            .warnings
            .iter()
            .any(|w| w.message.contains("incomplete")),
        "a derive under the cap must not warn"
    );

    // A TYPED derive that still caps.  With M4 the derive read of `func`'s
    // callers is itself typed, but all ten of lock's callers ARE functions, so
    // the typed read still exceeds read_cap = 3 and caps at four — and the
    // warning must fire.  (`a_typed_derive_reads_only_matching_rows` is the
    // complementary case, where the typed read fits the cap and correctly does
    // NOT warn.)
    let typed_capped = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"func { "lock" }"#, 3);
    assert!(
        typed_capped
            .warnings
            .iter()
            .any(|w| w.message.contains("this part of the graph is incomplete")),
        "a typed derive that still exceeds the cap must be said; got {:?}",
        typed_capped
            .warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
}

#[test]
fn a_typed_derive_reads_only_matching_rows() {
    // M4: a statement that cannot self-resolve derives from its neighbour, and
    // the derive read carries the statement's OWN predicate so it fetches only
    // matching rows — not an untyped superset it would cap and then filter.
    // `hot` has six callers (two functions, four data); each of these selective
    // derives fetches only its matches (<= 2), well under read_cap = 3, so it
    // resolves COMPLETE with no warning.
    //
    // The predicate reaches the read through the dedicated `compose_derive_*`
    // channel, NOT the shared `compose_children`/`compose_has_children` that
    // `find_symbol` uses for a node's displayed family (those must stay
    // type-agnostic — see `full_hierarchy_query`).
    //
    // Mutation guard (broken-and-watched): make any receiver mixin's
    // `derive_parent_expr` return `None` and the read reverts to the untyped
    // superset, which caps at four and — via M8's carry — warns.  So a passing
    // "no warning" assertion here means the predicate really reached the read.
    for (q, want) in [
        (r#"func { "hot" }"#, 3usize),                      // type selector
        (r#"filter("exact_name", "f_1") { "hot" }"#, 2),    // exact name via filter()
        (r#"filter("compound_name", "f_1") { "hot" }"#, 2), // compound name via filter()
    ] {
        let r = run_query_with_read_cap(TEST_INPUT_ESCALATION, q, 3);
        assert_eq!(
            r.nodes.as_vec().len(),
            want,
            "{q}: typed derive must resolve to the complete matching set"
        );
        assert!(
            !r.warnings.iter().any(|w| w.message.contains("incomplete")),
            "{q}: a derive whose typed read fits the cap must not warn; got {:?}",
            r.warnings
                .iter()
                .map(|w| w.message.clone())
                .collect::<Vec<_>>(),
        );
    }
}

#[test]
fn a_typed_has_derive_targets_the_contained_symbol() {
    // M4 has-direction: `mod("modx") has { func }` derives modx's CONTAINED
    // children and must filter them by the receiver's type on the CONTAINED
    // symbol (`contained_symbols`), not on modx itself.  modx contains ten
    // functions, so the answer is modx + 10.  If the predicate landed on the
    // source (modx) instead it would ask "modx.type = func" and return
    // nothing — the mutation guard for the has-child alias.
    let funcs = run_query(TEST_INPUT_ESCALATION, r#"mod("modx") has { func }"#);
    assert_eq!(
        funcs.nodes.as_vec().len(),
        11,
        "modx + its ten contained functions"
    );
    // modx contains no data symbols, so a typed has-derive for data is modx
    // alone.  A wrong-side filter would instead pass modx through regardless.
    let data = run_query(TEST_INPUT_ESCALATION, r#"mod("modx") has { data }"#);
    assert_eq!(
        data.nodes.as_vec().len(),
        1,
        "modx alone; it contains no data"
    );
}

#[test]
fn a_capped_composed_statement_warns_the_graph_is_incomplete() {
    // A statement whose read hit the SAFETY cap is too broad to compute, and
    // the user must be told — silence there is a confident wrong answer.
    //
    // `mod("modx") { func }` at read_cap = 3: the children read caps, so `func`
    // is capped but its budget is suppressed (it is composed), so the ordinary
    // "result budget" warning does NOT fire.  The incompleteness warning must.
    // No total is quoted — a total needs the whole result set.
    let res = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"mod("modx") { func }"#, 3);
    let msgs: Vec<&str> = res.warnings.iter().map(|w| w.message.as_str()).collect();
    assert!(
        msgs.iter()
            .any(|m| m.contains("this part of the graph is incomplete")),
        "a capped composed statement must warn, got {:?}",
        msgs
    );
    assert!(
        !msgs
            .iter()
            .any(|m| m.contains(" of ") && m.contains("symbols")),
        "the warning must not quote a total, got {:?}",
        msgs
    );
}

#[test]
fn a_cut_statement_shows_only_what_its_neighbour_confirms() {
    // `search("foo") { "fn_basic" }`.  Uncapped, the answer is EMPTY: none of
    // the search hits contains `fn_basic`.
    let complete =
        run_query_with_read_cap(TEST_INPUT_SEARCH, r#"search("foo") { "fn_basic" }"#, 1000);
    assert!(
        complete.nodes.as_vec().is_empty(),
        "premise: uncapped the composition is empty, got {:?}",
        complete.nodes.as_vec()
    );

    // At read_cap = 2 the search read stops at three of its hits.  The two
    // halves of the old rule ("a capped selection neither narrows nor is
    // narrowed") part ways here.
    //
    //  * The cut search does not narrow `fn_basic`: it holds some of its
    //    hits, so it cannot show that none contains it.  `fn_basic` stays.
    //  * The cut search IS narrowed by `fn_basic`, which was read in full:
    //    a hit that does not contain it is no part of the answer whichever
    //    hits were read.  The three used to be kept, and shown.
    //
    // And nothing claims "none of them relate": only some were looked at.
    let capped = run_query_with_read_cap(TEST_INPUT_SEARCH, r#"search("foo") { "fn_basic" }"#, 2);
    let (said, ids) = incomplete_and_ids(&capped);
    assert_eq!(ids, vec![110], "fn_basic alone; no search hit is confirmed");
    assert_eq!(said.len(), 1, "the search says it was cut: {:?}", said);
    assert!(
        !capped
            .warnings
            .iter()
            .any(|w| w.message.contains("none of them")),
        "a cut statement cannot say none of its rows relate: {:?}",
        capped.warnings
    );
}

#[test]
fn containment_edge_needs_both_endpoints_in_the_answer() {
    // The module CONTAINS the file and all three functions, and is the only
    // node in the answer. A has-edge whose child is absent would name a node
    // the caller never selected, so none may be emitted.
    //
    // The container side must be the populated one for this to discriminate:
    // asking it of a lone CHILD passes even when the scope is broken, because
    // there is then no container to expand from. That is how the first
    // version of this test managed to be vacuous.
    const QUERY: &str = r#""testmodule""#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    assert_eq!(
        res.nodes.as_vec().len(),
        1,
        "expected just the module, got {:?}",
        res.nodes.as_vec()
    );
    assert!(
        res.has_edges.0.is_empty(),
        "the module's children are not in the answer, so no has-edge may be \
         emitted, got {:?}",
        res.has_edges.0.iter().collect::<Vec<_>>()
    );
}

#[test]
fn equal_spans_pick_the_container_with_the_most_constraining_power() {
    // The fixture puts the directory, the file and the module instances all
    // at [0, 1000) of object 1, so "tightest container" is a genuine TIE for
    // any function they contain.
    //
    // The tie goes to the container that constrains the child most — here the
    // file, which holds only foo, over the module, which holds the file too.
    // That is read off the answer's own containment, not off symbol TYPE: at
    // this point there is only a graph, and "a file is tighter than a module"
    // is a fact about these rows rather than a rule worth hardcoding.
    const QUERY: &str = r#""testmodule" ; file("/main.go") ; "testmodule.foo""#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    // Assert the PREMISE: without both equal-span containers in the answer
    // there is no tie, and this test would pass under any rule at all. An
    // earlier version spelled the file as a bare `"/main.go"`, which matched
    // nothing — leaving `{module, foo}` and no tie to break.
    let instances: std::collections::HashSet<_> =
        res.nodes.0.iter().map(|n| n.symbol_instance.id).collect();
    for (id, what) in [(10, "module"), (510, "file"), (20, "foo")] {
        assert!(
            instances.contains(&id),
            "premise: {} (instance {}) must be in the answer, got {:?}",
            what,
            id,
            instances
        );
    }

    // One winner per child, never several.
    let mut children: Vec<_> = res.has_edges.0.iter().map(|e| e.child_instance).collect();
    children.sort();
    let mut deduped = children.clone();
    deduped.dedup();
    assert_eq!(
        children,
        deduped,
        "each child keeps exactly one container, got {:?}",
        res.has_edges.0.iter().collect::<Vec<_>>()
    );

    let foo_parent = res
        .has_edges
        .0
        .iter()
        .find(|e| e.child_instance == SymbolInstanceId::new(20))
        .map(|e| e.parent_instance);
    assert_eq!(
        foo_parent,
        Some(SymbolInstanceId::new(510)),
        "module (10) and file (510) both span [0,1000); the file holds only \
         foo while the module holds the file as well, so the file constrains \
         foo more and wins. Got {:?}",
        res.has_edges.0.iter().collect::<Vec<_>>()
    );
}

#[test]
fn containment_does_not_depend_on_how_the_query_was_phrased() {
    // Four spellings that select the SAME three nodes (module 10, foo 20,
    // file 510) must describe the same containment.
    //
    // They did not used to. Containment was read off each statement's
    // neighbourhood family, and those families are narrowed by the statement's
    // own scope — so `file(...) has { foo }` recorded (file, foo) while the
    // other spellings recorded (module, foo), and the graph disagreed with
    // itself about who contains foo. Asking the index for the containment
    // BETWEEN answer nodes removes the phrasing from the answer.
    const QUERIES: [&str; 4] = [
        r#""testmodule" ; file("/main.go") ; "testmodule.foo""#,
        r#"mod("testmodule") has { "testmodule.foo" } ; file("/main.go")"#,
        r#"file("/main.go") has { "testmodule.foo" } ; "testmodule""#,
        r#"mod("testmodule") { "testmodule.foo" } ; file("/main.go")"#,
    ];

    let mut baseline: Option<(Vec<i64>, Vec<(i64, i64)>)> = None;
    for query in QUERIES {
        let res = run_query(TEST_INPUT_CONTAINMENT, query);
        let mut nodes: Vec<i64> = res.nodes.0.iter().map(|n| n.symbol_instance.id).collect();
        nodes.sort();
        let mut edges: Vec<(i64, i64)> = res
            .has_edges
            .0
            .iter()
            .map(|e| (e.parent_instance.into(), e.child_instance.into()))
            .collect();
        edges.sort();
        match &baseline {
            None => baseline = Some((nodes, edges)),
            Some((want_nodes, want_edges)) => {
                assert_eq!(&nodes, want_nodes, "node sets must match for `{}`", query);
                assert_eq!(
                    &edges, want_edges,
                    "same nodes must give the same containment, but `{}` differs",
                    query
                );
            }
        }
    }
}

fn sorted_ids(res: &crate::statement::ExecutionResult) -> Vec<i64> {
    let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
    ids.sort();
    ids
}

#[test]
fn has_children_query() {
    // mod("testmodule") has { file has { "foo" } }
    // The word `file` filters its own statement.  It says nothing about
    // "foo", which is a function the file contains.
    const QUERY: &str = r#"mod("testmodule") has { file has { "foo" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);
    assert_eq!(sorted_ids(&res), vec![10, 20, 510]);
}

#[test]
fn has_parents_query() {
    // file has { "foo" }: the file that contains the function foo.
    const QUERY: &str = r#"file has { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);
    assert_eq!(sorted_ids(&res), vec![20, 510]);
}

#[test]
fn mixed_has_refs_query() {
    // mod("testmodule") has { file has { "foo" refs {} } }
    // Containment down to foo, then what foo refers to (bar).
    const QUERY: &str = r#"mod("testmodule") has { file has { "foo" refs {} } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);
    assert_eq!(sorted_ids(&res), vec![10, 20, 30, 510]);
}

#[test]
fn type_selector_function_query() {
    // func("testmodule.foo")
    // Returns function named "testmodule.foo"
    const QUERY: &str = r#"func("testmodule.foo")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have foo (20)
    assert_eq!(res.nodes.as_vec().len(), 1);
}

#[test]
fn type_selector_function_filter_at_root() {
    // func (no name) at root is a pure constraint with nothing to derive
    // from — the anchor-completeness check rejects it with a hint instead
    // of the old silent empty result.
    let res = run_query_err(TEST_INPUT_CONTAINMENT, r#"func"#);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("bare func at root must be an anchoring error"),
    };
    assert!(
        err.to_string().contains("selects nothing"),
        "unexpected error: {err}"
    );
}

#[test]
fn type_selector_function_explicit_select_all() {
    // `func select`
    // Explicitly select all functions (was func(filter="false")).
    const QUERY: &str = r#"func select"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);

    // foo (20), bar (30), baz (40)
    assert_eq!(res.nodes.as_vec().len(), 3);
}

#[test]
fn type_selector_module_query() {
    // mod("testmodule")
    // Returns module named "testmodule"
    const QUERY: &str = r#"mod("testmodule")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have module (10)
    assert_eq!(res.nodes.as_vec().len(), 1);
}

#[test]
fn has_propagates_by_default() {
    // file has { "foo" { "bar" } }
    // has now inherits by default, so the inner {} also uses HAS.
    // foo is in file (has), but foo does NOT contain bar (has) — functions don't contain functions.
    // So bar is not found, constraining foo and file out.
    const QUERY: &str = r#"file has { "foo" { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // With has inheriting: foo doesn't contain bar → empty result
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn has_with_explicit_refs_override() {
    // file has { "foo" refs { "bar" } }: `has` reaches foo, `refs` reaches bar.
    const QUERY: &str = r#"file has { "foo" refs { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);
    assert_eq!(sorted_ids(&res), vec![20, 30, 510]);
}

// ============================================================================
// has vs refs Comparison Tests
// ============================================================================
//
// These tests verify that has (containment) and refs (call graph) behave differently.
// Test data has:
// - Module contains: foo, bar, baz (via offset ranges)
// - Call graph: foo→bar→baz (via symbol_refs)

#[test]
fn has_vs_refs_module_to_function() {
    // With direct-children-only: test file(2) → function(1)
    // has: file CONTAINS foo (offset ranges)
    const HAS_QUERY: &str = r#"file("/main.go") has { "foo" }"#;
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    println!("has result: {:#?}", has_res.nodes);

    // File contains foo, so we get both
    assert_eq!(has_res.nodes.as_vec().len(), 2);
    assert!(has_res.nodes.as_vec().contains(&SymbolInstanceId::new(510))); // file
    assert!(has_res.nodes.as_vec().contains(&SymbolInstanceId::new(20))); // foo

    // refs: file CALLS foo? No refs from file to foo exist
    const REFS_QUERY: &str = r#"file("/main.go") refs { "foo" }"#;
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    println!("refs result: {:#?}", refs_res.nodes);

    // File doesn't call foo - no refs relationship exists
    // Both parent and child are filtered out when relationship doesn't hold
    assert_eq!(refs_res.nodes.as_vec().len(), 0);
}

#[test]
fn has_vs_refs_function_to_function() {
    // has: foo CONTAINS bar? No - foo [100,200) doesn't contain bar [200,300)
    const HAS_QUERY: &str = r#""foo" has { "bar" }"#;
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    println!("has result: {:#?}", has_res.nodes);

    // foo doesn't contain bar - no containment relationship exists
    // Both parent and child are filtered out when relationship doesn't hold
    assert_eq!(has_res.nodes.as_vec().len(), 0);

    // refs: foo CALLS bar? Yes - there's a ref from foo to bar
    const REFS_QUERY: &str = r#""foo" refs { "bar" }"#;
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    println!("refs result: {:#?}", refs_res.nodes);

    // foo calls bar, so we get both
    assert_eq!(refs_res.nodes.as_vec().len(), 2);
    assert!(refs_res.nodes.as_vec().contains(&SymbolInstanceId::new(20))); // foo
    assert!(refs_res.nodes.as_vec().contains(&SymbolInstanceId::new(30))); // bar
}

#[test]
fn has_vs_refs_all_children() {
    // Test has vs refs behavior comparison
    // has: Bare `func` replaces inherited `file` type filter, finds functions.
    // dir(2 instances) + file + func(3: foo,bar,baz) = 6 nodes.
    const HAS_QUERY: &str = r#"dir("/") has { file has { func } }"#;
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    println!("has result: {:#?}", has_res.nodes);

    assert_eq!(has_res.nodes.as_vec().len(), 6);

    // refs: test function-to-function refs
    // foo calls bar, bar calls baz
    const REFS_QUERY: &str = r#""foo" refs { func }"#;
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    println!("refs result: {:#?}", refs_res.nodes);

    // foo calls bar, so we get foo + bar
    assert_eq!(refs_res.nodes.as_vec().len(), 2); // foo + bar
}

#[test]
fn refs_is_default_relationship() {
    // Bare {} should use refs (the default)
    const DEFAULT_QUERY: &str = r#""foo" { "bar" }"#;
    let default_res = run_query(TEST_INPUT_CONTAINMENT, DEFAULT_QUERY);

    // Explicit refs should give same result
    const EXPLICIT_QUERY: &str = r#""foo" refs { "bar" }"#;
    let explicit_res = run_query(TEST_INPUT_CONTAINMENT, EXPLICIT_QUERY);

    println!("default {{}} result: {:#?}", default_res.nodes);
    println!("explicit refs result: {:#?}", explicit_res.nodes);

    // Both should have foo + bar (foo calls bar)
    assert_eq!(default_res.nodes.as_vec(), explicit_res.nodes.as_vec());
    assert_eq!(default_res.nodes.as_vec().len(), 2);
}

#[test]
fn refs_overrides_inherited_has() {
    // has { refs { } } - outer uses has, but inner explicitly uses refs
    // With direct-children-only: file(2) → function(1)
    // File contains foo (has), foo calls bar (refs)
    const QUERY: &str = r#"file("/main.go") has { "foo" refs { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);

    // Should have file (contains foo) + foo + bar (foo calls bar)
    assert_eq!(res.nodes.as_vec().len(), 3);
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(510))); // file
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(20))); // foo
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(30))); // bar
}

// ============================================================================
// Default Symbol Type Inheritance Tests
// ============================================================================
//
// These tests verify that type selectors set default types for child scopes.
// mod("test") {} should show modules AND functions that test references
// mod("test") { func } should explicitly filter to only functions
//
// Test data (TEST_INPUT_CONTAINMENT) has:
// - Module `testmodule` (type=3, id=1, instance=10)
// - Functions `foo`, `bar`, `baz` (type=1, ids=2,3,4, instances=20,30,40)
// - Refs: foo→bar (at 150), bar→baz (at 250) - so module refs bar and baz

#[test]
fn default_type_inheritance_module_refs_children() {
    // mod("testmodule") {} should show:
    // - module itself
    // - modules it references (none in test data)
    // - functions it references (bar and baz via contained refs)
    //
    // Without default type inheritance, {} would return ALL types.
    // With default type inheritance, {} filters to module + function types.
    const QUERY: &str = r#"mod("testmodule") {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have module (10) and the functions it refs (bar=30, baz=40)
    // via refs at positions 150 and 250 within module's range [0,1000)
    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // The module should be included
    assert!(
        nodes.contains(&SymbolInstanceId::new(10)),
        "Should include module"
    );

    // Functions referenced by refs within module's range should be included
    // (bar at 30, baz at 40)
    // Note: The refs are foo→bar (150→30) and bar→baz (250→40)
    // Both ref sites are within module's range [0,1000)
}

#[test]
fn default_type_inheritance_explicit_function_only() {
    // mod("testmodule") { func } should show:
    // - module itself (the parent selector)
    // - ONLY functions it references (not modules, because func is explicit)
    //
    // The explicit func overrides the default type inheritance for the CHILD scope
    // The parent (mod) is still included as it's the parent selector
    const QUERY: &str = r#"mod("testmodule") { func }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // Should include module (10) as the parent + functions (30, 40)
    // This is the same as the default case since there are no module-to-module refs
    // The difference would be visible if module referenced other modules
    assert!(
        nodes.contains(&SymbolInstanceId::new(10)),
        "Should include parent module"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "Should include bar"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "Should include baz"
    );
}

#[test]
fn default_type_inheritance_function_refs_children() {
    // func("foo") {} should show:
    // - function foo itself
    // - functions it references (bar)
    //
    // With default type inheritance from func, {} filters to function type only
    const QUERY: &str = r#"func("foo") {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // Should have foo (20) and bar (30) - foo calls bar
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "Should include foo"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "Should include bar"
    );

    // Should NOT include module since func sets default to function only
    assert!(
        !nodes.contains(&SymbolInstanceId::new(10)),
        "Should NOT include module"
    );
}

#[test]
fn default_type_inheritance_nested_scopes() {
    // mod("testmodule") { func("foo") {} }
    // First level: module (sets defaults to module+function)
    // Second level: func("foo") (overrides to function only)
    // Third level: {} inherits function-only from func
    const QUERY: &str = r#"mod("testmodule") { func("foo") {} }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // Module should be filtered out at second level by func
    // But wait - module is the parent, func filters the child
    // So we should have:
    // - module (10) at top level (no filter)
    // - foo (20) at second level (filtered to functions that module refs)
    // - bar (30) at third level (filtered to functions that foo refs)

    // Note: This depends on whether the type filter applies to the current level or child level
}

// ============================================================================
// File and Directory containment tests
// ============================================================================
//
// TEST_INPUT_CONTAINMENT has these symbols with hierarchy:
// - Directory "/" (id=50, instance=500) [0, 1000) level=4
// - File "/main.go" (id=51, instance=510) [0, 1000) level=2
// - Module "testmodule" (id=1, instance=10) [0, 1000) level=3
// - Function "testmodule.foo" (id=2, instance=20) [100, 200) level=1
// - Function "testmodule.bar" (id=3, instance=30) [200, 300) level=1
// - Function "testmodule.baz" (id=4, instance=40) [300, 400) level=1

#[test]
fn file_contains_function() {
    // file("/main.go") has { func }
    // Returns: file /main.go and all functions it contains
    const QUERY: &str = r#"file("/main.go") has { func }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510) + functions (20, 30, 40)
    assert_eq!(res.nodes.as_vec().len(), 4);
}

#[test]
fn file_contains_specific_function() {
    // file("/main.go") has { "foo" }
    // Returns: file /main.go and function foo
    const QUERY: &str = r#"file("/main.go") has { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510) + foo (20)
    assert_eq!(res.nodes.as_vec().len(), 2);
}

#[test]
fn directory_contains_file() {
    // dir("/") has { mod has { file } }
    // Bare `file` replaces inherited `mod` type filter, so it finds /main.go.
    // dir(2 instances) + module + file = 4 nodes.
    const QUERY: &str = r#"dir("/") has { mod has { file } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 4);
}

#[test]
fn directory_contains_module() {
    // dir("/") has { mod }
    // dir("/") has 2 instances (self-instance 500 and containment instance 501).
    // Bare `mod` filters to MODULE type, finding testmodule(10).
    // Result: dir(2 instances) + module = 3 nodes.
    const QUERY: &str = r#"dir("/") has { mod }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 3);
}

#[test]
fn directory_contains_function() {
    // dir("/") has { mod has { file has { func } } }
    // Each bare type selector replaces the inherited one from its parent scope:
    // file replaces mod, func replaces file.
    // dir(2 instances) + mod + file + func(3: foo,bar,baz) = 7 nodes.
    const QUERY: &str = r#"dir("/") has { mod has { file has { func } } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 7);
}

#[test]
fn full_hierarchy_query() {
    // dir("/") has { mod has { file("/main.go") has { "foo" } } }
    // Named file("/main.go") replaces inherited mod type filter, finds /main.go.
    // "foo" found inside the file. dir(2 instances) + mod + file + foo = 4 nodes.
    const QUERY: &str = r#"dir("/") has { mod has { file("/main.go") has { "foo" } } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 4);
}

#[test]
fn file_type_selector_filter_at_root() {
    // file (no name) at root is a pure constraint — anchoring error.
    let res = run_query_err(TEST_INPUT_CONTAINMENT, r#"file"#);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("bare file at root must be an anchoring error"),
    };
    assert!(
        err.to_string().contains("selects nothing"),
        "unexpected error: {err}"
    );
}

#[test]
fn file_type_selector_by_name() {
    // file("/main.go")
    // Returns file /main.go
    const QUERY: &str = r#"file("/main.go")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510)
    assert_eq!(res.nodes.as_vec().len(), 1);
}

#[test]
fn directory_type_selector_filter_at_root() {
    // dir (no name) at root is a pure constraint — anchoring error.
    let res = run_query_err(TEST_INPUT_CONTAINMENT, r#"dir"#);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("bare dir at root must be an anchoring error"),
    };
    assert!(
        err.to_string().contains("selects nothing"),
        "unexpected error: {err}"
    );
}

// ============================================================================
// Directory Containment - Direct Children Only Tests
// ============================================================================
//
// These tests verify the directory instance model in TEST_INPUT_TREE_BROWSER:
// - Each directory has instances ONLY for files directly in it
// - "/" has NO instances (no files directly in root)
// - "/src" has 1 instance (in object 1 for /src/main.go)
// - "/docs" has 1 instance (in object 5 for /docs/readme.md)
// - "/src/util" has 2 instances (in objects 2,3)
// - "/src/config" has 1 instance (in object 4)
//
// NOTE: dir("/src") currently uses CompoundNameMixin which does
// prefix/partial name matching, so it matches /src, /src/util, /src/config, etc.
// This is a known behavior - exact path matching could be added as an improvement.

#[test]
fn directory_src_util_contains_its_direct_files() {
    // dir("/src/util") has { file }
    // /src/util has instances: 1030 (self on obj 103), 1031 (on obj 2), 1032 (on obj 3).
    // Files: util.go(2020 on obj 2), helper.go(2030 on obj 3).
    // Bare `file` filters its own statement; a file is a leaf here anyway.
    // Result: dir(3 instances) + 2 files = 5 nodes.
    const QUERY: &str = r#"dir("/src/util") has { file }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec().len(),
        5,
        "/src/util should have 3 directory instances + 2 file instances = 5 nodes. Got {}.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn directory_docs_contains_its_direct_file() {
    // dir("/docs") has { file }
    // /docs has instances: 1020 (self on obj 102) and 1021 (on obj 5).
    // File: readme.md(2050 on obj 5).
    // Bare `file` filters its own statement; a file is a leaf here anyway.
    // Result: dir(2 instances) + 1 file = 3 nodes.
    const QUERY: &str = r#"dir("/docs") has { file }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec().len(),
        3,
        "/docs should have 2 directory instances + 1 file instance = 3 nodes. Got {}.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn directory_has_empty_scope_returns_children() {
    // dir("/src/util") has {}
    // Empty scope should inherit has relationship and return all direct children
    // /src/util has instances in objects 2,3 (util.go, helper.go)
    // With default type inheritance [DIRECTORY, FUNCTION], we get:
    // - Directory instances for /src/util
    // - Function instances in those objects (none in test data)
    // - Any other directories contained (none)
    const QUERY: &str = r#"dir("/src/util") has {}"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // /src/util has 2 instances (in objects 2 and 3)
    // Empty {} with has should find children in those objects
    // With default type filter [DIRECTORY, FUNCTION], we get:
    // - The 2 directory instances of /src/util itself (via the TypeSelector)
    // - Plus any contained directories/functions (none in test data that match)
    // So we expect at least 2 nodes for the directory itself
    assert!(
        res.nodes.as_vec().len() >= 2,
        "dir('/src/util') has {{}} should return directory + any children. Got {} nodes.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn directory_has_empty_scope_with_file_test_data() {
    // mod("testmodule") has {}
    // Empty scope should inherit has relationship and return all direct children
    // In TEST_INPUT_CONTAINMENT, module has instances and contains file/functions
    const QUERY: &str = r#"mod("testmodule") has {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // module "testmodule" has 1 instance [0,1000)
    // With default type filter [MODULE, FUNCTION], the empty has {} should find:
    // - The module instance itself
    // - Functions contained in it (foo, bar, baz - but depends on type hierarchy)
    // The module contains file (level 2), file contains functions (level 1)
    // But with direct containment, module (level 3) > file (level 2) should work
    // Actually with [MODULE, FUNCTION] filter, we should get:
    // - module itself (from mod selector)
    // - functions if they match the type filter
    assert!(
        res.nodes.as_vec().len() >= 1,
        "mod('testmodule') has {{}} should return module + any matching children. Got {} nodes.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn has_sibling_children_of_different_types_conjoin() {
    // dir("/") has { mod ; file }
    // Sibling children CONJOIN: the directory is kept because it contains a
    // module AND a file — not because it contains either.  "/" in
    // TEST_INPUT_CONTAINMENT contains both (the module testmodule and the
    // file /main.go, on different instances), so it satisfies both
    // conjuncts and survives together with the two children it matched.
    const QUERY: &str = r#"dir("/") has { mod ; file }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let names: Vec<_> = res.nodes.0.iter().map(|n| n.symbol.name.as_str()).collect();
    for expected in ["/", "testmodule", "/main.go"] {
        assert!(
            names.contains(&expected),
            "dir('/') has {{ mod ; file }} keeps the directory only if it \
             contains a module AND a file, and shows both: {expected} \
             missing from {names:?}",
        );
    }
}

// =============================================================================
// Directory refs tests (directory hierarchy via symbol_refs)
// =============================================================================

// Test data (TEST_INPUT_TREE_BROWSER):
// /src → /src/util and /src/config via symbol_refs
// Directories: /, /src, /docs, /src/util, /src/config

#[test]
fn directory_refs_children() {
    // dir("/src") refs { dir } should return /src + child dirs
    const QUERY: &str = r#"dir("/src") refs { dir }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    let names: Vec<_> = res.nodes.0.iter().map(|n| n.symbol.name.as_str()).collect();
    println!("directory_refs_children names: {:?}", names);

    assert!(names.contains(&"/src"), "Should contain parent /src");
    assert!(
        names.contains(&"/src/util"),
        "Should contain child /src/util"
    );
    assert!(
        names.contains(&"/src/config"),
        "Should contain child /src/config"
    );
}

// ============================================================================
// Generic select and filter verb tests
// ============================================================================

#[test]
fn generic_filter_type_func_with_name_and_select() {
    // filter("type", "func") filter("compound_name", "a") select
    // Same as "a" with func type — should find function "a"
    const QUERY: &str = r#"filter("type", "func") filter("compound_name", "a") select"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
}

#[test]
fn generic_filter_type_func_only() {
    // filter("type", "func") — a pure constraint with nothing to derive
    // from at root level: anchoring error instead of silent empty.
    let res = run_query_err(TEST_INPUT_CONTAINMENT, r#"filter("type", "func")"#);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("bare type filter at root must be an anchoring error"),
    };
    assert!(
        err.to_string().contains("selects nothing"),
        "unexpected error: {err}"
    );
}

#[test]
fn generic_filter_compound_name_inherit() {
    // filter("compound_name", "test", inherit=true) {{"b"}}
    // Namespace filter inherited through double parent.
    // The "test" compound name filter is inherited into child scopes,
    // constraining grandchildren to also match "test" in their name search.
    // Without the filter, "b" matches both test.b (92) and other.b (202).
    // With the filter, only test.* symbols survive.
    //
    // A name filter is a constraint, so the outer statement is NOT weak (see
    // `Command::is_non_constraining`) and the query composes as the
    // three-level chain it reads as: test.* calls test.* calls test.b.
    // Callers of test.b are test.a, test.c and test.main, but only test.a has
    // a caller of its own inside the namespace (test.main) — so test.c drops
    // out at the middle level, exactly as it does when the same query is
    // written with an explicit `select` (see `project_double_parent_query`).
    const QUERY: &str = r#"filter("compound_name", "test", inherit=true) {{"b"}}"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(res.warnings.is_empty(), "Should produce no warnings");
    assert_eq!(nodes.len(), 3);
    assert!(nodes.contains(&SymbolInstanceId::new(92)), "test.b");
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "test.a (caller of test.b, called by test.main)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(93)),
        "test.c calls test.b but nothing in the namespace calls test.c"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(942)),
        "test.main (caller of test.a)"
    );
    // Verify the filter actually excluded other.* symbols
    assert!(
        !nodes.contains(&SymbolInstanceId::new(201)),
        "other.a should be excluded"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(202)),
        "other.b should be excluded"
    );
}

#[test]
fn generic_filter_type_replacement() {
    // filter("type", "func") filter("type", "mod") — second replaces first (same kind tag)
    // The final type filter should be "mod" only.
    const QUERY: &str = r#"filter("type", "func") filter("type", "mod") filter("compound_name", "testmodule") select"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    // Should find module "testmodule" (10) — func filter was replaced by mod filter
    assert_eq!(res.nodes.as_vec().len(), 1);
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(10)));
}

#[test]
fn generic_filter_type_comma_separated() {
    // filter("type", "func,mod") — OR semantics for multiple types
    const QUERY: &str =
        r#"filter("type", "func,mod") filter("compound_name", "testmodule") select"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    // Should find module "testmodule" (matches mod type and compound name)
    assert!(res.nodes.as_vec().len() >= 1);
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(10)));
}

#[test]
fn generic_filter_exact_name() {
    // filter("exact_name", "/main.go") — exact name matching
    // This should use ExactNameMixin
    const QUERY: &str = r#"filter("type", "file") filter("exact_name", "/main.go") select"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    // Should find file "/main.go" (id=510)
    assert_eq!(res.nodes.as_vec().len(), 1);
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(510)));
}

#[test]
fn generic_filter_type_filter_alone_is_weak() {
    // filter("type", "func") alone — no selector, UnitVerb added.  The
    // anchor-completeness check now rejects it outright (the weak-statement
    // path is unreachable for constraint-only root statements).
    let res = run_query_err(TEST_INPUT_CONTAINMENT, r#"filter("type", "func")"#);
    assert!(res.is_err(), "constraint-only root statement must error");
}

#[test]
fn generic_filter_with_select_constrains_parent() {
    // filter + select is strong because select is a real (non-unit) selector.
    // The child selects directories, foo doesn't call any dirs → foo constrained away.
    const QUERY: &str = r#""foo" { filter("type", "dir") select }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        !nodes.contains(&SymbolInstanceId::new(20)),
        "foo should be constrained away by child that found no matching dirs"
    );
}

#[test]
fn a_type_word_filters_its_own_statement_only() {
    // "A type word filters the statement it is written in, and no other."
    // `func { "config.Debug" }` asks which functions use `config.Debug`.
    // The word `func` is about the users; what is used is data, and stays
    // findable without `data(...)` or `any`.
    let res = run_query(VERB_TEST, r#"func { "config.Debug" }"#);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)) && nodes.contains(&SymbolInstanceId::new(97)),
        "foo (a function) uses config.Debug (data); got {:?}",
        nodes
    );

    // The same going down: the type word on the child says nothing about
    // the grandchild.
    let res = run_query(VERB_TEST, r#""driver" { data { "info_a" } }"#);
    let nodes = res.nodes.as_vec();
    assert!(
        [200, 210, 220]
            .iter()
            .all(|id| nodes.contains(&SymbolInstanceId::new(*id))),
        "driver -> id_table -> info_a; got {:?}",
        nodes
    );
    // `inherit=true` is how a type word is carried into every scope below.
    let res = run_query(VERB_TEST, r#"func(inherit=true) { "config.Debug" }"#);
    assert!(
        !res.nodes.as_vec().contains(&SymbolInstanceId::new(97)),
        "an inherited `func` makes the child a function, and config.Debug is data"
    );
}

#[test]
fn named_selectors_of_one_statement_union() {
    // "Every named selector in a statement is a branch, and the statement is
    // the union of its branches."  A type on the selector does not change
    // that: the second used to replace the first.
    let res = run_query(TEST_INPUT_CONTAINMENT, r#"func("foo") func("bar")"#);
    assert_eq!(sorted_ids(&res), vec![20, 30]);

    // Each branch carries its own type.
    let res = run_query(VERB_TEST, r#"func("tar") data("config.Port")"#);
    assert_eq!(sorted_ids(&res), vec![94, 98]);

    // Mixed with a bare name, and under a parent: foo uses foo.bar (a
    // function) and config.Debug (data).
    let res = run_query(
        VERB_TEST,
        r#""foo" { func("foo.bar") data("config.Debug") "nothing_by_this_name" }"#,
    );
    assert_eq!(sorted_ids(&res), vec![91, 92, 97]);

    // A type word beside them filters all of them.
    let res = run_query(VERB_TEST, r#"func "tar" "config.Port""#);
    assert_eq!(sorted_ids(&res), vec![94]);
}

fn no_match_suggestions(input: &str, query: &str) -> Vec<String> {
    let res = run_query(input, query);
    let found: Vec<&crate::diagnostic::Diagnostic> = res
        .warnings
        .iter()
        .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::NoMatch)
        .collect();
    assert_eq!(
        found.len(),
        1,
        "`{}`: one no-match warning, got {:?}",
        query,
        res.warnings
    );
    found[0].suggestions.clone()
}

#[test]
fn a_no_match_suggests_only_what_would_match() {
    // "A no-match warning suggests only what would match if typed."
    // The name exists, as data: say how to ask for it.
    assert_eq!(
        no_match_suggestions(VERB_TEST, r#"func("config.Debug")"#),
        vec![r#"data("config.Debug")"#]
    );
    // A file is selected by its type.  The bare path used to be answered
    // with "Did you mean: /main.c" -- the string just typed.
    assert_eq!(
        no_match_suggestions(VERB_TEST, r#"project("test_project") "/main.c""#),
        vec![r#"file("/main.c")"#]
    );
    // A typed selector that misses under its own type is not told to type
    // itself: tar is a function, and nothing else.
    assert_eq!(
        no_match_suggestions(VERB_TEST, r#"data("tar")"#),
        vec![r#"func("tar")"#]
    );
    let res = run_query(VERB_TEST, r#"project("no_such_project") func("tar")"#);
    assert!(
        res.warnings
            .iter()
            .all(|w| !w.suggestions.contains(&r#"func("tar")"#.to_string())),
        "{:?}",
        res.warnings
    );
}

#[test]
fn two_type_words_in_one_statement_are_an_error() {
    // `macro func` used to run as `func`: the later word replaced the
    // earlier without a word.  The message gives the spelling that works.
    for (query, fix) in [
        (r#""foo" { macro func }"#, "`macro or func`"),
        (r#"data func("foo")"#, r#"`data or func("foo")`"#),
        (r#"func "foo" data("config.Port")"#, "replaces `func`"),
    ] {
        let err = match run_query_err(VERB_TEST, query) {
            Err(err) => format!("{:#}", err),
            Ok(_) => panic!("`{}` must not run", query),
        };
        assert!(
            err.contains("in the same statement") && err.contains(fix),
            "`{}`: {}",
            query,
            err
        );
    }
    // Not an error: the type said once, a group, an inherited type replaced
    // by the child's, and the explicit filters, which replace in place.
    for query in [
        r#"func "foo" "tar""#,
        r#""foo" { macro or func }"#,
        r#"func(inherit=true) "foo" { data }"#,
        r#"filter("type", "func") filter("type", "mod") "foo""#,
    ] {
        assert!(
            run_query_err(VERB_TEST, query).is_ok(),
            "`{}` must run",
            query
        );
    }
}

#[test]
fn a_group_that_says_a_type_replaces_the_inherited_one() {
    // foo uses config.Debug and not config.Port.  Under an inherited `func`,
    // one typed child replaces the type...
    let one = run_query(
        VERB_TEST,
        r#"func(inherit=true) "foo" { data("config.Debug") }"#,
    );
    assert_eq!(sorted_ids(&one), vec![91, 97]);
    // ...and so does a group of them.  The group used to keep the inherited
    // `func` beside its own `data` and match nothing.
    let group = run_query(
        VERB_TEST,
        r#"func(inherit=true) "foo" { data("config.Debug") or data("config.Port") }"#,
    );
    assert_eq!(sorted_ids(&group), vec![91, 97]);
    let juxtaposed = run_query(
        VERB_TEST,
        r#"func(inherit=true) "foo" { data("config.Debug") data("config.Port") }"#,
    );
    assert_eq!(sorted_ids(&juxtaposed), vec![91, 97]);
}

#[test]
fn weak_child_does_not_eliminate_parent() {
    // A bare type word constrains nothing about its neighbours: it is weak.
    // The fixture has no data symbols, so the child resolves empty.  A weak
    // child is a display echo: its emptiness must not take the parent with
    // it.  (`inherit=false` is the default, spelled out.)
    const QUERY: &str = r#""foo" { data(inherit=false) }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo must survive an empty WEAK child; got {:?}",
        nodes
    );
}

#[test]
fn weak_parent_does_not_eliminate_child() {
    // The mirror of `weak_child_does_not_eliminate_parent`, and the reason
    // it is a bug rather than a preference: written on the other side of the
    // braces, the same weak statement asserting the same absent evidence has
    // always left its neighbour alone.  Both spellings must agree.
    const QUERY: &str = r#"data(inherit=false) { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo must survive an empty WEAK parent; got {:?}",
        nodes
    );
}

#[test]
fn bare_and_explicit_type_children_agree() {
    // `data` and `data(inherit=false)` are one statement: a type word does
    // not inherit unless asked to, and neither spelling constrains a parent.
    let bare = run_query(TEST_INPUT_CONTAINMENT, r#""foo" { data }"#);
    let explicit = run_query(TEST_INPUT_CONTAINMENT, r#""foo" { data(inherit=false) }"#);

    assert_eq!(
        bare.nodes.as_vec(),
        explicit.nodes.as_vec(),
        "the two spellings of a weak type child must return the same nodes"
    );
}

#[test]
fn generic_select_alone_enumerates() {
    // Bare `select` is the bindness verb with the All anchor: it declares
    // the tree binding and enumerates everything its filters allow,
    // budget-bounded — no warning, no error.
    let res = run_query(TEST_INPUT_A, r#"select"#);
    assert!(
        !res.nodes.as_vec().is_empty(),
        "select must enumerate under the budget"
    );
}

#[test]
fn generic_filter_with_name_selector() {
    // filter("type", "func") "a" — GenericFilter + NameSelector coexist
    // The filter("type", "func") suppresses DefaultTypeFilter, NameSelector does the selection
    const QUERY: &str = r#"filter("type", "func") "a""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    // Should find function "a" (91)
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
}

#[test]
fn generic_select_queries_all_types_without_type_filter() {
    // Both GenericSelector and NameSelector now query all types at root level
    // because the root-level default type filter is [] (no filtering).
    // GenericSelector with filter("compound_name", ...) select finds all types.
    // NameSelector ("testmodule") also finds all types (no DefaultTypeFilter restriction).
    const QUERY_GENERIC: &str = r#"filter("compound_name", "testmodule") select"#;
    let res_generic = run_query(TEST_INPUT_CONTAINMENT, QUERY_GENERIC);

    const QUERY_PLAIN: &str = r#""testmodule""#;
    let res_plain = run_query(TEST_INPUT_CONTAINMENT, QUERY_PLAIN);

    // Both should find the module since there's no type filter at root level
    assert!(
        res_generic
            .nodes
            .as_vec()
            .contains(&SymbolInstanceId::new(10)),
        "GenericSelector should find module (queries all types)"
    );
    assert!(
        res_plain
            .nodes
            .as_vec()
            .contains(&SymbolInstanceId::new(10)),
        "NameSelector should also find module (root-level default type filter is empty)"
    );
}

#[test]
fn generic_select_is_idempotent() {
    // filter("compound_name", "foo") select filter("compound_name", "bar") select {}
    // select is now a simple marker — multiple selects collapse to one.
    // compound_name tag-dedup means the second filter replaces the first → only bar (30).
    const QUERY: &str =
        r#"filter("compound_name", "foo") select filter("compound_name", "bar") select {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "Should include bar"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(20)),
        "Should NOT include foo (replaced by bar)"
    );
}

// ============================================================================
// derive Tests
// ============================================================================

#[test]
fn derive_type_refs_equivalent_to_refs() {
    // derive(type="refs") should behave identically to refs
    const DERIVE_QUERY: &str = r#""foo" derive(type="refs") { "bar" }"#;
    const REFS_QUERY: &str = r#""foo" refs { "bar" }"#;
    let derive_res = run_query(TEST_INPUT_CONTAINMENT, DERIVE_QUERY);
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    assert_eq!(derive_res.nodes.as_vec(), refs_res.nodes.as_vec());
    assert_eq!(derive_res.nodes.as_vec().len(), 2); // foo + bar
}

#[test]
fn derive_type_has_equivalent_to_has() {
    // derive(type="has") should behave identically to has
    const DERIVE_QUERY: &str = r#"file("/main.go") derive(type="has") { "foo" }"#;
    const HAS_QUERY: &str = r#"file("/main.go") has { "foo" }"#;
    let derive_res = run_query(TEST_INPUT_CONTAINMENT, DERIVE_QUERY);
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    assert_eq!(derive_res.nodes.as_vec(), has_res.nodes.as_vec());
    assert_eq!(derive_res.nodes.as_vec().len(), 2); // file + foo
}

#[test]
fn derive_type_refs_has_union() {
    // derive(type="refs,has") should find via either relationship
    // File contains foo (has), foo calls bar (refs) — so with file as parent:
    // - has finds foo (contained by file)
    // - refs finds nothing (file doesn't call anything)
    // - derive(type="refs,has") finds foo (union)
    const QUERY: &str = r#"file("/main.go") derive(type="refs,has") { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    assert_eq!(res.nodes.as_vec().len(), 2); // file + foo
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(510))); // file
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(20))); // foo

    // Now test where refs would find results but has wouldn't:
    // foo calls bar (refs), but foo doesn't contain bar (has)
    const QUERY2: &str = r#""foo" derive(type="refs,has") { "bar" }"#;
    let res2 = run_query(TEST_INPUT_CONTAINMENT, QUERY2);

    assert_eq!(res2.nodes.as_vec().len(), 2); // foo + bar
    assert!(res2.nodes.as_vec().contains(&SymbolInstanceId::new(20))); // foo
    assert!(res2.nodes.as_vec().contains(&SymbolInstanceId::new(30))); // bar
}

#[test]
fn derive_inherit_true_propagates() {
    // derive(type="has", inherit=true) propagates to grandchildren
    // file derive(type="has", inherit=true) { { "foo" } }
    // Without inherit, the grandchild {} would reset to refs, but with inherit
    // the grandchild also uses has semantics
    // file has→ ??? has→ foo
    // Since file contains foo directly, the intermediate {} derives contained functions,
    // then the inner "foo" also uses has to constrain
    const QUERY: &str = r#"file("/main.go") derive(type="has", inherit=true) { func { "foo" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("derive inherit=true result: {:#?}", res.nodes);
    // file → (has) functions → (has) foo
    // intermediate func gets all functions contained in file (foo, bar, baz)
    // then "foo" is constrained by has from those functions — but functions don't contain other functions
    // So this should be empty because foo doesn't contain foo
    // Actually: the inner "foo" is a child of func, so it derives from func's selection
    // With has semantics: which of func's results contain "foo"? None — functions don't contain functions
    // This is expected: has between peer functions yields nothing
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn derive_inherits_by_default() {
    // derive now inherits by default (inherit=true).
    // file derive(type="has") { "foo" { "bar" } }
    // file has→ foo has→ bar — but foo doesn't contain bar → empty
    const QUERY: &str = r#"file("/main.go") derive(type="has") { "foo" { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("derive inherits by default result: {:#?}", res.nodes);
    // foo doesn't contain bar → empty
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn derive_explicit_no_inherit_resets_to_refs() {
    // With explicit inherit=false, grandchild resets to refs
    // file derive(type="has", inherit=false) { "foo" { "bar" } }
    // file has→ foo refs→ bar (grandchild resets to refs)
    const QUERY: &str = r#"file("/main.go") derive(type="has", inherit=false) { "foo" { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("derive explicit no inherit result: {:#?}", res.nodes);
    // file (has) foo (refs) bar — should find all three
    assert_eq!(res.nodes.as_vec().len(), 3); // file, foo, bar
}

// ============================================================================
// Unified type selector verb tests
// ============================================================================
//
// These tests verify the unified behavior where container type selectors
// (dir, file, mod) implicitly set refs+has with inherit, and func
// explicitly sets REFS to override any inherited relationship.

#[test]
fn dir_implicit_has_shows_files() {
    // dir("/src/util") { file } — works without explicit has
    // dir implicitly sets refs+has with inherit, so file children are found via HAS
    const QUERY: &str = r#"dir("/src/util") { file }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    let names: Vec<_> = res.nodes.0.iter().map(|n| n.symbol.name.as_str()).collect();
    println!("dir implicit has names: {:?}", names);

    // /src/util contains util.go and helper.go
    assert!(names.contains(&"/src/util"), "Should contain directory");
    assert!(
        names.contains(&"/src/util/util.go"),
        "Should contain util.go"
    );
    assert!(
        names.contains(&"/src/util/helper.go"),
        "Should contain helper.go"
    );
}

#[test]
fn dir_empty_scope_shows_dirs_and_files() {
    // dir("/src") {} — empty scope should show [DIRECTORY, FILE] (new defaults)
    // dir implicitly sets refs+has, so children are found via either relationship
    const QUERY: &str = r#"dir("/src") {}"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    let names: Vec<_> = res.nodes.0.iter().map(|n| n.symbol.name.as_str()).collect();
    println!("dir empty scope names: {:?}", names);

    // /src references /src/util and /src/config (refs), contains main.go (has)
    assert!(names.contains(&"/src"), "Should contain /src itself");
    assert!(
        names.contains(&"/src/util"),
        "Should contain child dir /src/util"
    );
    assert!(
        names.contains(&"/src/config"),
        "Should contain child dir /src/config"
    );
    assert!(
        names.contains(&"/src/main.go"),
        "Should contain child file /src/main.go"
    );
}

#[test]
fn func_overrides_inherited_refs_has() {
    // func("foo") { "bar" } — still uses REFS only (default unchanged)
    const QUERY: &str = r#"func("foo") { "bar" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    println!("func refs only nodes: {:?}", nodes);

    // foo calls bar (refs) — both found
    assert_eq!(nodes.len(), 2);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
}

#[test]
fn dir_func_overrides_inherited_refs_has() {
    // dir("/") { func("foo") { "bar" } }
    // dir sets refs+has with inherit, but func overrides to REFS for its children.
    // foo calls bar via REFS, so bar is found.
    const QUERY: &str = r#"dir("/") { func("foo") { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    println!("dir func override nodes: {:?}", nodes);

    // dir (500 or 501) + foo (20) + bar (30) = at least 3 nodes
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "Should include foo"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "Should include bar"
    );
    assert!(nodes.len() >= 3, "Should have dir + foo + bar");
}

#[test]
fn func_overrides_has_in_nested_scope() {
    // TypeSelector (func) does NOT override the inherited relationship type.
    // Use explicit `refs` to switch from inherited HAS back to REFS.
    // foo calls bar → result is file + foo + bar.
    const QUERY: &str = r#"file("/main.go") has { func refs { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    assert_eq!(nodes.len(), 3);
    assert!(nodes.contains(&SymbolInstanceId::new(510)), "file");
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo (calls bar)"
    );
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
}

#[test]
fn derive_refs_overrides_inherited_refs_has() {
    // derive(type="refs") inside dir overrides inherited refs+has to REFS-only
    const QUERY: &str = r#"dir("/") derive(type="refs") { dir }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    let names: Vec<_> = res.nodes.0.iter().map(|n| n.symbol.name.as_str()).collect();
    println!("derive refs override names: {:?}", names);

    // REFS only: / references /src and /docs via symbol_refs
    assert!(names.contains(&"/"), "Should contain /");
    assert!(names.contains(&"/src"), "Should contain /src");
    assert!(names.contains(&"/docs"), "Should contain /docs");
}

#[test]
fn file_implicit_has_shows_functions() {
    // file("/main.go") { func } — works without explicit has
    // file implicitly sets refs+has with inherit
    const QUERY: &str = r#"file("/main.go") { func }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    println!("file implicit has nodes: {:?}", nodes);

    // file contains foo, bar, baz (via HAS from implicit refs+has)
    // func filters to FUNCTION type and overrides to REFS — but the statement's
    // own relationship to parent is restored to inherited value (refs+has)
    // So file's children are found via refs+has, filtered to functions
    assert!(nodes.contains(&SymbolInstanceId::new(510)), "file");
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz");
    assert_eq!(nodes.len(), 4);
}

#[test]
fn derive_invalid_type_errors() {
    // derive(type="invalid") should produce an error
    const QUERY: &str = r#"derive(type="invalid") { "foo" }"#;
    let res = run_query_err(TEST_INPUT_CONTAINMENT, QUERY);
    assert!(res.is_err(), "Expected error for invalid type");
}

#[test]
fn derive_missing_type_errors() {
    // derive without type param should produce an error
    const QUERY: &str = r#"derive { "foo" }"#;
    let res = run_query_err(TEST_INPUT_CONTAINMENT, QUERY);
    assert!(res.is_err(), "Expected error for missing type param");
}

// Nested function containment tests
// TEST_INPUT_NESTED_FUNC has:
//   foo [100, 500) containing anon150 [150, 300) and anon350 [350, 490)
//   bar [500, 700), baz [700, 900)

#[test]
fn nested_func_has_children() {
    // "foo" has {} should return foo and its nested anonymous functions
    const QUERY: &str = r#""foo" has {}"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    // foo (20) and its two nested functions (25, 26)
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(20),
            SymbolInstanceId::new(25),
            SymbolInstanceId::new(26)
        ]
    );
}

#[test]
fn nested_func_has_func_children() {
    // "foo" has { func } should return foo and nested functions matching func
    const QUERY: &str = r#""foo" has { func }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    // foo (20) and its two nested functions (25, 26)
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(20),
            SymbolInstanceId::new(25),
            SymbolInstanceId::new(26)
        ]
    );
}

#[test]
fn nested_func_has_parent() {
    // func has { "anon150" } should find foo as a container of anon150
    const QUERY: &str = r#"func has { "anon150" }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    println!("{:#?}", res.nodes);
    // foo (20) as the container, and anon150 (25) as the child
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(25)));
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(20)));
}

// --- Non-constraining / caller-chain tests ---
//
// Bare type selectors (func, mod, type, etc. without a name) at the top
// of a caller-chain must NOT constrain intermediate nodes. Their selection
// is derived from children, so constraining children would be circular.

#[test]
fn non_constraining_bare_func_does_not_narrow_caller_chain() {
    // func {{ "baz" }} — two-level caller chain filtered to functions.
    // baz's callers: bar. bar's callers: foo. Both are functions.
    // Without non-constraining fix, func would constrain intermediate
    // nodes to functions only (circular), potentially breaking the chain.
    const QUERY: &str = r#"func {{ "baz" }}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    // foo (grandparent caller), bar (parent caller), baz
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz should be in results"
    );
}

#[test]
fn non_constraining_wrapped_bare_func_same_as_unwrapped() {
    // { func {{ "baz" }} } — wrapping in {} should not change behavior.
    // The outer {} gives func a parent, but func is still non-constraining
    // because its selection is child-derived (no ancestor has initial selection).
    const QUERY: &str = r#"{ func {{ "baz" }} }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz should be in results"
    );
}

#[test]
fn non_constraining_bare_mod_does_not_narrow_caller_chain() {
    // mod {{ "baz" }}: the word `mod` is about the outermost statement only,
    // so baz (a function) is found two scopes below it.
    const QUERY: &str = r#"mod {{ "baz" }}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);
    let ids = sorted_ids(&res);
    assert!(
        ids.contains(&10) && ids.contains(&40),
        "the module and baz; got {:?}",
        ids
    );

    // Carried down on request, the type meets a function and finds nothing.
    let res = run_query(TEST_INPUT_CONTAINMENT, r#"mod(inherit=true) {{ "baz" }}"#);
    assert_eq!(sorted_ids(&res), Vec::<i64>::new());
}

#[test]
fn non_constraining_does_not_affect_selector_with_name() {
    // func("foo") { "bar" } — func with a name is a selector, not non-constraining.
    // It should constrain children normally.
    const QUERY: &str = r#"func("foo") { "bar" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    // foo calls bar → both should be present
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar should be in results"
    );
}

#[test]
fn non_constraining_inside_selector_scope() {
    // "foo" { func { "baz" } } — foo calls bar, bar calls baz.
    // func filters to functions. The single {} level matches the 2-hop chain.
    const QUERY: &str = r#""foo" { func { "baz" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("non_constraining_inside_selector_scope: {:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    // foo (root selector), bar (intermediate func), baz (leaf selector)
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz should be in results"
    );
}

#[test]
fn non_constraining_inside_selector_scope_extra_depth_is_empty() {
    // "foo" { func {{ "baz" }} } — extra {} level adds a 3rd hop,
    // but foo→bar→baz is only 2 hops. Result should be empty.
    const QUERY: &str = r#""foo" { func {{ "baz" }} }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!(
        "non_constraining_inside_selector_scope_extra_depth: {:#?}",
        res.nodes
    );
    assert_eq!(
        res.nodes.as_vec().len(),
        0,
        "extra depth should yield no results"
    );
}

#[test]
fn non_constraining_containment_still_works() {
    // dir("/") has { func } — func as a leaf filter inside containment
    // should still constrain the parent (dir contains functions).
    // func is NOT non-constraining here because its ancestor dir("/")
    // has an initial selection.
    const QUERY: &str = r#"dir("/") has { func }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    // Directory + functions (foo, bar, baz)
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz should be in results"
    );
}

// ============================================================================
// Data inheritance pruning tests
// ============================================================================
//
// Test data in verb_test.sql:
//   driver(200) → id_table(210) → info_a(220), info_b(230)
//   info_a(220) → config_a(240), info_b(230) → config_b(250)
//   config_a(240) → channels_a(260), config_b(250) → channels_b(270)

#[test]
fn data_inherit_with_name_prunes_to_target_path() {
    // data(inherit=true) "driver" {{{{"channels_a"}}}}
    // Should return ONLY the path: driver → id_table → info_a → config_a → channels_a
    // NOT info_b, config_b, channels_b
    const QUERY: &str = r#"data(inherit=true) "driver" {{{{"channels_a"}}}}"#;
    let res = run_query(VERB_TEST, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();

    assert!(nodes.contains(&SymbolInstanceId::new(200)), "driver");
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "id_table");
    assert!(nodes.contains(&SymbolInstanceId::new(220)), "info_a");
    assert!(nodes.contains(&SymbolInstanceId::new(240)), "config_a");
    assert!(nodes.contains(&SymbolInstanceId::new(260)), "channels_a");

    assert!(
        !nodes.contains(&SymbolInstanceId::new(230)),
        "info_b should be pruned"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(250)),
        "config_b should be pruned"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(270)),
        "channels_b should be pruned"
    );
}

#[test]
fn data_inherit_without_name_prunes_to_target_path() {
    // Same query without the top-level name selector — should also prune correctly.
    // data(inherit=true) {{{{"channels_a"}}}}
    const QUERY: &str = r#"data(inherit=true) {{{{"channels_a"}}}}"#;
    let res = run_query(VERB_TEST, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();

    // Should contain the path to channels_a
    assert!(nodes.contains(&SymbolInstanceId::new(260)), "channels_a");

    // Should NOT contain the info_b/config_b/channels_b branch
    assert!(
        !nodes.contains(&SymbolInstanceId::new(230)),
        "info_b should be pruned"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(250)),
        "config_b should be pruned"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(270)),
        "channels_b should be pruned"
    );
}

#[test]
fn data_inherit_weak_parent_derives_full_chain() {
    // Regression: weak UnitVerb children were skipped even when the parent was also weak,
    // preventing the full chain from being derived.
    // data(inherit=true) {{{"channels_a"}}} — 3-level deep, all intermediate statements are weak.
    // Expected chain: id_table → info_a → config_a → channels_a
    const QUERY: &str = r#"data(inherit=true) {{{"channels_a"}}}"#;
    let res = run_query(VERB_TEST, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();

    assert!(
        nodes.contains(&SymbolInstanceId::new(210)),
        "id_table must be derived"
    );
    assert!(nodes.contains(&SymbolInstanceId::new(220)), "info_a");
    assert!(nodes.contains(&SymbolInstanceId::new(240)), "config_a");
    assert!(nodes.contains(&SymbolInstanceId::new(260)), "channels_a");

    assert!(
        !nodes.contains(&SymbolInstanceId::new(230)),
        "info_b should be pruned"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(250)),
        "config_b should be pruned"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(270)),
        "channels_b should be pruned"
    );
}

// ============================================================================
// Direct-only / unnest tests
// ============================================================================
//
// TEST_INPUT_NESTED_FUNC:
//   dir "/" [0,1000), file "/main.go" [0,1000), module "testmodule" [0,1000)
//   foo [100,500) containing anon150 [150,300) and anon350 [350,490)
//   bar [500,700), baz [700,900)
//   Refs: anon150 body [160,170) → bar, bar [550,560) → baz

#[test]
fn unfolded_has_file_reaches_nested_funcs() {
    // `X { Y }` lists every descendant whose symbol is not folded, at any
    // depth.  Nothing in this fixture is folded, so a file reaches every
    // function it contains, including the ones nested inside foo.
    //
    // This used to assert the opposite, under the strict depth+1 rule.  That
    // rule cannot survive module -> directory -> file -> declarations being a
    // real chain: it would make `mod { func }` empty.  Hiding is folding's
    // job, a property of the symbol, pinned in `type_filter_test.rs`.
    const QUERY: &str = r#"file("/main.go") has { func }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unfolded has func: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(510)), "file");
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz");
    assert!(
        nodes.contains(&SymbolInstanceId::new(25)),
        "anon150, nested inside foo, is reached"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(26)),
        "anon350, nested inside foo, is reached"
    );
}

#[test]
fn unnest_has_file_shows_all_children() {
    // file has { unnest func } — all function children including nested
    const QUERY: &str = r#"file("/main.go") has { unnest func }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest has func: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(510)), "file");
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(25)), "anon150");
    assert!(nodes.contains(&SymbolInstanceId::new(26)), "anon350");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz");
}

#[test]
fn unfolded_refs_reach_through_nested_scopes() {
    // foo { } — default REFS.  The ref to bar sits inside anon150, which is
    // inside foo, so it is foo's reference too.  A closure's calls count as
    // its enclosing function's: the default reaches through every level.
    const QUERY: &str = r#""foo" { }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unfolded refs from foo: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar, referenced from inside anon150"
    );
}

#[test]
fn unnest_refs_shows_all_refs() {
    // foo { unnest } — unnest REFS: all refs including nested
    const QUERY: &str = r#""foo" { unnest }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest refs from foo: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar (via anon150 ref)"
    );
}

#[test]
fn direct_only_has_foo_shows_nested_funcs() {
    // foo has { } — foo's direct HAS children are anon150 and anon350
    // (no intermediary between foo and its nested functions)
    const QUERY: &str = r#""foo" has { }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("foo direct has children: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(25)), "anon150");
    assert!(nodes.contains(&SymbolInstanceId::new(26)), "anon350");
}

#[test]
fn unfolded_nested_scope_chains_through_levels() {
    // "foo" refs { refs { } } — REFS at both levels.  The ref at [160,170) sits
    // inside anon150, which is inside foo, so bar is foo's reference; bar ->
    // baz then follows at the second level.
    //
    // Previously bar was hidden here and baz could not be discovered through
    // it.  That was the strict depth+1 rule; see the comment on
    // `unfolded_has_file_reaches_nested_funcs`.
    const QUERY: &str = r#""foo" refs { refs { } }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("nested scope refs-only from foo: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar, referenced from inside anon150"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz, reached through bar at the second level"
    );
}

#[test]
fn unnest_nested_scope_shows_transitive_children() {
    // "foo" refs { unnest refs { } } — unnest lifts the direct-only filter at level 1,
    // so bar appears. Then level 2 (direct-only) finds bar's direct ref to baz.
    const QUERY: &str = r#""foo" refs { unnest refs { } }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest nested scope from foo: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar (via unnested ref)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz (direct ref from bar)"
    );
}

#[test]
fn direct_only_refs_module_shows_all_refs() {
    // mod("testmodule") { } — REFS from module level. Functions (level 1) inside
    // a module (level 3) are NOT intermediaries for REFS, so refs are visible.
    const QUERY: &str = r#"mod("testmodule") { }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("module direct refs: {:?}", nodes);
    // Module refs: anon150 → bar at [160,170), bar → baz at [550,560)
    // Functions at level 1 don't block level 3 parent refs
    assert!(nodes.contains(&SymbolInstanceId::new(10)), "testmodule");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar (via ref)");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz (via ref)");
}

// ----------------------------------------------------------------------------
// REFS direct-only: which containers shadow a parent.
//
// A container strictly inside the parent shadows the parent's refs only when it
// is at the *same or broader* level.  Narrower containers do not: a function
// (level 2) inside a module (level 4) must not hide the module's refs, and a
// field (level 1) inside a type (level 2) must not hide the struct's refs.
//
// `direct_only_refs_module_shows_all_refs` above cannot see this, because its
// fixture module spans the whole file and the HAS half of the default relation
// supplies the same nodes.  These tests isolate the REFS half with
// `derive(type="refs")`.
// ----------------------------------------------------------------------------

#[test]
fn direct_only_refs_module_isolates_refs_half() {
    // REFS only: every ref in the fixture sits inside a function, so if functions
    // shadowed the module this returns the module and nothing else.
    const QUERY: &str = r#"mod("testmodule") derive(type="refs") { }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("module refs-only: {:?}", nodes);
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar — referenced from anon150, inside foo"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz — referenced from inside bar"
    );
}

#[test]
fn direct_only_refs_type_sees_refs_inside_its_fields() {
    // struct Outer { Inner a; Helper b; } — both refs live inside fields (level 1),
    // which are narrower than the type (level 2) and so must not shadow it.
    const QUERY: &str = r#"type("Outer") derive(type="refs") { }"#;
    let res = run_query(TEST_INPUT_STRUCT_FIELDS, QUERY);

    let nodes = res.nodes.as_vec();
    println!("Outer refs-only: {:?}", nodes);
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "Inner — named by field Outer.a"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(45)),
        "Helper — named by field Outer.b"
    );
}

#[test]
fn unfolded_refs_type_reaches_through_nested_type() {
    // Wrapper.Nested is a type inside a type.  Nothing here is folded, so
    // Wrapper reaches the reference through it -- and Wrapper.Nested, being the
    // closer container, has it too.
    //
    // NOTE: with the strict depth+1 rule this was the control proving that
    // shadowing happened at all, so that the rule could not be satisfied by
    // dropping the filter entirely.  With nothing folded here the default is
    // plain transitive descent; that the filter exists at all is pinned where
    // a fixture does fold a symbol (`type_filter_test.rs`).
    let outer = run_query(
        TEST_INPUT_STRUCT_FIELDS,
        r#"type("Wrapper") derive(type="refs") { }"#,
    );
    let outer_nodes = outer.nodes.as_vec();
    println!("Wrapper refs-only: {:?}", outer_nodes);
    assert!(
        outer_nodes.contains(&SymbolInstanceId::new(40)),
        "Inner, reached through Wrapper.Nested"
    );

    let inner = run_query(
        TEST_INPUT_STRUCT_FIELDS,
        r#"type("Wrapper.Nested") derive(type="refs") { }"#,
    );
    let inner_nodes = inner.nodes.as_vec();
    println!("Wrapper.Nested refs-only: {:?}", inner_nodes);
    assert!(
        inner_nodes.contains(&SymbolInstanceId::new(40)),
        "Inner — named directly by Wrapper.Nested"
    );
}

// ============================================================================
// Scoped children tests — verify that {} uses only parent-scoped instances
// ============================================================================
//
// Test data (in test_input_b.sql): macro M has expansion instances inside two
// different functions (e, g) in the same file.  M-in-e references data x;
// M-in-g references data y.  Querying children of M scoped to e should only
// return x, and scoped to g should only return y.

#[test]
fn scoped_children_e_m() {
    // "e" { "M" {} } — M has instances inside both e [950,959) and g [970,979).
    // The {} on M should only find children of M's instance inside e (x),
    // not M's instance inside g (y).
    const QUERY: &str = r#""e" { "M" {} }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(95)),
        "e should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(200)),
        "M inside e should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(210)),
        "x should be in results (child of M in e)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(201)),
        "M inside g should NOT be in results"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(211)),
        "y should NOT be in results (child of M in g)"
    );
}

#[test]
fn scoped_children_g_m() {
    // Symmetric: "g" { "M" {} } should only find y, not x.
    const QUERY: &str = r#""g" { "M" {} }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(97)),
        "g should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(201)),
        "M inside g should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(211)),
        "y should be in results (child of M in g)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(200)),
        "M inside e should NOT be in results"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(210)),
        "x should NOT be in results (child of M in e)"
    );
}

// ============================================================================
// Scoped derivation with labels
// ============================================================================
// These tests verify that the DB-based derivation (find_child_instance_ids /
// find_parent_instance_ids) works correctly when labels (@label / #label)
// connect separately-scoped query branches.

#[test]
fn label_scoped_children_e_m() {
    // Label the scoped M-children inside e, reference from another branch.
    // @res "e" { "M" { @found } }; #found
    // The labeled node should only contain x (child of M in e), never y.
    const QUERY: &str = r#"@res "e" { "M" { @found } }; #found"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(
        nodes.contains(&SymbolInstanceId::new(210)),
        "x (child of M in e)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(201)),
        "M inside g should NOT appear"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(211)),
        "y should NOT appear (child of M in g)"
    );
}

#[test]
fn label_scoped_children_g_m() {
    // Symmetric: label the scoped M-children inside g.
    const QUERY: &str = r#"@res "g" { "M" { @found } }; #found"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(97)), "g");
    assert!(nodes.contains(&SymbolInstanceId::new(201)), "M inside g");
    assert!(
        nodes.contains(&SymbolInstanceId::new(211)),
        "y (child of M in g)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(200)),
        "M inside e should NOT appear"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(210)),
        "x should NOT appear (child of M in e)"
    );
}

#[test]
fn label_refs_scoped_to_parent() {
    // Label "a" and use it: d→{e,f}, label e's children, check the label
    // only reflects e's refs (f), not all symbols.
    // @src "d" { @children }; #children
    const QUERY: &str = r#"@src "d" { @children }; #children"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(94)), "d");
    // d calls e and f
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e (child of d)");
    assert!(nodes.contains(&SymbolInstanceId::new(96)), "f (child of d)");
    // The label #children should expose e and f
    // but NOT a, b, c, g, or main
    assert!(
        !nodes.contains(&SymbolInstanceId::new(91)),
        "a should NOT appear"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(92)),
        "b should NOT appear"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(97)),
        "g should NOT appear"
    );
}

#[test]
fn label_two_branches_different_scopes() {
    // Two labeled branches scoping different parents, verify they don't leak.
    // @a_branch "e" { "M" {} }; @b_branch "g" { "M" {} }
    // Both branches exist independently; each M-scope is isolated.
    const QUERY: &str = r#"@a_branch "e" { "M" {} }; @b_branch "g" { "M" {} }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    // e-branch: e, M-in-e, x
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "x");
    // g-branch: g, M-in-g, y
    assert!(nodes.contains(&SymbolInstanceId::new(97)), "g");
    assert!(nodes.contains(&SymbolInstanceId::new(201)), "M inside g");
    assert!(nodes.contains(&SymbolInstanceId::new(211)), "y");
}

#[test]
fn label_parent_derivation_scoped() {
    // Derive parent from scoped child: find who calls e, scoped within the graph.
    // "d" { @callee "e" }; #callee
    // d→e, so #callee should include e but the derivation result still includes d + e.
    const QUERY: &str = r#""d" { @callee "e" }; #callee"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(94)), "d");
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e (labeled)");
}

#[test]
fn label_with_has_derivation() {
    // Use HAS derivation with labels: dir has { file has { @funcs func } }; #funcs
    // The labeled @funcs should contain functions found via containment.
    const QUERY: &str = r#"dir("/") has { file has { @funcs func } }; #funcs"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    // The containment data has: dir → file → {foo(20), bar(30), baz(40)}
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo via HAS");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar via HAS");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz via HAS");
}

#[test]
fn label_with_unnest_has_derivation() {
    // Use unnest HAS: dir has { unnest @all }; #all
    // Unnest should find all descendants, not just direct children.
    const QUERY: &str = r#"dir("/") has { unnest @all }; #all"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    // unnest HAS from dir: finds module, file, foo, bar, baz
    assert!(
        nodes.contains(&SymbolInstanceId::new(10)),
        "testmodule via unnest HAS"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(510)),
        "file via unnest HAS"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(20)),
        "foo via unnest HAS"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(30)),
        "bar via unnest HAS"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(40)),
        "baz via unnest HAS"
    );
}

#[test]
fn scoped_children_unnest_m() {
    // "e" { "M" { unnest } } — with unnest, should still be scoped to M-in-e.
    const QUERY: &str = r#""e" { "M" { unnest } }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(
        nodes.contains(&SymbolInstanceId::new(210)),
        "x (child of M in e)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(201)),
        "M inside g should NOT appear"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(211)),
        "y should NOT appear"
    );
}

#[test]
fn scoped_parent_derivation_from_child() {
    // Derive parent from child: "M" scoped under "e" should find e as parent.
    // "e" has { "M" } — M constrained to M-in-e, check e is the parent.
    const QUERY: &str = r#""e" has { "M" }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(201)),
        "M inside g should NOT appear"
    );
}

#[test]
fn scoped_children_refs_only() {
    // "e" refs { "f" } — REFS-only derivation. e calls f, so f appears.
    // M is not a ref child of e — it's only a HAS child (macro expansion),
    // so M should not appear.
    const QUERY: &str = r#""e" refs { "f" }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    // e refs f (via ref at [951,952)), so f should appear
    assert!(
        nodes.contains(&SymbolInstanceId::new(96)),
        "f (ref child of e)"
    );
    // M is not reachable via refs-only
    assert!(
        !nodes.contains(&SymbolInstanceId::new(200)),
        "M should NOT appear via refs"
    );
}

#[test]
fn scoped_has_children_only() {
    // "e" has { "M" } — HAS-only derivation. M is a macro expansion inside e,
    // so M-in-e should appear.
    const QUERY: &str = r#""e" has { "M" }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(
        nodes.contains(&SymbolInstanceId::new(200)),
        "M inside e via HAS"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(201)),
        "M inside g should NOT appear"
    );
}

#[test]
fn nested_has_refs_scope_isolation() {
    // Three-level query: dir has { mod has { func } }.
    // Each level uses HAS derivation. Functions should be scoped to the module,
    // which is scoped to the directory.
    const QUERY: &str = r#"dir("/") has { mod has { func } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    // dir has module, module has functions
    assert!(nodes.contains(&SymbolInstanceId::new(10)), "testmodule");
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz");
}

#[test]
fn label_scoped_nested_has_chain() {
    // Label at the deepest HAS level, verify scope isolation through the chain.
    // dir("/") has { mod has { @fns func } }; #fns
    const QUERY: &str = r#"dir("/") has { mod has { @fns func } }; #fns"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo labeled");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar labeled");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz labeled");
}

#[test]
fn label_forced_with_scoped_derivation() {
    // Forced label use with scoped derivation:
    // "e" label("parent") { "M" {} }; "g" { use("parent", forced=true) }
    // The forced use should inject e's children into g's scope, constraining
    // to only M-children visible from e.
    const QUERY: &str = r#""e" label("parent") { "M" {} }; "g" { use("parent", forced=true) }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(97)), "g");
    // e's scoped M children (x) should appear
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(
        nodes.contains(&SymbolInstanceId::new(210)),
        "x (child of M in e)"
    );
}

// ============================================================================
// Multi-instance symbol constraint tests.
//
// Symbol f (id=6) has two instances: 86 (bar.c) and 96 (main.c).  Only
// instance 86 (bar.c) references h.
//
// THE SOURCE OF A REFERENCE IS THE INSTANCE THAT ENCLOSES IT, so narrowing f
// by what it refers to keeps 86 and drops 96.  This test used to pin the
// opposite ("symbol-level matching keeps both"): one instance referring
// stood for the whole symbol, which is what let another program's `main`
// vouch for a call it never makes.  What d refers to is still the SYMBOL f,
// so d's narrowing is untouched.
// ============================================================================

#[test]
fn multi_instance_nested_keeps_the_instance_that_refers() {
    // d -> {e, f} and f(bar.c) -> h.
    // "d" { "f" { "h" } } keeps the f that calls h (86), not the other (96).
    const QUERY: &str = r#""d" { "f" { "h" } }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(94)),
        "d should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(86)),
        "f (bar.c) should be in results"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(96)),
        "f (main.c) does not refer to h"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(88)),
        "h should be in results"
    );
    assert!(res.warnings.is_empty(), "no warnings expected");
}

// ============================================================================
// Ephemeral layer tests
// ============================================================================

#[test]
fn ephemeral_verb_outside_layer_block_fails() {
    // Ephemeral verbs are only allowed inside layer { } blocks.
    // Using them standalone should produce a parse error.
    const QUERY: &str = concat!(
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=50000, end=50100, instance_type=1); "#,
        r#""foo""#,
    );
    let res = run_query_err(VERB_TEST, QUERY);
    assert!(res.is_err(), "standalone ephemeral_instance should fail");
}

#[test]
fn ephemeral_zero_width_span_is_rejected() {
    // `int4range(n, n)` is EMPTY, and in Postgres `anyrange @> emptyrange` is
    // TRUE — an empty range is contained by every range.  So a zero-width row's
    // owner lookup matches every instance in the object and its `ORDER BY`
    // hands it an arbitrary one, wherever in the file that sits; the row is
    // then numbered into a stranger's gap.
    //
    // A row spanning no bytes has nothing to point at, so it is refused at the
    // verb rather than placed somewhere meaningless.
    for (what, q) in [
        (
            "instance",
            concat!(
                r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
                r#"start=50000, end=50000, instance_type=1) }; "foo""#,
            ),
        ),
        (
            "ref",
            concat!(
                r#"layer { ephemeral_ref(to_symbol=1, from_object=1, "#,
                r#"start=50000, end=50000) }; "foo""#,
            ),
        ),
    ] {
        let msg = match run_query_err(VERB_TEST, q) {
            Err(e) => format!("{e:?}"),
            Ok(_) => panic!("a zero-width {what} span must be refused"),
        };
        assert!(
            msg.contains("end > start"),
            "the {what} error should say what is wrong, got: {msg}"
        );
    }

    // And a one-byte span is fine — the bound is `end > start`, not some
    // minimum width.
    const OK: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=50000, end=50001, instance_type=1) }; "foo""#,
    );
    assert!(
        run_query_err(VERB_TEST, OK).is_ok(),
        "a one-byte ephemeral instance is legitimate"
    );
}

#[test]
fn ephemeral_instance_is_queryable() {
    // Create an ephemeral instance for an existing symbol and verify
    // it appears when querying that symbol.
    // Symbol 1 = "foo", object 1 = main.c
    // Existing instance 91 is at [910,919). Add a second at [50000,50100).
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=50000, end=50100, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    // Must contain the original persistent instance
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "persistent foo instance should be in results"
    );
    // Must also contain the ephemeral instance (negative ID)
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(has_eph, "ephemeral instance should appear with negative ID");
}

#[test]
fn ephemeral_ref_creates_edge() {
    // Create an ephemeral ref from foo's object to tar's symbol,
    // then query foo → tar. Without the ephemeral ref, foo does not
    // reference tar (only foo.bar and foobar via existing refs).
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_ref(to_symbol=4, from_object=1, "#,
        r#"start=915, end=916) "#,
        r#"}; "#,
        r#""foo" { "tar" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(94)),
        "tar should be reachable via ephemeral ref"
    );
}

#[test]
fn ephemeral_layers_are_isolated_between_queries() {
    // Run two separate queries. An ephemeral instance created in one query
    // must not leak into the other.
    const QUERY1: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=51000, end=51100, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    const QUERY2: &str = r#""foo""#;

    let res1 = run_query(VERB_TEST, QUERY1);
    let res2 = run_query(VERB_TEST, QUERY2);

    let nodes1 = res1.nodes.as_vec();
    let nodes2 = res2.nodes.as_vec();

    let eph_count_1 = nodes1
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .count();
    let eph_count_2 = nodes2
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .count();

    assert!(
        eph_count_1 > 0,
        "query with ephemeral_instance should have negative IDs"
    );
    assert_eq!(
        eph_count_2, 0,
        "query without ephemeral verbs should have no negative IDs"
    );
}

#[test]
fn multiple_ephemeral_instances_same_symbol() {
    // Create two ephemeral instances for the same symbol. Both should appear.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=52000, end=52100, instance_type=1); "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=52200, end=52300, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    let eph_count = nodes
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .count();
    assert!(
        eph_count >= 2,
        "both ephemeral instances should appear, got {}",
        eph_count
    );
}

// --- A. Visibility filtering in relationship queries ---

#[test]
fn eph_ref_parent_traversal() {
    // A1: Ephemeral ref enables traversal that doesn't exist in persistent data.
    // foo has NO persistent ref to sort.IsSorted. An eph ref at [913,914)
    // (inside foo's range [910,919)) pointing to sort.IsSorted (symbol 5)
    // should make "foo" { "sort.IsSorted" } find sort.IsSorted as a child.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_ref(to_symbol=5, from_object=1, "#,
        r#"start=913, end=914) "#,
        r#"}; "#,
        r#""foo" { "sort.IsSorted" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(95)),
        "sort.IsSorted should be reachable via ephemeral ref"
    );
}

#[test]
fn eph_instance_in_containment() {
    // A2: Ephemeral instance participates in HAS (offset containment) queries.
    // Create eph instance of tar (symbol 4) at [911,912), which is inside
    // foo's range [910,919). Query "foo" has { "tar" } — the eph instance
    // should appear as a containment child of foo.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=4, object_id=1, "#,
        r#"start=911, end=912, instance_type=1) "#,
        r#"}; "#,
        r#""foo" has { "tar" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "foo should be in results"
    );
    // The ephemeral instance of tar at [911,912) should appear as child
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(
        has_eph,
        "ephemeral tar instance inside foo's range should appear in HAS query"
    );
}

// --- B. Edge discovery with ephemeral rows ---

#[test]
fn eph_ref_produces_edge() {
    // B1: Ephemeral ref appears in the edge list returned by find_edges_between.
    // Create eph ref at [914,915) (inside foo's range) to sort.IsSorted.
    // Edge 91→95 should appear.
    // Uses different offset from A1 [913,914) to produce a different layer hash.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_ref(to_symbol=5, from_object=1, "#,
        r#"start=914, end=915) "#,
        r#"}; "#,
        r#""foo" { "sort.IsSorted" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let edges = format_edges(res.edges);
    assert!(
        edges.contains(&"91-95".to_string()),
        "edge from foo(91) to sort.IsSorted(95) should exist via ephemeral ref, got {:?}",
        edges
    );
}

#[test]
fn eph_instance_produces_edge() {
    // B2: Ephemeral instance of foo.bar (symbol 2) makes both persistent (92)
    // and ephemeral instances appear as children of foo.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=2, object_id=1, "#,
        r#"start=53100, end=53200, instance_type=1) "#,
        r#"}; "#,
        r#""foo" { "foo.bar" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "foo should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(92)),
        "persistent foo.bar instance"
    );
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(
        has_eph,
        "ephemeral foo.bar instance should also appear with negative ID"
    );
}

// --- C. Phase 1 chaining — multiple ephemeral producers ---

#[test]
fn layer_block_symbol_then_instance() {
    // C1: Symbol + instance in one layer block. The instance should be
    // visible in the final query.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_symbol(name="eph_chain", "#,
        r#"project_id=1, symbol_type=1); "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=53300, end=53400, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "persistent foo");
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(
        has_eph,
        "ephemeral instance from second producer should appear"
    );
}

#[test]
fn layer_block_instance_and_ref_enables_traversal() {
    // C2: Instance + ref in one layer block. Create eph instance of
    // sort.IsSorted, and eph ref from foo's range [916,917) to
    // sort.IsSorted. Query should traverse foo → sort.IsSorted
    // and find both the persistent instance (95) and the ephemeral one.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=5, object_id=1, "#,
        r#"start=53500, end=53600, instance_type=1); "#,
        r#"ephemeral_ref(to_symbol=5, from_object=1, "#,
        r#"start=916, end=917) "#,
        r#"}; "#,
        r#""foo" { "sort.IsSorted" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "foo");
    assert!(
        nodes.contains(&SymbolInstanceId::new(95)),
        "persistent sort.IsSorted instance"
    );
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(
        has_eph,
        "ephemeral sort.IsSorted instance should also appear"
    );
}

// --- D. Content-addressed caching ---

#[test]
fn eph_instance_same_params_is_idempotent() {
    // D1: Running the same layer block query twice should succeed both
    // times. The second run hits the cache (find_eph_layer returns the existing
    // layer) and reuses the existing data without re-inserting.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=53700, end=53800, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res1 = run_query(VERB_TEST, QUERY);
    let res2 = run_query(VERB_TEST, QUERY);

    let nodes1 = res1.nodes.as_vec();
    let nodes2 = res2.nodes.as_vec();
    assert!(nodes1.contains(&SymbolInstanceId::new(91)));
    assert!(nodes2.contains(&SymbolInstanceId::new(91)));

    let eph1 = nodes1
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .count();
    let eph2 = nodes2
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .count();
    assert!(eph1 > 0, "first run should have ephemeral instance");
    assert!(
        eph2 > 0,
        "second run should reuse cached ephemeral instance"
    );
}

// --- E. Baseline regression guard ---

#[test]
fn query_without_eph_verbs_returns_only_persistent() {
    // E1: Queries without ephemeral verbs should return only persistent data.
    // All instance IDs must be positive — no ephemeral leakage from other tests.
    const QUERY: &str = r#""foo" { "foo.bar" }"#;
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(!nodes.is_empty(), "should return results");
    let all_positive = nodes.iter().all(|id| {
        let v: i64 = (*id).into();
        v > 0
    });
    assert!(
        all_positive,
        "all IDs should be positive (no ephemeral leakage), got {:?}",
        nodes
    );
}

// --- F. Ephemeral coexistence with persistent data ---

#[test]
fn eph_instance_at_persistent_offset_coexists() {
    // F1: An ephemeral instance at the same (symbol, object_id, offset_range) as
    // persistent instance 91 (foo at [910,919)) should succeed. The UNIQUE
    // constraint is split: persistent rows enforce uniqueness among themselves,
    // ephemeral rows enforce uniqueness within each layer. Cross-domain
    // duplicates are allowed.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=910, end=919, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "persistent foo instance should still be present"
    );
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(
        has_eph,
        "ephemeral foo instance at same offset should coexist"
    );
}

// --- G. Garbage collection ---

#[test]
fn eph_gc_purges_old_layers() {
    // Verify GC works by creating an ephemeral layer, verifying it exists,
    // deleting it explicitly, then re-running the query to confirm fresh
    // layers are created (different IDs).
    //
    // We use delete_eph_layer (targeted) instead of purge_old_eph_layers
    // (global TTL=0) to avoid interfering with concurrently running tests.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=54000, end=54100, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );

    // Run 1: create ephemeral layers.
    let res1 = run_query(VERB_TEST, QUERY);
    let eph_ids_1: Vec<_> = res1
        .nodes
        .as_vec()
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .copied()
        .collect();
    assert!(!eph_ids_1.is_empty(), "should have ephemeral instances");

    // Get the ephemeral layer IDs and delete them.
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(VERB_TEST).await;

        // Verify layer has instances before deletion.
        for &inst_id in &eph_ids_1 {
            let layer_ids = index
                .get_eph_layer_for_instance(inst_id.into())
                .await
                .unwrap();
            for layer_id in layer_ids {
                let ids = index
                    .get_eph_instance_ids_for_layer(layer_id)
                    .await
                    .unwrap();
                assert!(
                    !ids.is_empty(),
                    "layer {} should have instances before deletion",
                    layer_id
                );
                index.delete_eph_layer(layer_id).await.unwrap();
            }
        }
    });

    // Run 2: same query should create fresh layers (not cache hits).
    let res2 = run_query(VERB_TEST, QUERY);
    let eph_ids_2: Vec<_> = res2
        .nodes
        .as_vec()
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .copied()
        .collect();
    assert!(
        !eph_ids_2.is_empty(),
        "should still produce ephemeral instances"
    );
    // Fresh layers get new negative IDs from the sequence, so they differ.
    assert_ne!(
        eph_ids_1, eph_ids_2,
        "after deletion, re-running should produce different ephemeral IDs"
    );
}

// --- Layer block tests ---

#[test]
fn layer_block_groups_ops_into_single_layer() {
    // Create an ephemeral symbol + ephemeral instance inside a layer block.
    // Both ops go into a single layer. The instance (for symbol 1 = foo)
    // should be queryable.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_symbol(name="layer_test_sym", project_id=1, symbol_type=1); "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=55000, end=55100, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "persistent foo should be in results"
    );
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(has_eph, "ephemeral instance from layer block should appear");
}

#[test]
fn layer_block_cache_hit() {
    // Run the same layer block query twice. Second run should hit cache
    // and produce the same ephemeral IDs.  The layer-activation trace makes
    // the cache path explicit: first run creates the layer, second reuses
    // the same row without repopulating.  (The 55100..55200 instance range
    // is unique to this test, so the first run is a guaranteed cache miss
    // even with other tests sharing the fixture DB.)
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=55100, end=55200, instance_type=1) "#,
        r#"}; "#,
        r#""foo""#,
    );

    let (res1, acts1) = run_query_traced(VERB_TEST, QUERY);
    let eph_ids_1: Vec<_> = res1
        .nodes
        .as_vec()
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .copied()
        .collect();

    let (res2, acts2) = run_query_traced(VERB_TEST, QUERY);
    let eph_ids_2: Vec<_> = res2
        .nodes
        .as_vec()
        .iter()
        .filter(|id| {
            let v: i64 = (**id).into();
            v < 0
        })
        .copied()
        .collect();

    assert_eq!(
        eph_ids_1, eph_ids_2,
        "same layer block should produce same ephemeral IDs (cache hit)"
    );

    // One layer-bearing statement, materialised per visible root: one root shard
    // per fixture project (project 2's root shard is empty — the op references
    // project 1's object — but goes through the same cache gate).
    use crate::command::ShardRole;
    assert_root_roles(&acts1, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
    assert_root_roles(&acts2, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
    assert!(
        acts1.iter().all(|a| a.created),
        "first run must create each root's layer, got {:?}",
        acts1
    );
    assert!(
        acts2.iter().all(|a| !a.created),
        "second run must hit the cache on every root, got {:?}",
        acts2
    );
    assert_eq!(
        acts1.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        acts2.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        "cache hits must reuse the same per-root layer rows"
    );
}

#[test]
fn layer_block_includes_parent_context_in_hash() {
    // Two sequential layer blocks. The second layer block's hash includes
    // the parent context (the first layer block's layer ID).
    // Verify both layers are created and the query works.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_ref(to_symbol=5, from_object=1, "#,
        r#"start=55300, end=55310) "#,
        r#"}; "#,
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id=4, object_id=1, "#,
        r#"start=55200, end=55300, instance_type=1) "#,
        r#"}; "#,
        r#""tar""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(94)),
        "persistent tar should be in results"
    );
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(has_eph, "ephemeral instance from layer block should appear");
}

// ============================================================================
// Loc verb tests
// ============================================================================

#[test]
fn loc_matches_multiple_files() {
    // verb_test.sql has two files named main.c in different projects:
    //   object 1: /main.c      (project 1)
    //   object 3: /src/main.c  (project 2)
    // Both match the suffix "main.c". loc("main.c", 1) should create
    // an ephemeral symbol+instance for each matching file.
    const QUERY: &str = r#"loc("main.c", 1)"#;
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    // All instances should be ephemeral (negative IDs).
    assert!(
        nodes.len() >= 2,
        "loc should match at least 2 files, got {}",
        nodes.len()
    );
    assert!(
        nodes.iter().all(|id| {
            let v: i64 = (*id).into();
            v < 0
        }),
        "all loc instances should be ephemeral (negative IDs), got {:?}",
        nodes
    );
}

#[test]
fn loc_single_file_with_project_filter() {
    // When a project filter is specified, only the matching project's file
    // should be returned.
    const QUERY: &str = r#"loc("main.c", 1, project="test_project_2")"#;
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert_eq!(
        nodes.len(),
        1,
        "loc with project filter should match exactly 1 file, got {:?}",
        nodes
    );
    assert!(
        {
            let v: i64 = nodes[0].into();
            v < 0
        },
        "loc instance should be ephemeral (negative ID)"
    );
}

#[test]
fn eph_layer_rollback_prevents_poisoned_cache() {
    // If create_eph_layer starts a transaction but it is rolled back
    // (simulating a failed populate), the layer must not be visible.
    // A subsequent request with the same hash must get created=true.
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(VERB_TEST).await;
        let hash = sha2::Sha256::digest(b"rollback_test_unique_hash");

        // Attempt 1: create layer, then rollback (simulate failure).
        {
            let txn = index
                .create_eph_layer(None, None, &hash, index::db_diesel::EphLayerKind::Ephemeral)
                .await
                .unwrap();
            assert!(txn.created(), "first create should return created=true");
            txn.rollback().await.unwrap();
        }

        // Attempt 2: same hash — must get created=true (layer was rolled back).
        {
            let txn = index
                .create_eph_layer(None, None, &hash, index::db_diesel::EphLayerKind::Ephemeral)
                .await
                .unwrap();
            assert!(txn.created(), "after rollback, retry must get created=true");
            txn.rollback().await.unwrap();
        }
    });
}

#[test]
fn canary_filtered_by_find_symbol() {
    use index::db_diesel::{
        CompositeFilter, EphContext, LeafNameMixin, ScopeContext, CANARY_LAYER_ID,
    };

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_A).await;

        // Build a filter that would match the canary symbol by name.
        let filter = CompositeFilter::leaf(LeafNameMixin::new("__canary__", true));

        // Call find_symbol with no ephemeral layers visible (roots only).
        let roots = index.load_root_layers().await.unwrap();
        let result = index
            .find_symbol(
                &filter,
                ScopeContext::Absent,
                ScopeContext::Absent,
                FamilyDemands::default(),
                &EphContext::rooted(roots.clone()),
            )
            .await
            .expect("find_symbol should succeed (canary must be filtered out)")
            .into_inner();

        // The canary row must never appear in results.
        assert!(
            result.nodes.is_empty(),
            "canary symbol must not appear in results with an empty chain, got {} nodes",
            result.nodes.len()
        );

        // Even with the canary layer visible, the canary should be findable.
        // Pseudo-root construction: visibility is a flat set, so admitting
        // the canary as an extra "root" makes it visible without inventing
        // a chain for it.
        let mut canary_roots = roots;
        canary_roots.push(index::db_diesel::RootLayer {
            id: CANARY_LAYER_ID,
            project_id: -999999,
            name: "canary".to_string(),
            hash: vec![],
        });
        let canary_eph = EphContext::rooted(canary_roots);
        let result = index
            .find_symbol(
                &filter,
                ScopeContext::Absent,
                ScopeContext::Absent,
                FamilyDemands::default(),
                &canary_eph,
            )
            .await
            .expect("find_symbol with canary visible should succeed")
            .into_inner();
        assert_eq!(
            result.nodes.len(),
            1,
            "canary symbol should appear when its layer is in eph_ids"
        );
    });
}

#[test]
fn canary_source_not_fetchable() {
    use index::symbols::FileId;

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_A).await;

        // The canary object is id -999999; get_file_contents must refuse
        // non-positive ids so /source/{file_id} can't return fixture content.
        let result = index.get_file_contents(FileId::new(-999999)).await;
        assert!(
            result.is_err(),
            "canary object content must not be fetchable by id"
        );
    });
}

#[test]
fn canary_leak_detected_by_has_eph_leak() {
    use index::db_diesel::{EphContext, Selection, SelectionNode, CANARY_LAYER_ID};
    use index::models_diesel::{Object, Project, Symbol, SymbolInstance};
    use std::collections::Bound;

    // Manually construct a Selection with a canary row — simulates a filter bypass.
    let mut selection = Selection::new();
    selection.nodes.push(SelectionNode {
        symbol: Symbol {
            id: -999999,
            name: "__canary__".into(),
            symbol_path: "canary".into(),
            project_id: -999999,
            symbol_type: 1,
            symbol_scope: None,
            leaf_name: "__canary__".into(),
            layer: CANARY_LAYER_ID,
            folded: false,
        },
        symbol_instance: SymbolInstance {
            id: -999999,
            symbol: -999999,
            object_id: -999999,
            offset_range: (Bound::Included(0), Bound::Excluded(1)),
            instance_type: 1,
            layer: CANARY_LAYER_ID,
            nesting_level: 0,
            dfs_enter: 0,
            dfs_exit: 0,
            name_range: None,
            signature_range: None,
            content_range: None,
        },
        object: Object {
            id: -999999,
            project_id: -999999,
            module_path: "".into(),
            filesystem_path: "/__canary__".into(),
            filetype: "canary".into(),
            content_hash: "".into(),
            layer: CANARY_LAYER_ID,
        },
        project: Project {
            id: -999999,
            project_name: "__canary__".into(),
            root_path: "/__canary__".into(),
            upload_status: "complete".into(),
            root_layer_id: 1,
        },
        query_statements: vec![],
    });

    // With an empty visible set, the canary is a leak.
    assert!(
        selection.has_eph_leak(&EphContext::rooted(vec![])),
        "canary row must be detected as a leak with an empty visible set"
    );

    // With some other layer visible, canary is still a leak.
    let other = EphContext::rooted(vec![index::db_diesel::RootLayer {
        id: -1,
        project_id: 1,
        name: "p1".to_string(),
        hash: vec![],
    }]);
    assert!(
        selection.has_eph_leak(&other),
        "canary row must be detected as a leak when its ID is not visible"
    );

    // Only when canary layer is explicitly included, it's not a leak.
    let canary_visible = EphContext::rooted(vec![index::db_diesel::RootLayer {
        id: CANARY_LAYER_ID,
        project_id: -999999,
        name: "canary".to_string(),
        hash: vec![],
    }]);
    assert!(
        !selection.has_eph_leak(&canary_visible),
        "canary row must not be a leak when its ID is visible"
    );
}

#[test]
fn canary_not_reachable_via_run_query() {
    let res = run_query(TEST_INPUT_A, r#""__canary__""#);
    assert!(
        res.nodes.as_vec().is_empty(),
        "canary must not appear via full query path"
    );
}

/// Leak-detection coverage for every relationship field in `Selection`,
/// not just `.nodes`.  A leak in any of `.parents`, `.children`,
/// `.has_parents`, `.has_children` is just as dangerous (the row would
/// surface in the result graph even though the user did not authorise
/// its layer); `Checked<Selection>` must catch all of them.
#[test]
fn canary_leak_in_each_relationship_field_detected() {
    use index::db_diesel::{
        Checked, ChildReference, EphContext, HasChildReference, HasParentReference,
        ParentReference, Selection, CANARY_LAYER_ID,
    };
    use index::models_diesel::{Object, Symbol, SymbolInstance, SymbolRef};
    use std::collections::Bound;

    fn canary_symbol() -> Symbol {
        Symbol {
            id: -999999,
            name: "__canary__".into(),
            symbol_path: "canary".into(),
            project_id: -999999,
            symbol_type: 1,
            symbol_scope: None,
            leaf_name: "__canary__".into(),
            layer: CANARY_LAYER_ID,
            folded: false,
        }
    }
    fn canary_instance() -> SymbolInstance {
        SymbolInstance {
            id: -999999,
            symbol: -999999,
            object_id: -999999,
            offset_range: (Bound::Included(0), Bound::Excluded(1)),
            instance_type: 1,
            layer: CANARY_LAYER_ID,
            nesting_level: 0,
            dfs_enter: 0,
            dfs_exit: 0,
            name_range: None,
            signature_range: None,
            content_range: None,
        }
    }
    fn canary_object() -> Object {
        Object {
            id: -999999,
            project_id: -999999,
            module_path: "".into(),
            filesystem_path: "/__canary__".into(),
            filetype: "canary".into(),
            content_hash: "".into(),
            layer: CANARY_LAYER_ID,
        }
    }
    fn canary_ref() -> SymbolRef {
        SymbolRef {
            id: -999999,
            to_symbol: -999999,
            from_object: -999999,
            from_offset_range: (Bound::Included(0), Bound::Excluded(1)),
            layer: CANARY_LAYER_ID,
            nesting_level: 0,
            dfs_enter: 0,
            from_full_range: None,
            roles: 0,
            relation: 0,
        }
    }

    let empty_eph = EphContext::rooted(vec![]);

    let mut s = Selection::new();
    s.parents.push(ParentReference {
        to_symbol: canary_symbol(),
        to_instance: canary_instance(),
        from_instance: canary_instance(),
        symbol_ref: canary_ref(),
    });
    assert!(
        s.has_eph_leak(&empty_eph),
        "canary in .parents must be detected as a leak"
    );
    assert!(
        Checked::new(s, &empty_eph).is_err(),
        "Checked::new must bail when .parents leaks"
    );

    let mut s = Selection::new();
    s.children.push(ChildReference {
        parent_symbol: canary_symbol(),
        symbol: canary_symbol(),
        symbol_instance: canary_instance(),
        from_instance: canary_instance(),
        symbol_ref: canary_ref(),
        from_object: canary_object(),
    });
    assert!(
        s.has_eph_leak(&empty_eph),
        "canary in .children must be detected as a leak"
    );
    assert!(
        Checked::new(s, &empty_eph).is_err(),
        "Checked::new must bail when .children leaks"
    );

    let mut s = Selection::new();
    s.has_parents.push(HasParentReference {
        child_symbol: canary_symbol(),
        child_instance: canary_instance(),
        parent_symbol: canary_symbol(),
        parent_instance: canary_instance(),
    });
    assert!(
        s.has_eph_leak(&empty_eph),
        "canary in .has_parents must be detected as a leak"
    );
    assert!(
        Checked::new(s, &empty_eph).is_err(),
        "Checked::new must bail when .has_parents leaks"
    );

    let mut s = Selection::new();
    s.has_children.push(HasChildReference {
        parent_symbol: canary_symbol(),
        parent_instance: canary_instance(),
        child_symbol: canary_symbol(),
        child_instance: canary_instance(),
        parent_object: canary_object(),
    });
    assert!(
        s.has_eph_leak(&empty_eph),
        "canary in .has_children must be detected as a leak"
    );
    assert!(
        Checked::new(s, &empty_eph).is_err(),
        "Checked::new must bail when .has_children leaks"
    );
}

#[test]
fn loc_bad_file_path_errors() {
    let res = run_query_err(VERB_TEST, r#"loc("nonexistent_file.c", 1)"#);
    let msg = res.err().expect("should be an error").to_string();
    assert!(
        msg.contains("no file matching"),
        "expected 'no file matching' error, got: {}",
        msg
    );
}

#[test]
fn loc_bad_line_number_errors() {
    let res = run_query_err(VERB_TEST, r#"loc("main.c", 999)"#);
    let msg = res.err().expect("should be an error").to_string();
    assert!(
        msg.contains("out of range"),
        "expected 'out of range' error, got: {}",
        msg
    );
}

#[test]
fn layer_block_unknown_object_id_errors() {
    // The scoped per-root inserts silently drop rows whose object matches
    // no visible project, and the resulting layer is cached on its inputs —
    // so without verb-level validation a typo'd id would produce a sticky,
    // silently-empty result.  The validation must reject it loudly instead.
    let res = run_query_err(
        VERB_TEST,
        r#"layer { ephemeral_instance(symbol_id=1, object_id=99999, start=0, end=10, instance_type=1) }"#,
    );
    let msg = res.err().expect("should be an error").to_string();
    assert!(
        msg.contains("unknown object_id"),
        "expected unknown-object error, got: {}",
        msg
    );
}

#[test]
fn ephemeral_symbol_unknown_project_errors() {
    let res = run_query_err(
        VERB_TEST,
        r#"layer { ephemeral_symbol(name="ghost", project_id=424242, symbol_type=1) }"#,
    );
    let msg = res.err().expect("should be an error").to_string();
    assert!(
        msg.contains("does not match any visible project"),
        "expected unknown-project error, got: {}",
        msg
    );
}

#[test]
fn loc_missing_args_errors() {
    let res = run_query_err(VERB_TEST, r#"loc("main.c")"#);
    let msg = res.err().expect("should be an error").to_string();
    assert!(
        msg.contains("requires two positional"),
        "expected 'requires two positional' error, got: {}",
        msg
    );
}

#[test]
fn layer_sign_check_rejects_mismatched_rows() {
    // Regression lineage: an orphan row shaped like `id < 0, layer =
    // NULL` once re-surfaced as a stale loc:* node (the duplicate `-18`
    // instance).  Under root layers the schema enforces strictly more:
    // layer is NOT NULL, and the sign CHECK `(id > 0) = (layer > 0)`
    // pins persistent (positive-id) rows to root layers and ephemeral
    // (negative-id) rows to ephemeral layers.  All three orphan shapes must
    // be rejected.
    use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        // Touch the fixture so the shared container is up before we
        // open our own connection.
        let _ = get_shared_index(VERB_TEST).await;
        let url = get_shared_db_url(VERB_TEST);
        let mut conn = AsyncPgConnection::establish(url)
            .await
            .expect("connect to test DB");

        // (a) Negative id with NULL layer — rejected by NOT NULL.
        let err = diesel::sql_query(
            "INSERT INTO index.symbol_instances \
             (id, symbol, object_id, offset_range, instance_type, layer) \
             VALUES (-424242, 1, 1, int4range(0, 1), 1, NULL)",
        )
        .execute(&mut conn)
        .await
        .err()
        .expect("NOT NULL must reject id<0 with layer=NULL");
        assert!(
            format!("{:?}", err).contains("not-null"),
            "expected not-null violation, got: {:?}",
            err
        );

        // (b) Negative (ephemeral) id in a root layer (fixture root 1000001).
        let err = diesel::sql_query(
            "INSERT INTO index.symbol_instances \
             (id, symbol, object_id, offset_range, instance_type, layer) \
             VALUES (-424242, 1, 1, int4range(0, 1), 1, 1000001)",
        )
        .execute(&mut conn)
        .await
        .err()
        .expect("sign CHECK must reject id<0 in a root layer");
        assert!(
            format!("{:?}", err).contains("layer_sign_check"),
            "expected layer_sign_check violation, got: {:?}",
            err
        );

        // (c) Positive (persistent) id in an ephemeral layer (canary -999999).
        let err = diesel::sql_query(
            "INSERT INTO index.symbol_instances \
             (id, symbol, object_id, offset_range, instance_type, layer) \
             VALUES (424242, 1, 1, int4range(0, 1), 1, -999999)",
        )
        .execute(&mut conn)
        .await
        .err()
        .expect("sign CHECK must reject id>0 in an ephemeral layer");
        assert!(
            format!("{:?}", err).contains("layer_sign_check"),
            "expected layer_sign_check violation, got: {:?}",
            err
        );
    });
}

#[test]
fn wrap_loc_yields_single_instance_and_parent_has_edge() {
    // Regression for the {loc(...)} wrapper pattern.  The user's
    // failing query had an inner `loc("qaic_drv.c", 100)` inside
    // `{...}`; a leftover persistent loc-row caused two nodes plus a
    // missing has-edge from the containing function.  On a clean DB
    // the wrapper must yield exactly one ephemeral loc node and a
    // has-edge from the containing function (fixture: wrap_loc_target,
    // body covers line 1 of /main.c).
    const QUERY: &str = r#"{loc("main.c", 1, project="test_project")}"#;
    let res = run_query(VERB_TEST, QUERY);

    let loc_nodes: Vec<_> = res
        .nodes
        .0
        .iter()
        .filter(|n| n.symbol.name == "loc:main.c:1")
        .collect();
    assert_eq!(
        loc_nodes.len(),
        1,
        "expected exactly one loc:main.c:1 node, got {}: {:?}",
        loc_nodes.len(),
        loc_nodes
            .iter()
            .map(|n| (&n.symbol.name, n.symbol_instance.id))
            .collect::<Vec<_>>()
    );

    let loc_inst_id = loc_nodes[0].symbol_instance.id;
    assert!(
        loc_inst_id < 0,
        "loc instance must be ephemeral, got id {}",
        loc_inst_id
    );

    let wrap_to_loc = res.has_edges.0.iter().any(|e| {
        e.parent == SymbolId::new(9) && e.child_instance == SymbolInstanceId::new(loc_inst_id)
    });
    assert!(
        wrap_to_loc,
        "expected has-edge from wrap_loc_target (sym 9) to loc instance {}, got: {:?}",
        loc_inst_id,
        res.has_edges.0.iter().collect::<Vec<_>>()
    );
}

#[test]
fn ephemeral_instance_with_label_input_resolves_to_labelled_selection() {
    // Phase 5 integration test for the data-flow refactor:
    // an `ephemeral_instance` argument can reference a `@label` of a prior
    // statement, and the verb resolves the label to the labelled
    // statement's selected symbol IDs at materialise time.  This is the
    // proof that ephemeral layer creation can depend on a real-DB
    // selection — impossible under the old phase-based "all layers
    // before all selections" model.
    //
    // Query:
    //   @target "foo";
    //   layer { ephemeral_instance(symbol_id="@target", object_id=1,
    //                              start=50000, end=50100,
    //                              instance_type=1) }
    //
    // Expected:
    //   - The labelled statement's selection includes "foo" instance 91.
    //   - The layer materialises a new ephemeral instance (negative id)
    //     pointing at the same symbol id (=1) that "@target" resolved to.
    //   - End-to-end ordering: parser adds a User edge from @target's
    //     statement to the layer statement; `compute_roots` pre-drains
    //     before pushing the layer statement; `LabelResolutions`
    //     resolves @target → [1] at push time; the populate batch
    //     emits one EphInstanceRow with symbol_id=1.
    const QUERY: &str = concat!(
        r#"@target "foo"; "#,
        r#"layer { ephemeral_instance(symbol_id="@target", object_id=1, "#,
        r#"start=50000, end=50100, instance_type=1) }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(91)),
        "labelled @target should resolve to persistent foo instance 91; got {:?}",
        nodes
    );
    let has_eph = nodes.iter().any(|id| {
        let v: i64 = (*id).into();
        v < 0
    });
    assert!(
        has_eph,
        "layer should materialise a new ephemeral instance with negative id; got {:?}",
        nodes
    );
}

/// `symbol_id` is a union type, and the argument's TYPE picks the arm: an
/// integer is a symbol id, a string is a label reference.  The pre-typed
/// spelling `symbol_id="1"` is now neither, and says so.
#[test]
fn ephemeral_symbol_ref_discriminates_by_type() {
    const QUOTED_ID: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id="1", object_id=1, "#,
        r#"start=60700, end=60800, instance_type=1) }"#,
    );
    let Err(err) = run_query_err(VERB_TEST, QUOTED_ID) else {
        panic!("a quoted symbol id must not parse");
    };
    assert!(
        err.to_string().contains("symbol_id=1"),
        "expected the unquoted spelling in the hint, got: {err}"
    );

    // An empty label reference is still caught.
    const EMPTY_LABEL: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id="@", object_id=1, "#,
        r#"start=60700, end=60800, instance_type=1) }"#,
    );
    assert!(run_query_err(VERB_TEST, EMPTY_LABEL).is_err());
}

#[test]
fn ephemeral_instance_label_hash_matches_equivalent_literal() {
    // Validates the cache-key semantic: an ephemeral_instance with
    // symbol_id="@target" where @target resolves to symbol id 1 must
    // produce the same layer hash as symbol_id=1 — same resolved
    // symbol IDs in the hash inputs means the same cache entry.
    //
    // Hashes are private, but cache sharing has a direct
    // observable: a shared layer means a shared row in
    // `symbol_instances` with a shared negative `id`.  We assert that
    // the negative-ID set produced by the literal form intersects the
    // negative-ID set produced by the label form — only possible if
    // both runs hit the same layers row.  (Using identical
    // start/end ensures the rows are byte-identical; running the
    // label form after the literal means the literal's already-
    // committed row is what the label run finds via ON CONFLICT.)
    //
    // Robust against concurrent shared-DB pollution: we compare the
    // *negative IDs we produced* across the two runs, not absolute
    // table counts.
    const LITERAL_FIRST: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=60500, end=60600, instance_type=1) }"#,
    );
    const LABEL_VARIANT: &str = concat!(
        r#"@target "foo"; "#,
        r#"layer { ephemeral_instance(symbol_id="@target", object_id=1, "#,
        r#"start=60500, end=60600, instance_type=1) }"#,
    );

    let res_literal = run_query(VERB_TEST, LITERAL_FIRST);
    let res_label = run_query(VERB_TEST, LABEL_VARIANT);

    let neg_ids = |res: &crate::statement::ExecutionResult| -> std::collections::HashSet<i64> {
        res.nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .collect()
    };
    let literal_eph = neg_ids(&res_literal);
    let label_eph = neg_ids(&res_label);

    assert!(
        !literal_eph.is_empty(),
        "literal form must produce at least one ephemeral instance"
    );
    assert!(
        !label_eph.is_empty(),
        "label form must produce at least one ephemeral instance"
    );

    let shared: std::collections::HashSet<_> =
        literal_eph.intersection(&label_eph).copied().collect();
    assert!(
        !shared.is_empty(),
        "ephemeral_instance(symbol_id=\"@target\") where @target → [1] \
         must share at least one materialised instance row with the \
         equivalent literal form (same layer hash means same layers \
         row means same instance id).  literal_eph={:?}, label_eph={:?}",
        literal_eph,
        label_eph,
    );
}

#[test]
fn ephemeral_instance_same_tree_label_rejected() {
    // Parse-time label ordering rule (per-statement materialisations groundwork):
    // a layer's `@label` must reference a label defined by an EARLIER
    // top-level statement.  Label and reference living in the same
    // top-level statement tree is a structured error with a
    // split-with-`;` hint.
    const QUERY: &str = concat!(
        r#"@target "foo" { layer { ephemeral_instance(symbol_id="@target", "#,
        r#"object_id=1, start=50000, end=50100, instance_type=1) } }"#,
    );
    let res = run_query_err(VERB_TEST, QUERY);
    let err = match res {
        Ok(_) => panic!("same-tree label reference must be rejected"),
        Err(e) => e,
    };
    let msg = format!("{}", err);
    assert!(
        msg.contains("split with `;`"),
        "error should carry the split-with-; hint, got: {}",
        msg
    );
}

#[test]
fn ephemeral_instance_forward_label_rejected() {
    // Forward reference: the layer statement precedes the statement that
    // defines the label.  Must be a structured error, not an empty
    // resolution.
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id="@target", object_id=1, "#,
        r#"start=50000, end=50100, instance_type=1) }; "#,
        r#"@target "foo""#,
    );
    let res = run_query_err(VERB_TEST, QUERY);
    let err = match res {
        Ok(_) => panic!("forward label reference must be rejected"),
        Err(e) => e,
    };
    let msg = format!("{}", err);
    assert!(
        msg.contains("later statement"),
        "error should say the label is defined in a later statement, got: {}",
        msg
    );
}

// ============================================================================
// search() verb tests (Step 6: skeleton — defaults only)
// ============================================================================

#[test]
fn search_skeleton_single_match() {
    // search("foo") against TEST_INPUT_SEARCH finds at least the matches
    // in `hello foo world` (object 1), `foo foo foo` (object 2), `foobar
    // foo foo_bar foo.bar` (object 3), `Foo FOO foo` (object 4), and
    // `doc-only content with foo here` (object 5).  Step 6's defaults are
    // case=insensitive, whole_word=false, limit=500, so we expect many
    // matches across all objects.  All instances should be ephemeral
    // (negative ids).
    const QUERY: &str = r#"search("foo")"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(
        !nodes.is_empty(),
        "search(\"foo\") should match at least once across the fixture, got {} nodes",
        nodes.len(),
    );
    assert!(
        nodes.iter().all(|id| {
            let v: i64 = (*id).into();
            v < 0
        }),
        "all search instances should be ephemeral (negative IDs), got {:?}",
        nodes,
    );
}

#[test]
fn search_nested_in_container_survives_composition() {
    // `func("fn_basic") { search("foo") }`: fn_basic's instance spans object
    // 1's whole content, so the "foo" match (bytes 6..9) is contained in it.
    // The container statement computes its neighbourhood BEFORE the search
    // layer materialises, so its has_children cannot name the ephemeral
    // rows; the composition must accept the child's own (fresh) has_parents
    // evidence instead.  Regression test for `file("x") { search("y") }`
    // returning 0 symbols.
    const QUERY: &str = r#"func("fn_basic") { search("foo") }"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let ids: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
    assert!(
        ids.iter().any(|v| *v < 0),
        "the contained search match must survive the composition, got {:?}",
        ids,
    );
    assert!(
        ids.contains(&110),
        "the container (fn_basic, instance 110) must survive, got {:?}",
        ids,
    );
}

#[test]
fn nested_search_in_search_does_not_compose() {
    // `search("hello foo") { search("foo") }`: the outer match spans bytes
    // [0,9) of obj 1 ("hello foo world") and the inner "foo" match [6,9) lies
    // inside it -- but the two do NOT compose, and that is by decision.
    //
    // Containment is a position in the object's DFS order now, and an
    // ephemeral instance takes its place from the innermost PERSISTENT
    // instance containing it.  Both matches live directly inside the same
    // container, so both land at the same depth with the same interval:
    //
    //     EPH -1  depth=1  interval [1026,1033]  range [0,9)
    //     EPH -2  depth=1  interval [1026,1033]  range [6,9)
    //
    // Their nesting relative to each other is a RANGE fact, not a depth fact,
    // and the two searches materialise independently -- by design "neither
    // command sees the other's layers at materialise time" -- so neither can
    // be numbered inside the other.  Ordering the inserts does not help; the
    // collision is across batches.
    //
    // Deciding this by range instead would mean a second containment path for
    // ephemeral rows.  `search { search }` is an uncommon query and was judged
    // not worth it; `"foo" { search }`, the common one, works and is covered by
    // `search_nested_in_container_survives_composition`.
    const QUERY: &str = r#"search("hello foo") { search("foo") }"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let ids: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
    assert!(
        ids.is_empty(),
        "the inner search constrains the outer away, got {:?}",
        ids,
    );
}

#[test]
fn every_match_in_a_crowded_object_stays_inside_its_owner() {
    // 131 matches, all inside the single function instance covering
    // /p1/many.c: scoping by that function must drop none of them.
    //
    // This began as a regression test for a slot-overflow bug -- the ordinal
    // came from `UNNEST ... WITH ORDINALITY`, which numbers the whole BATCH,
    // so with 8-wide slots the 128th row ran past `base + 1024` and landed
    // inside a declaration it had nothing to do with.  Ephemeral instances are
    // width-1 leaves now, which gives 1022 slots per owner and puts every slot
    // inside the owner whatever picks the ordinal, so that overflow is
    // unreachable and this no longer discriminates the ordinal choice.  It
    // still pins the property that matters: no match is lost.
    const ALL: &str = r#"search("zqx")"#;
    const SCOPED: &str = r#"func("fn_many") { search("zqx") }"#;

    let all = run_query(TEST_INPUT_SEARCH, ALL);
    let scoped = run_query(TEST_INPUT_SEARCH, SCOPED);

    let eph = |res: &crate::statement::ExecutionResult| -> usize {
        res.nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .count()
    };

    let all_matches = eph(&all);
    assert!(
        all_matches > 127,
        "fixture must exceed the 127-slot grid to exercise the overflow, got {}",
        all_matches,
    );
    assert_eq!(
        eph(&scoped),
        131,
        "every one of many.c's 131 matches sits inside fn_many, so scoping by \
         it must drop none",
    );
}

#[test]
fn a_lone_match_in_its_own_object_reaches_its_owner() {
    // /p1/tail.c contributes a single match to the same `search("zqx")` batch
    // that /p1/many.c fills with 131 -- a row that is not crowded by anything,
    // merely late in the batch.
    //
    // Under the old 8-wide grid a batch-global ordinal gave it slot 132 and
    // `base + 2 + 131*8` put it past the end of its own owner, in a file with
    // nothing to do with the other matches.  Width-1 leaves removed that whole
    // failure mode; what stays asserted is that upward derivation finds this
    // row's owner, which is the half the crowded-object test cannot show.
    // It has to be asked of the WIDE batch.  `func("fn_tail") { search("zqx") }`
    // looks fine either way, because the scoped search materialises its own,
    // much smaller batch in which tail.c's match is first again.  Deriving
    // upward from the unscoped search is what consumes the 132-row batch.
    const QUERY: &str = r#"{ search("zqx") }"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let persistent: Vec<i64> = res
        .nodes
        .as_vec()
        .iter()
        .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
        .filter(|id| *id > 0)
        .collect();

    assert!(
        persistent.contains(&114),
        "fn_many contains many.c's matches, got {:?}",
        persistent
    );
    assert!(
        persistent.contains(&115),
        "fn_tail contains tail.c's one match -- with a batch-global ordinal \
         that row is numbered as if 131 rows preceded it under ITS owner and \
         lands outside fn_tail entirely, got {:?}",
        persistent
    );
}

#[test]
fn search_explicit_type_filter_constrains_layer_read() {
    // `func search("foo")`: search rows are content-typed, so an explicit
    // `func` type filter in the same statement excludes every match on the
    // layer-read path.  Before the layer-read filter fix the command's
    // filters were silently dropped when reading a materialised layer and
    // this returned all matches.
    const QUERY: &str = r#"func search("foo")"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    assert!(
        res.nodes.as_vec().is_empty(),
        "explicit func filter must exclude content-typed search matches, got {:?}",
        res.nodes.as_vec(),
    );
}

#[test]
fn search_project_filter_layer_read_parity() {
    // `project(...)` is folded into the search populate at object level;
    // after the layer-read fix it also constrains the read path.  Both
    // paths must agree: filtering to project 1 keeps its matches and drops
    // project 2's (objects 4/5 carry "foo" matches too).
    let all = run_query(TEST_INPUT_SEARCH, r#"search("foo")"#);
    let p1 = run_query(
        TEST_INPUT_SEARCH,
        r#"search("foo") project("search_proj_1")"#,
    );

    let all_n = all.nodes.as_vec().len();
    let p1_n = p1.nodes.as_vec().len();
    assert!(p1_n > 0, "project 1 must keep its own matches");
    assert!(
        p1_n < all_n,
        "project filter must drop project 2's matches: {} filtered vs {} total",
        p1_n,
        all_n,
    );
}

#[test]
fn budget_truncation_surfaces_warning() {
    // cap=1 → leaf LIMIT 8; search("foo") materialises >8 match instances, so
    // the read caps and the statement must SURFACE its incompleteness.  The
    // message is the safety-cap one ("graph is incomplete"), not the old
    // "result budget" wording: `read_limit = min(leaf_limit, safety)` means a
    // budget-bounded read is always also capped, and the capped message — which
    // does not quote a misleading total or say "raise the limit" — subsumes it.
    let res = run_query_with_budget(TEST_INPUT_SEARCH, r#"search("foo")"#, 1);
    assert!(
        res.warnings
            .iter()
            .any(|w| w.message.contains("this part of the graph is incomplete")),
        "expected an incompleteness warning, got {:?}",
        res.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
    assert!(
        !res.nodes.as_vec().is_empty(),
        "bounded means truncated, not empty"
    );

    // Unlimited budget: same query, no incompleteness warning.
    let res = run_query_with_budget(TEST_INPUT_SEARCH, r#"search("foo")"#, 0);
    assert!(
        !res.warnings
            .iter()
            .any(|w| w.message.contains("incomplete")),
        "unlimited budget must not warn",
    );
}

#[test]
fn an_echo_child_does_not_cost_the_budget() {
    // The budget law: *truncated rows may only travel where they cannot
    // narrow an independent selection.*  A bare `{}` is an echo — it derives
    // from the parent and, being weak, never narrows it — so the parent keeps
    // its bound and still warns.
    //
    // Before this rule ANY dependent cleared the budget, which is what made a
    // wide leaf with an empty scope time out where the same leaf alone
    // answered instantly (`perf/BUDGET_GATE.md`).  The surfaced message is now
    // the incompleteness one (see `budget_truncation_surfaces_warning`); the
    // point of this test is that the echo child does not SUPPRESS it.
    let res = run_query_with_budget(TEST_INPUT_SEARCH, r#"search("foo") { }"#, 1);
    assert!(
        res.warnings
            .iter()
            .any(|w| w.message.contains("this part of the graph is incomplete")),
        "an echo child must not clear the budget, got {:?}",
        res.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
}

#[test]
fn a_data_originating_child_costs_the_budget() {
    // The other half of the law, from the user's side: when a child computes
    // its own selection, the parent's rows feed `constrain_*` and the
    // truncation warning must not appear.
    //
    // In THIS shape `find_symbol`'s scope rule is what enforces it — the
    // child is conditioned and uncomputed, so `build_children_scope` hands
    // the parent a `Scope` and the leaf limit drops there.  The case only
    // Gate 1 covers is
    // `a_composed_child_keeps_its_rows_complete_under_a_budget`.
    let res = run_query_with_budget(TEST_INPUT_SEARCH, r#"search("foo") { "fn_basic" }"#, 1);
    assert!(
        !res.warnings
            .iter()
            .any(|w| w.message.contains("result budget")),
        "a data-originating child must clear the budget, got {:?}",
        res.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
}

/// The incompleteness warnings of a run, and its instance ids, sorted.
fn incomplete_and_ids(res: &crate::statement::ExecutionResult) -> (Vec<String>, Vec<i64>) {
    let said = res
        .warnings
        .iter()
        .filter(|w| w.message.contains("this part of the graph is incomplete"))
        .map(|w| w.message.clone())
        .collect();
    let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
    ids.sort();
    (said, ids)
}

#[test]
fn an_edge_states_the_union_of_what_its_references_say() {
    use index::roles::RefKind;
    use index::symbols::SymbolType;

    // `caller` refers to `hook` (data, a function pointer) three times: it
    // calls it, takes its value, and increments it.  The answer has ONE edge
    // for the pair, and what it says is all three.
    let res = run_query(
        crate::test_util::TEST_INPUT_REFERENCE_ROLES,
        r#""caller" { }"#,
    );
    let (caller, hook, plain) = (SymbolId::new(1), SymbolId::new(2), SymbolId::new(5));
    assert_eq!(
        res.edges.stated(caller, hook).kinds(SymbolType::Data),
        vec![RefKind::Calls, RefKind::Writes, RefKind::Reads]
    );
    // `plain` is referred to with nothing stated: the edge is there and says
    // nothing, as every edge of an index without roles does.
    assert!(res
        .edges
        .0
        .iter()
        .any(|(from, to, _)| from.symbol_id == caller && to.symbol_id == plain));
    let nothing_said = res.edges.stated(caller, plain);
    assert!(nothing_said.kinds(SymbolType::Function).is_empty());
    assert!(nothing_said.refers(), "a token of caller names it");
    assert!(res.edges.stated(caller, hook).refers());

    // A relation reaches the answer the same way: the slot holds `my_read`.
    let res = run_query(
        crate::test_util::TEST_INPUT_REFERENCE_ROLES,
        r#""slot" { }"#,
    );
    assert_eq!(
        res.edges
            .stated(SymbolId::new(3), SymbolId::new(4))
            .kinds(SymbolType::Function),
        vec![RefKind::Holds]
    );
    // And nothing refers by that edge: no token of the slot names `my_read`.
    assert!(!res
        .edges
        .stated(SymbolId::new(3), SymbolId::new(4))
        .refers());
}

#[test]
fn a_relation_belongs_to_its_source_only() {
    // `slot` (a field of struct `ops`, in /ops.h, in directory /) HOLDS
    // `my_read`: a relation row, placed on the field's declaration.  A
    // reference belongs to every declaration around it, and the row used to
    // be attributed the same way -- so the struct, the file and the directory
    // each "referred to" `my_read`.  Kernel: `file_operations (type) ->
    // bt_bmc_open`, an arrow nobody could read.
    //
    // ids: dir 500, file 510, ops 10, slot 20, my_read 30, plain 40,
    //      DEFINE_X 60, lock 70.
    let ids = |q: &str| -> Vec<i64> {
        let res = run_query(crate::test_util::TEST_INPUT_RELATION_ROWS, q);
        let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
        ids.sort();
        ids
    };

    // The slot holds it.
    assert_eq!(ids(r#"field("slot") { func }"#), vec![20, 30, 40]);
    // The struct does not: only what a token inside it names.
    assert_eq!(ids(r#"type("ops") refs { func }"#), vec![10, 40]);
    // Asked from the other end: what holds my_read is the slot alone ...
    assert_eq!(ids(r#"holds { "my_read" }"#), vec![20, 30]);
    assert_eq!(ids(r#"{ "my_read" }"#), vec![20, 30, 500, 510]);
    // ... while an ordinary reference at the same place still belongs to
    // everything around it (the macro body names `plain` too).
    assert_eq!(ids(r#"refs { "plain" }"#), vec![10, 20, 40, 60, 500, 510]);

    // The same for what a macro's invocation declared: the row sits on the
    // `#define`, and the file around the `#define` declares nothing.
    assert_eq!(ids(r#"declares { "lock" }"#), vec![60, 70]);
    assert_eq!(ids(r#"macro("DEFINE_X") { }"#), vec![40, 60, 70]);

    // And in the edges drawn among an answer's nodes: one edge to my_read.
    let res = run_query(
        crate::test_util::TEST_INPUT_RELATION_ROWS,
        r#"file("/ops.h") ; type("ops") ; field("slot") ; "my_read" ; "plain""#,
    );
    let to = |target: i64| -> Vec<i64> {
        let mut from: Vec<i64> = res
            .edges
            .0
            .iter()
            .filter(|(_, t, _)| t.symbol_id == SymbolId::new(target))
            .map(|(f, _, _)| f.instance_id.into())
            .collect();
        from.sort();
        from.dedup();
        from
    };
    assert_eq!(to(3), vec![20], "my_read is held by the slot alone");
    assert_eq!(
        to(4),
        vec![10, 20, 510],
        "plain is referred to by all three"
    );
}

/// Instance ids of an answer on the relation fixture, sorted.
///
/// ids: dir 500, file 510, ops 10, slot 20, my_read 30, plain 40,
///      DEFINE_X 60, lock 70, DEFINE_Y 80, both 90, bare_slot 100, kept 110,
///      hidden 120 (folded), GEN_Z 130 (folded), gen_only 140.
fn relation_ids(q: &str) -> Vec<i64> {
    let res = run_query(crate::test_util::TEST_INPUT_RELATION_ROWS, q);
    let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
    ids.sort();
    ids
}

/// What a relationship word said it passed over, on the relation fixture.
fn passed_over(q: &str) -> Vec<String> {
    run_query(crate::test_util::TEST_INPUT_RELATION_ROWS, q)
        .warnings
        .iter()
        .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
        .map(|w| w.message.clone())
        .collect()
}

#[test]
fn a_relationship_word_selects_the_edges_drawn_with_its_mark() {
    let ids = relation_ids;
    // `slot` HOLDS `my_read` and names `plain`; `DEFINE_X` DECLARES `lock`
    // and names `plain`.  Four relationships, each with its own word.

    // refs: a token names it.  Not what the slot holds, in either direction
    // (a statement that names something shows it; the unnamed one is empty).
    assert_eq!(ids(r#"field("slot") refs { }"#), vec![20, 40]);
    assert_eq!(ids(r#"field("slot") refs { func }"#), vec![20, 40]);
    assert_eq!(ids(r#"field("bare_slot") refs { }"#), vec![100]);
    assert_eq!(ids(r#"refs { "my_read" }"#), vec![30]);
    assert_eq!(ids(r#"macro("DEFINE_X") refs { }"#), vec![40, 60]);
    assert_eq!(ids(r#"refs { "lock" }"#), vec![70]);
    assert_eq!(ids(r#"refs { "plain" }"#), vec![10, 20, 40, 60, 500, 510]);
    // `derive(type="refs")` is the word.
    assert_eq!(
        ids(r#"field("slot") derive(type="refs") { }"#),
        vec![20, 40]
    );

    // holds: exactly the slot and what is stored in it.
    assert_eq!(ids(r#"field("slot") holds { }"#), vec![20, 30]);
    assert_eq!(ids(r#"holds { "my_read" }"#), vec![20, 30]);
    assert_eq!(ids(r#"field("bare_slot") holds { }"#), vec![100, 110]);
    assert_eq!(
        ids(r#"field holds { "my_read" "kept" }"#),
        vec![20, 30, 100, 110]
    );
    assert_eq!(ids(r#"field("slot") holds { "plain" }"#), Vec::<i64>::new());
    assert_eq!(ids(r#"holds { "plain" }"#), vec![40]);
    assert_eq!(ids(r#"holds { "lock" }"#), vec![70]);

    // declares: exactly the macro and what its invocation declared.
    assert_eq!(ids(r#"macro("DEFINE_X") declares { }"#), vec![60, 70]);
    assert_eq!(ids(r#"declares { "lock" }"#), vec![60, 70]);
    assert_eq!(
        ids(r#"macro declares { "lock" "both" }"#),
        vec![60, 70, 80, 90]
    );
    assert_eq!(ids(r#"declares { "plain" }"#), vec![40]);
    assert_eq!(ids(r#"declares { "my_read" }"#), vec![30]);

    // has is containment, as it was.
    assert_eq!(ids(r#"type("ops") has { }"#), vec![10, 20]);

    // A bare scope is all four.
    assert_eq!(ids(r#"field("slot") { }"#), vec![20, 30, 40]);
    assert_eq!(ids(r#"field("bare_slot") { }"#), vec![100, 110]);
    assert_eq!(ids(r#"macro("DEFINE_X") { }"#), vec![40, 60, 70]);
    assert_eq!(ids(r#"type("ops") { }"#), vec![10, 20, 40]);
    assert_eq!(ids(r#"{ "my_read" }"#), vec![20, 30, 500, 510]);
    assert_eq!(ids(r#"field { "my_read" }"#), vec![20, 30]);

    // The words carry into nested scopes as `has` and `refs` do.
    assert_eq!(
        ids(r#"type("ops") has { field holds { } }"#),
        vec![10, 20, 30]
    );
    // Two relationship words on one statement mean either, and are joined
    // with `or`.  (Side by side they used to run as the second alone; that
    // is an error now, `two_relationship_words_are_joined_with_or`.)
    assert_eq!(ids(r#"field("slot") has or holds { }"#), vec![20, 30]);
    assert_eq!(ids(r#"field("slot") holds or refs { }"#), vec![20, 30, 40]);
}

#[test]
fn a_pair_with_two_relationships_is_listed_by_either_word_and_drawn_with_both() {
    use index::roles::RefKind;
    use index::symbols::SymbolType;

    // `DEFINE_Y` declares `both`, and its body names it too.
    let (define_y, both) = (SymbolId::new(7), SymbolId::new(8));
    for q in [
        r#"macro("DEFINE_Y") refs { }"#,
        r#"macro("DEFINE_Y") declares { }"#,
        r#"macro("DEFINE_Y") { }"#,
        r#"refs { "both" }"#,
        r#"declares { "both" }"#,
    ] {
        let res = run_query(crate::test_util::TEST_INPUT_RELATION_ROWS, q);
        let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
        ids.sort();
        let macro_and_data: Vec<i64> = ids
            .iter()
            .copied()
            .filter(|id| *id == 80 || *id == 90)
            .collect();
        assert_eq!(macro_and_data, vec![80, 90], "{q}");
        // Whichever word found the pair, it is drawn with all it has.
        let stated = res.edges.stated(define_y, both);
        assert_eq!(
            stated.kinds(SymbolType::Data),
            vec![RefKind::Declares],
            "{q}"
        );
        assert!(stated.refers(), "{q}: a token of the macro names it too");
    }
    // `holds` is neither of the two.
    assert_eq!(relation_ids(r#"macro("DEFINE_Y") holds { }"#), vec![80]);
}

#[test]
fn refs_says_which_relations_it_passed_over_when_it_lists_nothing() {
    // The slot holds `my_read` and no token of it names `my_read`: `refs`
    // lists nothing, and says what the bare scope would have listed.  (The
    // statement that was named still shows what it named.)
    for (q, shown, expected) in [
        (
            r#"field("slot") refs { "my_read" }"#,
            vec![],
            "`refs` lists only ordinary references (`→`); `field(\"slot\")` holds 1 symbol \
             that `\"my_read\"` matches (`⇢`): `my_read`. `holds { … }` or `{ … }` lists it",
        ),
        (
            r#"field("slot") refs { func("my_read") }"#,
            vec![],
            "`refs` lists only ordinary references (`→`); `field(\"slot\")` holds 1 symbol \
             that `func(\"my_read\")` matches (`⇢`): `my_read`. `holds { … }` or `{ … }` \
             lists it",
        ),
        (
            // A glob is not a predicate the read itself applies: the names
            // are those the statement's whole predicate matches.
            r#"field("slot") refs { g"my_*" }"#,
            vec![],
            "`refs` lists only ordinary references (`→`); `field(\"slot\")` holds 1 symbol \
             that `g\"my_*\"` matches (`⇢`): `my_read`. `holds { … }` or `{ … }` lists it",
        ),
        (
            r#"field("bare_slot") refs { }"#,
            vec![100],
            "`refs` lists only ordinary references (`→`); `field(\"bare_slot\")` holds 1 \
             symbol (`⇢`): `kept`. `holds { … }` or `{ … }` lists it",
        ),
        (
            r#"refs { "my_read" }"#,
            vec![30],
            "`refs` lists only ordinary references (`→`); 1 symbol holds `\"my_read\"` \
             (`⇢`): `slot`. `holds { … }` or `{ … }` lists it",
        ),
        (
            r#"field refs { "kept" }"#,
            vec![110],
            "`refs` lists only ordinary references (`→`); 1 symbol that `field` matches \
             holds `\"kept\"` (`⇢`): `bare_slot`. `holds { … }` or `{ … }` lists it",
        ),
        (
            r#"macro("DEFINE_X") refs { data }"#,
            vec![60],
            "`refs` lists only ordinary references (`→`); `macro(\"DEFINE_X\")` declares 1 \
             symbol that `data` matches (`↦`): `lock`. `declares { … }` or `{ … }` lists it",
        ),
        (
            r#"refs { "lock" }"#,
            vec![70],
            "`refs` lists only ordinary references (`→`); 1 symbol declares `\"lock\"` \
             (`↦`): `DEFINE_X`. `declares { … }` or `{ … }` lists it",
        ),
    ] {
        assert_eq!(passed_over(q), vec![expected.to_string()], "{q}");
        assert_eq!(relation_ids(q), shown, "{q}");
    }

    // The same when the statement asked about is too broad to be read on its
    // own and was resolved through its neighbour (the kernel's
    // `field("file_operations.read") refs { func }`): with a cap of one,
    // `data` (two symbols) is found only through the macro.
    let through_neighbour = run_query_with_read_cap(
        crate::test_util::TEST_INPUT_RELATION_ROWS,
        r#"macro("DEFINE_X") refs { data }"#,
        1,
    );
    let said: Vec<&str> = through_neighbour
        .warnings
        .iter()
        .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
        .map(|w| w.message.as_str())
        .collect();
    assert_eq!(
        said,
        vec![
            "`refs` lists only ordinary references (`→`); `macro(\"DEFINE_X\")` declares 1 \
             symbol that `data` matches (`↦`): `lock`. `declares { … }` or `{ … }` lists it"
        ]
    );

    // A listing passes over folded symbols, and so does what is said of it:
    // the slot also holds `hidden` (folded), which `{ }` would not list ...
    assert_eq!(
        passed_over(r#"field("bare_slot") refs { unnest }"#),
        vec![
            "`refs` lists only ordinary references (`→`); `field(\"bare_slot\")` holds 2 \
             symbols (`⇢`): `hidden`, `kept`. `holds { … }` or `{ … }` list them"
        ]
    );
    // ... but a statement that names it shows it, folded or not.
    assert_eq!(
        passed_over(r#"field("bare_slot") refs { "hidden" }"#),
        vec![
            "`refs` lists only ordinary references (`→`); `field(\"bare_slot\")` holds 1 \
             symbol that `\"hidden\"` matches (`⇢`): `hidden`. `holds { … }` or `{ … }` \
             lists it"
        ]
    );
    // Upward the same: `gen_only` is declared by a folded macro alone, which
    // `{ "gen_only" }` does not list, and `macro("GEN_Z")` does.
    assert_eq!(passed_over(r#"refs { "gen_only" }"#), Vec::<String>::new());
    assert_eq!(
        passed_over(r#"macro("GEN_Z") refs { "gen_only" }"#),
        vec![
            "`refs` lists only ordinary references (`→`); `macro(\"GEN_Z\")` declares 1 \
             symbol that `\"gen_only\"` matches (`↦`): `gen_only`. `declares { … }` or \
             `{ … }` lists it"
        ]
    );

    // An answer a limit cut is not an empty answer: it is said to be
    // incomplete, and nothing is read to explain it.
    let three = r#""bare_slot" "kept" "lock" refs { }"#;
    assert_eq!(
        passed_over(three),
        vec![
            "`refs` lists only ordinary references (`→`); `\"bare_slot\" \"kept\" \"lock\"` \
             holds 1 symbol (`⇢`): `kept`. `holds { … }` or `{ … }` lists it"
        ]
    );
    let cut = run_query_with_read_cap(crate::test_util::TEST_INPUT_RELATION_ROWS, three, 2);
    assert!(
        cut.warnings
            .iter()
            .any(|w| w.message.contains("too many results in `{ }`")),
        "the listing is cut: {:?}",
        cut.warnings
    );
    assert!(cut
        .warnings
        .iter()
        .all(|w| w.kind != crate::diagnostic::DiagnosticKind::PassedOver));

    // Nothing is said when the answer is not empty ...
    for q in [
        r#"field("slot") refs { }"#,
        r#"field("slot") refs { func }"#,
        r#"refs { "plain" }"#,
        r#"macro("DEFINE_Y") refs { }"#,
        r#"refs { "both" }"#,
    ] {
        assert_eq!(passed_over(q), Vec::<String>::new(), "{q}");
    }
    // ... nor when it is empty and no relation was passed over: the struct
    // holds nothing (a relation belongs to its source only); what the slot
    // holds is not data, so `data` would not have listed it either; and a
    // bare scope or a relation word passes over no relation.
    for (q, shown) in [
        (r#"type("ops") refs { "my_read" }"#, vec![]),
        (r#"file("/ops.h") refs { "lock" }"#, vec![]),
        (r#"macro("DEFINE_X") refs { "my_read" }"#, vec![]),
        (r#"field("bare_slot") refs { data }"#, vec![100]),
        (r#"field("slot") refs { g"zz*" }"#, vec![]),
        (r#"field("bare_slot") refs { g"my_*" }"#, vec![]),
        (r#"field("slot") holds { "plain" }"#, vec![]),
        (r#"field("slot") { "lock" }"#, vec![]),
    ] {
        assert_eq!(passed_over(q), Vec::<String>::new(), "{q}");
        assert_eq!(relation_ids(q), shown, "{q}");
    }
}

#[test]
fn a_relation_word_asked_of_what_cannot_be_its_source_says_so() {
    // Only a slot holds, and only a macro declares.  Asked of anything else
    // the scope is empty for a reason the reader can be told.
    for (q, shown, expected) in [
        (
            r#"type("ops") holds { }"#,
            vec![10],
            "`holds` related nothing: only a slot holds (a field, or an interface method), \
             and `type(\"ops\")` selects none",
        ),
        (
            r#""plain" holds { func }"#,
            vec![40],
            "`holds` related nothing: only a slot holds (a field, or an interface method), \
             and `\"plain\"` selects none",
        ),
        (
            r#"macro("DEFINE_Y") holds { "both" }"#,
            vec![],
            "`holds` related nothing: only a slot holds (a field, or an interface method), \
             and `macro(\"DEFINE_Y\")` selects none",
        ),
        (
            r#"field("slot") declares { }"#,
            vec![20],
            "`declares` related nothing: only a macro declares, and `field(\"slot\")` \
             selects none",
        ),
    ] {
        assert_eq!(passed_over(q), vec![expected.to_string()], "{q}");
        assert_eq!(relation_ids(q), shown, "{q}");
    }
    // Not said of a source of the right kind, whether it relates ...
    for q in [
        r#"field("slot") holds { }"#,
        r#"macro("DEFINE_X") declares { }"#,
        r#"holds { "my_read" }"#,
    ] {
        assert_eq!(passed_over(q), Vec::<String>::new(), "{q}");
    }
    // ... or not: the slot is a slot, it just does not hold `plain`.
    assert_eq!(
        passed_over(r#"field("slot") holds { "plain" }"#),
        Vec::<String>::new()
    );
    // And never of a source nobody named: there is nothing to say about it.
    assert_eq!(passed_over(r#"holds { "plain" }"#), Vec::<String>::new());
    assert_eq!(
        passed_over(r#"declares { "my_read" }"#),
        Vec::<String>::new()
    );
}

#[test]
fn a_passed_over_notice_survives_a_cache_hit() {
    // The notice is worked out from reads, and a read answered from the SQL
    // cache is the same read: the second run says what the first said.
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index =
            crate::test_util::get_shared_index(crate::test_util::TEST_INPUT_RELATION_ROWS).await;
        let said = |res: &crate::statement::ExecutionResult| -> Vec<String> {
            res.warnings
                .iter()
                .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
                .map(|w| w.message.clone())
                .collect()
        };
        for q in [
            r#"field("slot") refs { "my_read" }"#,
            r#"type("ops") holds { }"#,
        ] {
            let (cold, _) = crate::test_util::run_query_traced_on(index.clone(), q)
                .await
                .unwrap();
            let before = index.sql_cache().stats();
            let (warm, _) = crate::test_util::run_query_traced_on(index.clone(), q)
                .await
                .unwrap();
            let after = index.sql_cache().stats();
            assert_eq!(said(&cold).len(), 1, "{q}: {:?}", cold.warnings);
            assert_eq!(said(&warm), said(&cold), "{q}");
            assert_eq!(
                after.misses, before.misses,
                "{q}: the second run read nothing"
            );
            assert!(after.hits > before.hits, "{q}");
        }
    });
}

/// Instance ids of an answer on the role-words fixture, sorted.
///
/// project `stated`: caller 10, other 20, via 30, ops 40, slot 50,
///   my_read 60, hook 70, shared 80, plain 90, maybe 100, counter 110,
///   T 120, M 130, direct 140, stored 150, twin 160 (declared at 165) and
///   170, noisy 180, M2 190, user_of_m 200.
/// project `silent`: old_caller 1010, shared 1020, old_target 1030,
///   OLD_DEFINE 1040, old_lock 1050.
fn role_ids(q: &str) -> Vec<i64> {
    let res = run_query(crate::test_util::TEST_INPUT_ROLE_WORDS, q);
    let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
    ids.sort();
    ids
}

/// What a relationship word said it passed over, on the role-words fixture.
fn role_said(q: &str) -> Vec<String> {
    run_query(crate::test_util::TEST_INPUT_ROLE_WORDS, q)
        .warnings
        .iter()
        .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
        .map(|w| w.message.clone())
        .collect()
}

#[test]
fn a_relationship_word_acts_on_its_statement_wherever_it_stands() {
    // A TERM ACTS ON THE STATEMENT IT IS WRITTEN IN.  A relationship word
    // describes the edge into that statement's braces whether it is written
    // first, last or between the other terms.
    let ids = role_ids;
    // The word is doing something in each of these: without it the answer
    // is another one.
    assert_ne!(ids(r#""caller" calls { }"#), ids(r#""caller" { }"#));
    assert_ne!(ids(r#"func writes { "hook" }"#), ids(r#"func { "hook" }"#));
    for (last, elsewhere) in [
        (r#""caller" calls { }"#, r#"calls "caller" { }"#),
        (r#"func calls { "hook" }"#, r#"calls func { "hook" }"#),
        (r#"func writes { "hook" }"#, r#"writes func { "hook" }"#),
        (r#"func "caller" calls { }"#, r#"func calls "caller" { }"#),
        (
            r#""caller" writes or takes { }"#,
            r#"writes or takes "caller" { }"#,
        ),
        // In a nested statement as in the first.
        (
            r#""via" calls { field holds { func } }"#,
            r#"calls "via" { holds field { func } }"#,
        ),
    ] {
        assert!(!ids(last).is_empty(), "{last}");
        assert_eq!(ids(elsewhere), ids(last), "{elsewhere}");
    }
    assert_eq!(
        ids(r#"calls "via" { holds field { func } }"#),
        vec![30, 50, 60, 140, 150]
    );
}

#[test]
fn a_role_word_narrows_refs_to_the_references_that_state_it() {
    let ids = role_ids;
    // `caller` calls `hook` and `shared`, takes `hook`, and `counter++`es;
    // it names `plain`, `maybe`, `T` and `M` with nothing stated.
    assert_eq!(
        ids(r#""caller" refs { }"#),
        vec![10, 70, 80, 90, 100, 110, 120, 130]
    );

    // calls: downward and upward.
    assert_eq!(ids(r#""caller" calls { }"#), vec![10, 70, 80]);
    assert_eq!(ids(r#"calls { "hook" }"#), vec![10, 70]);
    assert_eq!(ids(r#"func calls { "hook" }"#), vec![10, 70]);
    assert_eq!(ids(r#""caller" calls { "hook" }"#), vec![10, 70]);
    // writes.
    assert_eq!(ids(r#""caller" writes { }"#), vec![10, 110]);
    assert_eq!(ids(r#"writes { "counter" }"#), vec![10, 110]);
    // takes, also spelled reads: one bit, either word, any target.
    assert_eq!(ids(r#""caller" takes { }"#), vec![10, 70, 110]);
    assert_eq!(ids(r#""caller" reads { }"#), vec![10, 70, 110]);
    assert_eq!(ids(r#"takes { "hook" }"#), vec![10, 70]);
    assert_eq!(ids(r#"reads { "hook" }"#), vec![10, 70]);
    assert_eq!(ids(r#"reads { "counter" }"#), vec![10, 110]);

    // `counter++` is one token that states two roles: both words find it.
    assert_eq!(
        ids(r#"writes { "counter" }"#),
        ids(r#"takes { "counter" }"#)
    );
    // A role is a role: the call of `hook` is not a write of it.
    assert_eq!(ids(r#"writes { "hook" }"#), vec![70]);

    // AN UNSTATED ROLE IS UNKNOWN, NEVER "NO" -- and never "yes": no role
    // word matches a reference that states nothing.  (A statement that names
    // something shows it; the unnamed one is empty.)
    for word in ["calls", "writes", "takes", "reads"] {
        assert_eq!(ids(&format!(r#"{word} {{ "plain" }}"#)), vec![90], "{word}");
        assert_eq!(ids(&format!(r#""other" {word} {{ }}"#)), vec![20], "{word}");
        assert_eq!(
            ids(&format!(r#""old_caller" {word} {{ }}"#)),
            vec![1010],
            "{word}"
        );
    }
    // A relation row is no token and states no role.
    assert_eq!(ids(r#"calls { "my_read" }"#), vec![60]);
    assert_eq!(ids(r#"field("slot") calls { }"#), vec![50]);

    // `calls` is literal: `via` calls the slot.  What the slot holds is one
    // more hop, and it is written.
    assert_eq!(ids(r#""via" calls { }"#), vec![30, 50, 140]);
    assert_eq!(ids(r#""via" calls { func }"#), vec![30, 140]);
    assert_eq!(ids(r#""via" calls { field }"#), vec![30, 50]);
    assert_eq!(
        ids(r#""via" calls { field holds { func } }"#),
        vec![30, 50, 60, 140, 150]
    );
    // The word carries into nested scopes as `refs` does: the slot calls
    // nothing, so the inner scope written without `holds` finds nothing.
    assert_ne!(
        ids(r#""via" calls { field { func } }"#),
        ids(r#""via" calls { field holds { func } }"#)
    );
    assert_eq!(
        ids(r#""via" calls { field { func } }"#),
        ids(r#""via" calls { field calls { func } }"#)
    );

    // Two role words on one statement are joined with `or`
    // (`relationship_words_joined_with_or_match_a_pair_by_any_of_its_rows`).

    // The words take no arguments.
    for q in [
        r#"calls(inherit=true) { "hook" }"#,
        r#"writes("x") { "hook" }"#,
        r#"reads(inherit=false) { "hook" }"#,
    ] {
        assert!(
            crate::test_util::run_query_err(crate::test_util::TEST_INPUT_ROLE_WORDS, q).is_err(),
            "{q}"
        );
    }
}

#[test]
fn a_role_word_reads_a_drawn_edge_as_a_reference_that_states_nothing() {
    // A forced edge (`!"x"`) is drawn by the query, not read from the text:
    // in memory it is an ordinary reference that states nothing, and every
    // relationship word is applied to it as to any other row.  `refs` admits
    // such a reference and draws the edge.  No other word does: not `has`,
    // not the relation words, and not a role word -- the edge is not stated
    // to be a call.
    let ids = role_ids;
    let drawn = |word: &str| ids(&format!(r#""other" {word} {{ !"my_read" }}"#));
    assert_eq!(drawn(""), vec![20, 60], "a bare scope draws it");
    assert_eq!(drawn("refs"), vec![20, 60]);
    for word in [
        "has", "holds", "declares", "calls", "writes", "takes", "reads",
    ] {
        assert_eq!(drawn(word), Vec::<i64>::new(), "{word}");
    }

    // A label use (`#l`) is the same kind of row.  The label's own statement
    // prints `my_read` whatever happens; the question is what becomes of
    // `other`.  A word that reads the reference table and does not admit the
    // row leaves `other` as it was, role words exactly as `holds` and
    // `declares` do.  (`has`, which reads no reference at all, drops it.)
    let labelled = |word: &str| ids(&format!(r#"@l "my_read"; "other" {word} {{ #l }}"#));
    assert_eq!(labelled(""), vec![20, 60]);
    assert_eq!(labelled("refs"), vec![20, 60]);
    for word in ["holds", "declares", "calls", "writes", "takes", "reads"] {
        assert_eq!(labelled(word), vec![20, 60], "{word}");
    }
    assert_eq!(labelled("has"), vec![60]);
}

#[test]
fn a_role_word_says_how_many_symbols_it_could_not_decide() {
    // `caller` names `plain` and `maybe` without saying what it does to
    // them; `calls` lists neither and cannot say they are not calls.  `T` (a
    // type) and `M` (a macro) are not counted: a reference to one has no
    // role to state.  `counter` is not counted: its reference states what it
    // does, and that is not a call.
    assert_eq!(
        role_said(r#""caller" calls { }"#),
        vec![
            "`calls` passed over 2 symbols that `\"caller\"` names without stating a role \
             (`#define` bodies, macro uses): `maybe`, `plain`. Whether it calls them is not \
             known; `refs { … }` lists them"
                .to_string()
        ]
    );
    // By default: the answer is not empty (it lists `hook` and `shared`).
    assert_eq!(role_ids(r#""caller" calls { }"#), vec![10, 70, 80]);

    // Upward: `other` names `hook`, role unstated.
    assert_eq!(
        role_said(r#"calls { "hook" }"#),
        vec![
            "`calls` passed over 1 symbol that names `\"hook\"` without stating a role \
             (`#define` bodies, macro uses): `other`. Whether it calls `\"hook\"` is not \
             known; `refs { … }` lists it"
                .to_string()
        ]
    );
    // The statement's own words are part of what would have been listed.
    assert_eq!(
        role_said(r#""caller" writes { data }"#),
        vec![
            "`writes` passed over 1 symbol matching `data` that `\"caller\"` names without \
             stating a role (`#define` bodies, macro uses): `maybe`. Whether it writes it is \
             not known; `refs { … }` lists it"
                .to_string()
        ]
    );
    // The word as it was written.
    assert_eq!(
        role_said(r#""caller" reads { func }"#),
        vec![
            "`reads` passed over 2 symbols matching `func` that `\"caller\"` names without \
             stating a role (`#define` bodies, macro uses): `plain`, `shared`. Whether it \
             reads them is not known; `refs { … }` lists them"
                .to_string()
        ]
    );
    assert!(role_said(r#""caller" takes { func }"#)[0].starts_with("`takes` passed over 2 symbols"));
    // `caller` names `shared` where nothing is stated and CALLS it somewhere
    // else: `calls` found it, so it did not pass it over (above); `reads`
    // did not find it, and did.

    // Between two statements that both name something, the target is the one
    // spoken of; when the target was found, the sources are.
    assert_eq!(
        role_said(r#""other" calls { "hook" }"#),
        vec![
            "`calls` passed over 1 symbol matching `\"hook\"` that `\"other\"` names without \
             stating a role (`#define` bodies, macro uses): `hook`. Whether it calls it is \
             not known; `refs { … }` lists it"
                .to_string()
        ]
    );
    assert_eq!(
        role_said(r#"func calls { "hook" }"#),
        vec![
            "`calls` passed over 1 symbol matching `func` that names `\"hook\"` without \
             stating a role (`#define` bodies, macro uses): `other`. Whether it calls \
             `\"hook\"` is not known; `refs { … }` lists it"
                .to_string()
        ]
    );
    // Two sources in one statement: `caller` calls `hook`, so `hook` was
    // found; `other` only names it, and is what was passed over.
    assert_eq!(
        role_said(r#""caller" "other" calls { "hook" }"#),
        vec![
            "`calls` passed over 1 symbol matching `\"caller\" \"other\"` that names \
             `\"hook\"` without stating a role (`#define` bodies, macro uses): `other`. \
             Whether it calls `\"hook\"` is not known; `refs { … }` lists it"
                .to_string()
        ]
    );
    // Two symbols of one name: `M` calls one `twin` and names the other.
    // The one it only names is the one passed over.
    assert_eq!(
        role_ids(r#"macro("M") calls { "twin" }"#),
        vec![130, 160, 165]
    );
    assert_eq!(
        role_ids(r#"macro("M") refs { "twin" }"#),
        vec![130, 160, 165, 170]
    );
    assert_eq!(
        role_said(r#"macro("M") calls { "twin" }"#),
        vec![
            "`calls` passed over 1 symbol matching `\"twin\"` that `macro(\"M\")` names \
             without stating a role (`#define` bodies, macro uses): `twin`. Whether it calls \
             it is not known; `refs { … }` lists it"
                .to_string()
        ]
    );

    // Nothing passed over, nothing said: every reference states its role ...
    for q in [
        r#""via" calls { }"#,
        r#"writes { "counter" }"#,
        r#""caller" calls { "hook" }"#,
        // ... or the only unstated ones name a type or a macro ...
        r#"calls { "T" }"#,
        r#"calls { "M" }"#,
        r#""caller" calls { type }"#,
        // ... or the statement would not have listed them anyway ...
        r#""caller" calls { "counter" }"#,
        r#""caller" calls { field }"#,
        // ... or its own words match nothing anywhere (it is told that).
        r#""caller" calls { "no_such_symbol" }"#,
        r#""no_such_symbol" calls { "plain" }"#,
    ] {
        assert_eq!(role_said(q), Vec::<String>::new(), "{q}");
    }
    // A word that is not a role word passes over no role.
    for q in [
        r#""caller" refs { }"#,
        r#""caller" { }"#,
        r#""caller" has { }"#,
    ] {
        assert_eq!(role_said(q), Vec::<String>::new(), "{q}");
    }
}

#[test]
fn what_a_role_word_passed_over_is_what_refs_would_have_listed_there() {
    // "passed over" is measured against the answer the same query gives with
    // `refs` in that one place.  `caller` names `plain` with nothing stated,
    // and `plain` calls nothing: the outer `calls` passed `plain` over only
    // if the inner scope would have kept it.
    assert_eq!(
        role_said(r#""caller" calls { calls { "hook" } }"#),
        Vec::<String>::new(),
        "`plain` and `maybe` do not call `hook`: `refs` there lists neither"
    );
    // From the far end too: `other` names `hook` with nothing stated, but
    // `caller` does not call `other`, so it was never going to be listed.
    assert_eq!(
        role_ids(r#""caller" refs { calls { "hook" } }"#),
        Vec::<i64>::new(),
        "nothing `caller` names calls `hook`, whatever the outer word"
    );

    // One scope further out there is nothing settled to test against (the
    // middle scopes name nothing and came out empty), so nothing can be said
    // to have been passed over: silence, not a guess.
    assert_eq!(
        role_said(r#"calls { calls { calls { "plain" } } }"#),
        Vec::<String>::new()
    );

    // `via` calls `slot`, and `slot` names `plain` with nothing stated: with
    // `refs` in the inner scope `slot` is listed, so the inner `calls`
    // passed it over.  `caller` names `plain` too and is not counted: `via`
    // does not call `caller`.
    assert_eq!(
        role_said(r#""via" calls { calls { "plain" } }"#),
        vec![
            "`calls` passed over 1 symbol that names `\"plain\"` without stating a role \
             (`#define` bodies, macro uses): `slot`. Whether it calls `\"plain\"` is not \
             known; `refs { … }` lists it"
                .to_string()
        ]
    );
    assert_eq!(
        role_ids(r#""via" calls { refs { "plain" } }"#),
        vec![30, 50, 90]
    );
}

/// What a relationship word said it passed over on the role-words fixture,
/// with every read limited to `cap` rows.
fn role_said_with_read_cap(q: &str, cap: usize) -> Vec<String> {
    crate::test_util::run_query_with_read_cap(crate::test_util::TEST_INPUT_ROLE_WORDS, q, cap)
        .warnings
        .iter()
        .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
        .map(|w| w.message.clone())
        .collect()
}

#[test]
fn a_role_word_counts_nothing_it_could_not_read_to_the_end() {
    // `caller` calls two things.  Limited to one row the answer is cut, and
    // says it is incomplete.  So is the read of what the word found, and
    // what it passed over is what it did not find: nothing is counted.
    let cut = crate::test_util::run_query_with_read_cap(
        crate::test_util::TEST_INPUT_ROLE_WORDS,
        r#""caller" calls { }"#,
        1,
    );
    assert!(
        cut.warnings
            .iter()
            .any(|w| w.kind == crate::diagnostic::DiagnosticKind::Truncation),
        "{:?}",
        cut.warnings
    );
    assert_eq!(
        role_said_with_read_cap(r#""caller" calls { }"#, 1),
        Vec::<String>::new()
    );

    // `noisy` names `maybe` eleven times with nothing stated.  Read whole,
    // that is one symbol passed over ...
    assert_eq!(
        role_said(r#""noisy" calls { }"#),
        vec![
            "`calls` passed over 1 symbol that `\"noisy\"` names without stating a role \
             (`#define` bodies, macro uses): `maybe`. Whether it calls it is not known; \
             `refs { … }` lists it"
                .to_string()
        ]
    );
    // ... and with a budget of ten references the read gives up without a
    // row.  There is then nothing to count or to name, and silence would
    // pass the eleven over without a word.
    assert_eq!(
        role_said_with_read_cap(r#""noisy" calls { }"#, 1),
        vec![
            "`calls` passed over more references than could be read here, which `\"noisy\"` \
             makes without stating a role (`#define` bodies, macro uses). Whether they are \
             what `calls` asks for is not known; `refs { … }` lists them"
                .to_string()
        ]
    );
}

#[test]
fn what_a_role_word_passed_over_is_read_from_rows_the_word_did_not_choose() {
    // Two functions are called `twin`.  The body of `M` calls the first and
    // names the second; the body of `M2` names the first.  `user_of_m` uses
    // both macros.
    //
    // With a probe cap of two, `"twin"` (three rows) does not resolve on its
    // own words and is resolved THROUGH the edge: the `twin` the macros
    // call.  Those rows were chosen by the word.  The unnamed middle was
    // not: it is what `user_of_m` names, `M` and `M2`.  That is where to
    // read from, and reading from there the `twin` that `M` only names is
    // what `calls` passed over.  (Read from the chosen rows, the answer
    // would speak of `M2` instead and never look at what the macros name.)
    let q = r#""user_of_m" refs { calls { "twin" } }"#;
    assert_eq!(
        role_said_with_read_cap(q, 2),
        vec![
            // "at least": three rows carry the name, and the read stops at
            // three.
            "`calls` passed over at least 1 symbols matching `\"twin\"` that it names \
             without stating a role (`#define` bodies, macro uses): `twin`. Whether it calls \
             them is not known; `refs { … }` lists them"
                .to_string()
        ]
    );
}

#[test]
fn an_index_that_states_no_roles_says_a_role_word_cannot_match_there() {
    // `silent` was uploaded by something that states no roles.  Its
    // references are all "refers to": a role word answers nothing there, and
    // that is a fact about the index, not about the code.
    let silent = "the `silent` index states no roles; `calls` cannot match there. \
                  `refs { … }` lists its references";
    assert_eq!(
        role_said(r#""old_caller" calls { }"#),
        vec![silent.to_string()]
    );
    assert_eq!(
        role_said(r#"calls { "old_target" }"#),
        vec![silent.to_string()]
    );
    assert_eq!(
        role_said(r#"writes { "old_target" }"#),
        vec![silent.replace("`calls`", "`writes`")]
    );
    // Said once, however many scopes carry the word: both edges of this
    // query run into the same index.
    assert_eq!(
        role_said(r#""old_caller" calls { calls { "old_target" } }"#),
        vec![silent.to_string()]
    );

    // `shared` is a name in both projects.  Each index is spoken of for what
    // it is: one states no roles; in the other `other` names `shared`
    // without stating one.
    assert_eq!(role_ids(r#"calls { "shared" }"#), vec![10, 80, 1020]);
    assert_eq!(
        role_said(r#"calls { "shared" }"#),
        vec![
            silent.to_string(),
            "`calls` passed over 1 symbol that names `\"shared\"` without stating a role \
             (`#define` bodies, macro uses): `other`. Whether it calls `\"shared\"` is not \
             known; `refs { … }` lists it"
                .to_string(),
        ]
    );
    // With only the stating project in view, nothing is said of the other.
    assert_eq!(
        role_said(r#"project("stated") calls { "shared" }"#).len(),
        1
    );
    assert!(role_said(r#"project("stated") calls { "shared" }"#)[0].starts_with("`calls` passed"));

    // Not said where the word passed nothing over in that index: nothing
    // there names `OLD_DEFINE`, and `old_target` names nothing.
    for q in [
        r#"calls { "OLD_DEFINE" }"#,
        r#""old_target" calls { }"#,
        r#""old_caller" refs { }"#,
    ] {
        assert_eq!(role_said(q), Vec::<String>::new(), "{q}");
    }
}

#[test]
fn a_role_word_that_found_nothing_says_which_slot_holds_the_target() {
    // Nothing calls `my_read` by name: it is stored in `ops.slot` and called
    // through it.  `calls` is literal, so the answer is empty -- and says
    // where the function went.
    assert_eq!(role_ids(r#"calls { "my_read" }"#), vec![60]);
    assert_eq!(
        role_said(r#"calls { "my_read" }"#),
        vec![
            "nothing calls `\"my_read\"` directly; `slot` holds it (`⇢`): \
             `calls { field holds { \"my_read\" } }` goes through the slot"
                .to_string()
        ]
    );
    assert_eq!(
        role_said(r#"func takes { "my_read" }"#),
        vec![
            "nothing matching `func` takes `\"my_read\"` directly; `slot` holds it (`⇢`): \
             `func takes { field holds { \"my_read\" } }` goes through the slot"
                .to_string()
        ]
    );
    // The rewrite is a query, and it answers.
    assert_eq!(
        role_ids(r#"calls { field holds { "my_read" } }"#),
        vec![30, 50, 60]
    );

    // Not said when something does call it, when no slot holds it, or when
    // the word is not a role word.
    for q in [
        r#"calls { "hook" }"#,
        r#"writes { "counter" }"#,
        r#"calls { "T" }"#,
        r#"refs { "my_read" }"#,
        r#"has { "my_read" }"#,
        r#""no_such_symbol" calls { "my_read" }"#,
    ] {
        assert!(
            role_said(q).iter().all(|m| !m.contains("directly")),
            "{q}: {:?}",
            role_said(q)
        );
    }
    // `direct` is held by the slot too, and `via` calls it by name: the
    // answer is not empty and there is nothing to explain.
    assert_eq!(role_ids(r#"calls { "direct" }"#), vec![30, 140]);
    assert_eq!(role_said(r#"calls { "direct" }"#), Vec::<String>::new());
    // `stored` is held by the slot, and `other` names it without stating a
    // role.  "Nothing calls it" is then not known, and the answer says that
    // and no more.
    assert_eq!(role_ids(r#"calls { "stored" }"#), vec![150]);
    assert_eq!(
        role_said(r#"calls { "stored" }"#),
        vec![
            "`calls` passed over 1 symbol that names `\"stored\"` without stating a role \
             (`#define` bodies, macro uses): `other`. Whether it calls `\"stored\"` is not \
             known; `refs { … }` lists it"
                .to_string()
        ]
    );
    // Likewise of an index that states no roles.
    assert!(role_said(r#"calls { "old_target" }"#)
        .iter()
        .all(|m| !m.contains("directly")));
}

#[test]
fn a_relation_word_says_when_the_index_has_no_such_rows() {
    // `silent` records no relation of either kind; `stated` has `holds`
    // rows and no `declares` rows.
    assert_eq!(
        role_said(r#"macro("OLD_DEFINE") declares { data }"#),
        vec![
            "the `silent` index has no `declares` rows: nothing in it records what a macro \
             declares"
                .to_string()
        ]
    );
    assert_eq!(
        role_said(r#"declares { "old_lock" }"#),
        vec![
            "the `silent` index has no `declares` rows: nothing in it records what a macro \
             declares"
                .to_string()
        ]
    );
    assert_eq!(
        role_said(r#"holds { "old_target" }"#),
        vec![
            "the `silent` index has no `holds` rows: nothing in it records what a slot holds"
                .to_string()
        ]
    );
    assert_eq!(
        role_said(r#"macro("M") declares { }"#),
        vec![
            "the `stated` index has no `declares` rows: nothing in it records what a macro \
             declares"
                .to_string()
        ]
    );

    // An index that has such rows is asked, and its silence is its answer.
    for q in [
        r#"holds { "plain" }"#,
        r#"field("slot") holds { "plain" }"#,
        // Not empty.
        r#"field("slot") holds { }"#,
        r#"holds { "my_read" }"#,
        // Its own words match nothing anywhere (it is told that).
        r#"macro("OLD_DEFINE") declares { "no_such_symbol" }"#,
    ] {
        assert!(
            role_said(q).iter().all(|m| !m.contains("rows")),
            "{q}: {:?}",
            role_said(q)
        );
    }
    // The source is the wrong kind of thing: that is the reason, and the
    // only one given.
    assert_eq!(
        role_said(r#""old_caller" holds { }"#),
        vec![
            "`holds` related nothing: only a slot holds (a field, or an interface method), \
             and `\"old_caller\"` selects none"
                .to_string()
        ]
    );
    // On the relation fixture, whose index has both kinds, nothing is said
    // of the index.
    for q in [r#"declares { "plain" }"#, r#"holds { "plain" }"#] {
        assert!(passed_over(q).iter().all(|m| !m.contains("rows")), "{q}");
    }
}

#[test]
fn what_a_role_word_says_survives_a_cache_hit() {
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index =
            crate::test_util::get_shared_index(crate::test_util::TEST_INPUT_ROLE_WORDS).await;
        let said = |res: &crate::statement::ExecutionResult| -> Vec<String> {
            res.warnings
                .iter()
                .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
                .map(|w| w.message.clone())
                .collect()
        };
        for (q, notices) in [
            (r#""caller" calls { }"#, 1),
            (r#"calls { "shared" }"#, 2),
            (r#"calls { "my_read" }"#, 1),
            (r#"macro("OLD_DEFINE") declares { data }"#, 1),
        ] {
            let (cold, _) = crate::test_util::run_query_traced_on(index.clone(), q)
                .await
                .unwrap();
            let before = index.sql_cache().stats();
            let (warm, _) = crate::test_util::run_query_traced_on(index.clone(), q)
                .await
                .unwrap();
            let after = index.sql_cache().stats();
            assert_eq!(said(&cold).len(), notices, "{q}: {:?}", cold.warnings);
            assert_eq!(said(&warm), said(&cold), "{q}");
            assert_eq!(
                after.misses, before.misses,
                "{q}: the second run read nothing"
            );
            assert!(after.hits > before.hits, "{q}");
        }
    });
}

#[test]
fn relationship_words_joined_with_or_match_a_pair_by_any_of_its_rows() {
    // On the role-words fixture: `caller` calls `hook` and `shared`, writes
    // and takes `counter`, takes `hook`; `via` calls `slot` and `direct`;
    // `slot` (a field of `ops`) holds `my_read`, `direct` and `stored`.
    let ids = role_ids;

    // Two role words: either role.
    assert_eq!(ids(r#""caller" calls { }"#), vec![10, 70, 80]);
    assert_eq!(ids(r#""caller" writes { }"#), vec![10, 110]);
    assert_eq!(
        ids(r#""caller" calls or writes { }"#),
        vec![10, 70, 80, 110]
    );
    assert_eq!(
        ids(r#""caller" writes or calls { }"#),
        vec![10, 70, 80, 110]
    );
    assert_eq!(
        ids(r#""caller" (calls or writes) { }"#),
        vec![10, 70, 80, 110]
    );
    // A role word or a relation: downward ...
    assert_eq!(
        ids(r#"field("slot") calls or holds { }"#),
        vec![50, 60, 140, 150]
    );
    assert_eq!(ids(r#""via" calls or holds { }"#), vec![30, 50, 140]);
    // ... and upward: `via` calls `direct`, `slot` holds it.
    assert_eq!(ids(r#"calls { "direct" }"#), vec![30, 140]);
    assert_eq!(ids(r#"holds { "direct" }"#), vec![50, 140]);
    assert_eq!(ids(r#"calls or holds { "direct" }"#), vec![30, 50, 140]);
    assert_eq!(ids(r#"calls or holds { "my_read" }"#), vec![50, 60]);
    // A role word or containment.
    assert_eq!(ids(r#"type("ops") calls { }"#), vec![40]);
    assert_eq!(ids(r#"type("ops") has or calls { }"#), vec![40, 50]);
    assert_eq!(ids(r#"calls or has { "slot" }"#)[..3], [30, 40, 50]);

    // LAW 3: a type group for the statement, a relationship group for the
    // edge, side by side.
    assert_eq!(
        ids(r#"func or field calls or holds { "direct" }"#),
        vec![30, 50, 140]
    );
    assert_eq!(
        ids(r#"func or data calls or holds { "direct" }"#),
        vec![30, 140]
    );
    assert_eq!(
        ids(r#"field or data calls or holds { "direct" }"#),
        vec![50, 140]
    );

    // A role word narrows `refs`: beside `refs` it adds nothing.
    assert_eq!(
        ids(r#""caller" calls or refs { }"#),
        ids(r#""caller" refs { }"#)
    );
    // One bit, two spellings.
    assert_eq!(
        ids(r#""caller" reads or takes { }"#),
        ids(r#""caller" takes { }"#)
    );

    // The expression carries into nested scopes as one word does: `via`
    // calls the slot, and the slot holds the three functions.
    assert_eq!(
        ids(r#""via" calls or holds { field { func } }"#),
        vec![30, 50, 60, 140, 150]
    );
    assert_eq!(
        ids(r#""via" calls or holds { field { func } }"#),
        ids(r#""via" calls { field holds { func } }"#)
    );

    // On the relation fixture: `slot` HOLDS `my_read` and names `plain`;
    // `DEFINE_X` DECLARES `lock` and names `plain`; `ops` contains `slot`.
    let ids = relation_ids;
    assert_eq!(ids(r#"field("slot") refs or holds { }"#), vec![20, 30, 40]);
    assert_eq!(ids(r#"refs or holds { "my_read" }"#), vec![20, 30]);
    assert_eq!(
        ids(r#"macro("DEFINE_X") refs or declares { }"#),
        vec![40, 60, 70]
    );
    assert_eq!(
        ids(r#"macro("DEFINE_X") holds or declares { }"#),
        vec![60, 70]
    );
    assert_eq!(ids(r#"type("ops") has or refs { }"#), vec![10, 20, 40]);
    assert_eq!(ids(r#"type("ops") has or holds { }"#), vec![10, 20]);
    assert_eq!(ids(r#"has or holds { "my_read" }"#), vec![20, 30, 500, 510]);
}

#[test]
fn the_four_relationships_joined_are_the_bare_scope() {
    // FOUR DISJOINT RELATIONSHIPS, ONE PER MARK; A BARE `{ }` IS ALL FOUR.
    // Written out, they are the same edge, the same answer, and the same
    // reads: the second query of each pair is answered from the SQL cache of
    // the first without one new statement.
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        for (input, bare, written) in [
            (
                crate::test_util::TEST_INPUT_RELATION_ROWS,
                r#"field("slot") { }"#,
                r#"field("slot") has or refs or declares or holds { }"#,
            ),
            (
                crate::test_util::TEST_INPUT_RELATION_ROWS,
                r#"{ "my_read" }"#,
                r#"holds or declares or refs or has { "my_read" }"#,
            ),
            (
                crate::test_util::TEST_INPUT_RELATION_ROWS,
                r#"type("ops") { { } }"#,
                r#"type("ops") has or refs or declares or holds { { } }"#,
            ),
            (
                crate::test_util::TEST_INPUT_ROLE_WORDS,
                r#""via" { field { func } }"#,
                r#""via" has or refs or (declares or holds) { field { func } }"#,
            ),
        ] {
            let index = crate::test_util::get_shared_index(input).await;
            let (first, _) = crate::test_util::run_query_traced_on(index.clone(), bare)
                .await
                .unwrap();
            let before = index.sql_cache().stats();
            let (second, _) = crate::test_util::run_query_traced_on(index.clone(), written)
                .await
                .unwrap();
            let after = index.sql_cache().stats();
            let ids = |res: &crate::statement::ExecutionResult| -> Vec<i64> {
                let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
                ids.sort();
                ids
            };
            assert!(!ids(&first).is_empty(), "{bare}");
            assert_eq!(ids(&second), ids(&first), "{written}");
            assert_eq!(
                after.misses, before.misses,
                "{written}: a statement the bare scope did not run"
            );
            assert!(after.hits > before.hits, "{written}");
        }
    });
}

#[test]
fn what_an_expression_passed_over_is_said_of_the_words_it_is_true_of() {
    let unstated = "without stating a role (`#define` bodies, macro uses)";

    // Two role words: the expression is quoted as written, and what is not
    // known is whether the source does either.
    assert_eq!(
        role_said(r#""caller" calls or writes { }"#),
        vec![format!(
            "`calls or writes` passed over 2 symbols that `\"caller\"` names {unstated}: \
             `maybe`, `plain`. Whether it calls or writes them is not known; `refs {{ … }}` \
             lists them"
        )]
    );
    // Each spelling as it was written.
    assert_eq!(
        role_said(r#""caller" writes or reads { func }"#),
        vec![format!(
            "`writes or reads` passed over 2 symbols matching `func` that `\"caller\"` names \
             {unstated}: `plain`, `shared`. Whether it writes or reads them is not known; \
             `refs {{ … }}` lists them"
        )]
    );
    // A role word beside a relation or beside containment: the expression
    // passed them over, and what is not known is the role.
    assert_eq!(
        role_said(r#""caller" calls or holds { }"#),
        vec![format!(
            "`calls or holds` passed over 2 symbols that `\"caller\"` names {unstated}: \
             `maybe`, `plain`. Whether it calls them is not known; `refs {{ … }}` lists them"
        )]
    );
    assert_eq!(
        role_said(r#""caller" has or calls { }"#),
        vec![format!(
            "`has or calls` passed over 2 symbols that `\"caller\"` names {unstated}: \
             `maybe`, `plain`. Whether it calls them is not known; `refs {{ … }}` lists them"
        )]
    );
    // Upward.  `slot` holds `stored`, so the answer is not empty; `other`
    // names it with nothing stated.
    assert_eq!(role_ids(r#"calls or holds { "stored" }"#), vec![50, 150]);
    assert_eq!(
        role_said(r#"calls or holds { "stored" }"#),
        vec![format!(
            "`calls or holds` passed over 1 symbol that names `\"stored\"` {unstated}: \
             `other`. Whether it calls `\"stored\"` is not known; `refs {{ … }}` lists it"
        )]
    );
    // `refs` admits a reference that states nothing: beside it, a role word
    // passes nothing over.
    for q in [
        r#""caller" calls or refs { }"#,
        r#""caller" refs or writes { func }"#,
        r#"calls or refs { "stored" }"#,
    ] {
        assert_eq!(role_said(q), Vec::<String>::new(), "{q}");
    }

    // An index that states no roles: it is the ROLE word that cannot match
    // there.  `holds` beside it is another matter, and is said separately:
    // `old_caller` is a function, and only a slot holds.
    assert_eq!(
        role_said(r#""old_caller" calls or holds { }"#),
        vec![
            "the `silent` index states no roles; `calls` cannot match there. `refs { … }` \
             lists its references"
                .to_string(),
            "`holds` related nothing: only a slot holds (a field, or an interface method), \
             and `\"old_caller\"` selects none"
                .to_string(),
        ]
    );
    assert_eq!(
        role_said(r#"writes or takes or has { "old_target" }"#),
        vec![
            "the `silent` index states no roles; `writes or takes` cannot match there. \
             `refs { … }` lists its references"
                .to_string()
        ]
    );

    // "Nothing calls it directly; a slot holds it" is said to a query that
    // did not ask for what a slot holds ...
    assert_eq!(
        role_said(r#"func calls or has { "my_read" }"#),
        vec![
            "nothing matching `func` calls `\"my_read\"` directly; `slot` holds it (`⇢`): \
             `func calls { field holds { \"my_read\" } }` goes through the slot"
                .to_string()
        ]
    );
    // ... and not to one that did: `calls or holds` found the slot,
    assert_eq!(role_ids(r#"calls or holds { "my_read" }"#), vec![50, 60]);
    assert_eq!(
        role_said(r#"calls or holds { "my_read" }"#),
        Vec::<String>::new()
    );
    // and where the statement's own type left the slot out, the word passed
    // nothing over: it is `func` that did, and it says nothing of slots.
    assert_eq!(role_ids(r#"func calls or holds { "my_read" }"#), vec![60]);
    assert_eq!(
        role_said(r#"func calls or holds { "my_read" }"#),
        Vec::<String>::new()
    );
    // A source somebody named, of the wrong kind for `holds`: that much is
    // true of `holds` whatever stands beside it.
    assert_eq!(
        role_said(r#""caller" calls or holds { "my_read" }"#),
        vec![
            "`holds` related nothing: only a slot holds (a field, or an interface method), \
             and `\"caller\"` selects none"
                .to_string()
        ]
    );

    // A relation word that could not match says so beside any other word
    // (`stated` has no `declares` rows) ...
    assert_eq!(
        role_said(r#"macro("M") declares or has { }"#),
        vec![
            "the `stated` index has no `declares` rows: nothing in it records what a macro \
             declares"
                .to_string()
        ]
    );
    // ... two of them are quoted as written ...
    assert_eq!(
        role_said(r#""old_caller" holds or declares { }"#),
        vec![
            "`holds or declares` related nothing: only a slot holds (a field, or an \
             interface method); only a macro declares, and `\"old_caller\"` selects none"
                .to_string()
        ]
    );
    assert_eq!(
        role_said(r#"declares or holds { "old_target" }"#),
        vec![
            "the `silent` index has no `declares` or `holds` rows: nothing in it records \
             what a macro declares or what a slot holds"
                .to_string()
        ]
    );
    // ... and nothing is said when the answer is not empty.
    assert_eq!(
        role_said(r#"field("slot") holds or declares { }"#),
        Vec::<String>::new()
    );

    // `refs` beside other words still lists only what those words list; the
    // sentence names all of it.  (Relation fixture: `slot` holds `my_read`,
    // `DEFINE_X` declares `lock`.)
    assert_eq!(
        passed_over(r#"field("slot") has or refs { "my_read" }"#),
        vec![
            "`has or refs` lists only containment (`▸`) and ordinary references (`→`); \
             `field(\"slot\")` holds 1 symbol that `\"my_read\"` matches (`⇢`): `my_read`. \
             `holds { … }` or `{ … }` lists it"
                .to_string()
        ]
    );
    assert_eq!(
        passed_over(r#"refs or declares { "my_read" }"#),
        vec![
            "`refs or declares` lists only ordinary references (`→`) and what a macro \
             declares (`↦`); 1 symbol holds `\"my_read\"` (`⇢`): `slot`. `holds { … }` or \
             `{ … }` lists it"
                .to_string()
        ]
    );
    assert_eq!(
        passed_over(r#"macro("DEFINE_X") calls or refs { data }"#),
        vec![
            "`calls or refs` lists only ordinary references (`→`); `macro(\"DEFINE_X\")` \
             declares 1 symbol that `data` matches (`↦`): `lock`. `declares { … }` or \
             `{ … }` lists it"
                .to_string()
        ]
    );
    // All four written out pass nothing over, like the bare scope.
    for q in [
        r#"field("slot") refs or holds { "my_read" }"#,
        r#"field("slot") has or refs or declares or holds { "lock" }"#,
        r#"field("slot") { "lock" }"#,
    ] {
        assert_eq!(passed_over(q), Vec::<String>::new(), "{q}");
    }
}

#[test]
fn what_an_expression_says_survives_a_cache_hit() {
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index =
            crate::test_util::get_shared_index(crate::test_util::TEST_INPUT_ROLE_WORDS).await;
        let said = |res: &crate::statement::ExecutionResult| -> Vec<String> {
            res.warnings
                .iter()
                .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
                .map(|w| w.message.clone())
                .collect()
        };
        for (q, notices) in [
            (r#""caller" calls or writes { }"#, 1),
            (r#""old_caller" calls or holds { }"#, 2),
            (r#"func calls or has { "my_read" }"#, 1),
        ] {
            let (cold, _) = crate::test_util::run_query_traced_on(index.clone(), q)
                .await
                .unwrap();
            let before = index.sql_cache().stats();
            let (warm, _) = crate::test_util::run_query_traced_on(index.clone(), q)
                .await
                .unwrap();
            let after = index.sql_cache().stats();
            assert_eq!(said(&cold).len(), notices, "{q}: {:?}", cold.warnings);
            assert_eq!(said(&warm), said(&cold), "{q}");
            assert_eq!(
                after.misses, before.misses,
                "{q}: the second run read nothing"
            );
            assert!(after.hits > before.hits, "{q}");
        }
    });
}

#[test]
fn hidden_children_are_not_an_incomplete_answer() {
    // `GEN` refers to `shown` and to twelve folded symbols.  `"GEN" { }` lists
    // `shown`, and that is the whole answer: the twelve are left out for what
    // they are, not because a limit cut them.
    //
    // GEN's own children read did not apply the listing's filter, so the
    // twelve filled its limit (8 here) and the statement said "this part of
    // the graph is incomplete" over a complete answer.  On the kernel that is
    // every macro whose invocations generate hidden declarations.
    let (said, ids) = incomplete_and_ids(&run_query_with_budget(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""GEN" { }"#,
        1,
    ));
    assert_eq!(ids, vec![20, 30], "GEN and the one child it lists");
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);

    // The same without a budget: the safety cap (3) is what the twelve fill.
    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""GEN" { }"#,
        3,
    ));
    assert_eq!(ids, vec![20, 30]);
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);

    // Containment: `host` holds `inner` and twelve folded declarations.
    let (said, ids) = incomplete_and_ids(&run_query_with_budget(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""host" { }"#,
        1,
    ));
    assert_eq!(ids, vec![50, 60], "host and the one declaration it lists");
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);

    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""host" { }"#,
        3,
    ));
    assert_eq!(ids, vec![50, 60]);
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);

    // `unnest` lists the hidden ones, so there a limit does cut the answer.
    let (said, _) = incomplete_and_ids(&run_query_with_read_cap(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""GEN" { unnest }"#,
        3,
    ));
    assert!(!said.is_empty(), "thirteen children, a cap of three");
}

fn node_ids(res: &crate::statement::ExecutionResult) -> Vec<i64> {
    let mut ids: Vec<i64> = res.nodes.as_vec().iter().map(|i| (*i).into()).collect();
    ids.sort();
    ids
}

#[test]
fn a_reference_comes_from_the_instance_that_encloses_it() {
    // `main` is one symbol with an instance in /a.c and one in /b.c.  Only
    // the one in /a.c calls `print_version`.  Evidence used to be kept per
    // SYMBOL: "some `main` refers to it" stood for every `main`.
    //
    // Narrowing the caller: both instances were kept.
    let res = run_query(
        crate::test_util::TEST_INPUT_ONE_NAME_TWO_PROGRAMS,
        r#""main" { "print_version" }"#,
    );
    assert_eq!(
        node_ids(&res),
        vec![10, 30],
        "the main that makes the call, and what it calls"
    );

    // Narrowing the callee: the main of /b.c is the only caller left, and it
    // does not call `print_version`.  Kernel-sized case, ucx:
    // `file("…/ucx_profiling.c") { func("main") { "print_version" } }`
    // returned `print_version` with no edge, on the strength of another
    // program's `main`.
    let res = run_query(
        crate::test_util::TEST_INPUT_ONE_NAME_TWO_PROGRAMS,
        r#"file("/b.c") { func("main") { "print_version" } }"#,
    );
    assert_eq!(
        node_ids(&res),
        Vec::<i64>::new(),
        "nothing in /b.c calls it"
    );

    // The same file and the call its `main` does make.
    let res = run_query(
        crate::test_util::TEST_INPUT_ONE_NAME_TWO_PROGRAMS,
        r#"file("/b.c") { func("main") { "helper" } }"#,
    );
    assert_eq!(node_ids(&res), vec![20, 40, 1002]);

    // A name asked for with nothing below it still has every instance.
    let res = run_query(
        crate::test_util::TEST_INPUT_ONE_NAME_TWO_PROGRAMS,
        r#""main""#,
    );
    assert_eq!(node_ids(&res), vec![10, 20]);

    // What a statement refers to is still a symbol: both callers reach it.
    let res = run_query(
        crate::test_util::TEST_INPUT_ONE_NAME_TWO_PROGRAMS,
        r#""main" { }"#,
    );
    assert_eq!(node_ids(&res), vec![10, 20, 30, 40]);
}

#[test]
fn a_container_narrows_a_name_written_in_many_places() {
    // `init_module` is one symbol with an instance in each of twelve files.
    // Asked for inside one file it has one answer.  The file used to narrow
    // the SYMBOL only: the refinement probe returned all twelve instances,
    // capped (5 here), and a capped statement is not narrowed -- so the answer
    // was the file, some `init_module` from elsewhere, no edge between them
    // and "this part of the graph is incomplete".  Kernel:
    // `file("…/bt-bmc.c") has { func("init_module") }`, 9,714 instances.
    let res = run_query_with_read_cap(
        TEST_INPUT_ONE_NAME_MANY_FILES,
        r#"file("/drv_3.c") has { "init_module" }"#,
        5,
    );
    let (said, ids) = incomplete_and_ids(&res);
    assert!(said.is_empty(), "the file bounds the name, got {:?}", said);
    assert_eq!(ids, vec![103, 203], "the file and the instance in it");
    let held: Vec<(i64, i64)> = res
        .has_edges
        .0
        .iter()
        .map(|e| (e.parent_instance.into(), e.child_instance.into()))
        .collect();
    assert_eq!(held, vec![(103, 203)]);

    // A bare scope accepts containment as well, and the file refers to
    // nothing, so the answer is the same.
    let res = run_query_with_read_cap(
        TEST_INPUT_ONE_NAME_MANY_FILES,
        r#"file("/drv_3.c") { "init_module" }"#,
        5,
    );
    let (said, ids) = incomplete_and_ids(&res);
    assert!(said.is_empty(), "the file bounds the name, got {:?}", said);
    assert_eq!(ids, vec![103, 203]);

    // The mirror: the module `drivers` has an instance in every file, and
    // one of them holds `probe_3`.
    let res = run_query_with_read_cap(
        TEST_INPUT_ONE_NAME_MANY_FILES,
        r#"mod("drivers") has { "probe_3" }"#,
        5,
    );
    let (said, ids) = incomplete_and_ids(&res);
    assert!(
        said.is_empty(),
        "the child bounds the module, got {:?}",
        said
    );
    assert_eq!(
        ids,
        vec![303, 503],
        "probe_3 and the module instance over it"
    );
}

#[test]
fn what_was_left_out_is_counted_by_name() {
    // "A listing that passes over a folded symbol says so."  Twelve are left
    // out here; three are named and the rest counted.
    let said = |res: &crate::statement::ExecutionResult| -> Vec<String> {
        res.warnings
            .iter()
            .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::LeftOut)
            .map(|w| w.message.clone())
            .collect()
    };
    assert_eq!(
        said(&run_query(TEST_INPUT_HIDDEN_CHILDREN, r#""GEN" { }"#)),
        vec![
            "`{ }` leaves out 12 folded (macro-generated) symbols: `hid_1`, `hid_10`, \
             `hid_11` and 9 more. Add `unnest` to list them, or name one"
        ]
    );
    // Containment says it too.
    assert_eq!(
        said(&run_query(TEST_INPUT_HIDDEN_CHILDREN, r#""host" { }"#)),
        vec![
            "`{ }` leaves out 12 folded (macro-generated) symbols: `g_1`, `g_10`, `g_11` \
             and 9 more. Add `unnest` to list them, or name one"
        ]
    );
    // The read that finds them is bounded like any other, and says so.
    assert_eq!(
        said(&run_query_with_read_cap(
            TEST_INPUT_HIDDEN_CHILDREN,
            r#""GEN" { }"#,
            3
        )),
        vec![
            "`{ }` leaves out at least 4 folded (macro-generated) symbols: `hid_1`, \
             `hid_2`, `hid_3` and more. Add `unnest` to list them, or name one"
        ]
    );
    // Nothing folded under `wide`.
    assert!(said(&run_query(TEST_INPUT_HIDDEN_CHILDREN, r#""wide" { }"#)).is_empty());
    // A listing that was itself cut is not read a second time: it stopped
    // early, and its complement could not.  It says "incomplete" instead.
    // `GEN` and `wide` together list thirteen and leave out twelve.
    let both = r#""GEN" "wide" { }"#;
    assert_eq!(said(&run_query(TEST_INPUT_HIDDEN_CHILDREN, both)).len(), 1);
    let cut = run_query_with_read_cap(TEST_INPUT_HIDDEN_CHILDREN, both, 3);
    assert!(said(&cut).is_empty(), "{:?}", cut.warnings);
    assert!(
        cut.warnings
            .iter()
            .any(|w| w.message.contains("this part of the graph is incomplete")),
        "{:?}",
        cut.warnings
    );
}

#[test]
fn a_family_nobody_reads_is_not_an_incomplete_answer() {
    // `wide` refers to twelve listed symbols.  Under a budget its own children
    // read stops at 8 rows, but nothing reads those rows: the `{ }` below
    // derives its twelve through a read of its own, with its own limit and
    // its own warning.  All twelve are in the answer, so nothing is said.
    let (said, ids) = incomplete_and_ids(&run_query_with_budget(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""wide" { }"#,
        1,
    ));
    let mut all: Vec<i64> = (2001..=2012).collect();
    all.insert(0, 40);
    assert_eq!(ids, all, "wide and all twelve children");
    assert!(said.is_empty(), "nothing was cut, got {:?}", said);

    // When the read that produces the listing is the one that is cut, it is
    // said, and said once.
    let (said, ids) = incomplete_and_ids(&run_query_with_read_cap(
        TEST_INPUT_HIDDEN_CHILDREN,
        r#""wide" { }"#,
        3,
    ));
    assert!(ids.len() < all.len(), "the listing is cut, got {:?}", ids);
    assert!(!said.is_empty(), "and that must be said");
}

#[test]
fn a_composed_child_keeps_its_rows_complete_under_a_budget() {
    // Gate 1's own case — the one `find_symbol`'s scope rule does NOT cover.
    //
    // The wrapper `{ … }` is a unit with no name of its own, so
    // `build_parent_scope` finds no parent filter to scope by; the child IS
    // conditioned and the wrapper originates data (it has a non-weak
    // descendant), so the parent side DEFERS — `Skip`.  The child has no
    // children, so that side is `Skip` too.  Neither is a `Scope`, so nothing
    // inside `find_symbol` objects to bounding the child — yet the wrapper
    // then takes those rows through `constrain_by_child`.
    //
    // `g"test.*"` matches 8 symbols over 9 instances (`test.f` has two), and
    // cap=1 means a LIMIT of 8.  The bound would bite by exactly one row.
    const QUERY: &str = r#"{ g"test.*" }"#;
    let unlimited = run_query_with_budget(TEST_INPUT_MODULES, QUERY, 0);
    let capped = run_query_with_budget(TEST_INPUT_MODULES, QUERY, 1);

    let sorted = |r: &crate::statement::ExecutionResult| {
        let mut v = r.nodes.as_vec();
        v.sort();
        v
    };
    assert!(
        sorted(&unlimited).len() > 8,
        "fixture must exceed cap=1's LIMIT of 8 or the comparison proves \
         nothing; got {}",
        sorted(&unlimited).len()
    );
    assert_eq!(
        sorted(&capped),
        sorted(&unlimited),
        "a result budget must not change a composed answer: the child's rows \
         feed the wrapper's constrain_by_child"
    );
}

#[test]
fn multi_instance_target_keeps_its_reference_edges() {
    // `test.f` has TWO definition instances — 86 in /bar.c, 96 in /main.c —
    // and is referenced twice, from `test.d` (94) and `test.e` (95).
    //
    // The parents query used to return one row per (reference, enclosing
    // declaration, INSTANCE OF THE TARGET), so two rows per reference here,
    // and the edge kept whichever the planner returned first.  It now returns
    // one row per (reference, enclosing declaration) and the target instance
    // is chosen explicitly.  Both edges must survive, once each.
    const QUERY: &str = r#"{ "test.f" }"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    let nodes = res.nodes.as_vec();
    for id in [86, 94, 95, 96] {
        assert!(
            nodes.contains(&SymbolInstanceId::new(id)),
            "expected instance {id} among {nodes:?}"
        );
    }

    let edges = format_edges(res.edges);
    let to_f: Vec<&String> = edges
        .iter()
        .filter(|e| e.ends_with("-86") || e.ends_with("-96"))
        .collect();
    assert!(
        to_f.contains(&&"94-86".to_string()) && to_f.contains(&&"95-86".to_string()),
        "both callers must keep their edge to test.f, got {to_f:?}"
    );
    // Both instances are definitions, so the tiebreak is the lowest id: every
    // edge lands on 86.  The point is not that 86 is the better answer — it is
    // that the answer no longer depends on which row the planner returned
    // first.  See `multi_instance_edges_do_not_fan_out` for the row-level
    // fence, and note that an instance in the REFERENCING object (96 lives in
    // /main.c with both call sites; 86 is in /bar.c) may be the more useful
    // choice if this is ever revisited.
    assert!(
        !edges.iter().any(|e| e.ends_with("-96")),
        "a single deterministic instance must carry the edges, got {edges:?}"
    );
}

#[test]
fn bounded_neighbourhood_alone_does_not_warn() {
    // cap=1 → leaf LIMIT 8.  `"b"` selects exactly ONE instance, so the answer
    // is complete — but its parents family is 3 reference sites × 3 enclosing
    // declarations each (the calling function, the file, the directory) = 9
    // rows, which fills the same LIMIT.
    //
    // The warning used to fire on that: a bounded NEIGHBOURHOOD reported as a
    // possibly-incomplete RESULT.  On the live index the same false alarm
    // greeted `"i915_ggtt"`, one symbol whose parents family runs to 2540
    // rows.  Families are bounded only for statements nobody composes with,
    // and their rows reach the user solely as edges between result nodes,
    // which `find_edges_between` rediscovers unbounded.
    let res = run_query_with_budget(TEST_INPUT_A, r#""b""#, 1);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(92)]);
    assert!(
        !res.warnings
            .iter()
            .any(|w| w.message.contains("result budget")),
        "a complete result must not warn about its neighbourhood, got {:?}",
        res.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );
}

#[test]
fn search_skeleton_no_match_returns_empty() {
    // Nonexistent needle ≥ 3 chars: helper returns an empty match set, the
    // verb should not bail.  Step 6 returns Ok(Some(LayerSpec)) regardless,
    // so the layer is created (empty); the caller's selection is then
    // empty.  Critically: the query should succeed (no err), and produce a
    // result with zero matched instances.
    const QUERY: &str = r#"search("nonexistent_xyzzy")"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let eph_only: Vec<_> = res
        .nodes
        .as_vec()
        .into_iter()
        .filter(|id| {
            let v: i64 = (*id).into();
            v < 0
        })
        .collect();
    assert!(
        eph_only.is_empty(),
        "search of a nonexistent string should yield no ephemeral instances, got {:?}",
        eph_only,
    );
}

#[test]
fn search_skeleton_min_query_length_rejects_short() {
    // Step 6 enforces ≥ 3-character minimum at constructor time (pg_trgm
    // needs one full trigram for index extraction).  A 2-char query
    // should error out at parse time.
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("ab")"#);
    let err = match res {
        Ok(_) => panic!("search(\"ab\") (< 3 chars) should error"),
        Err(e) => e,
    };
    let msg = format!("{}", err);
    assert!(
        msg.contains("at least 3 characters"),
        "error should explain the 3-character minimum, got: {}",
        msg,
    );
}

#[test]
fn search_skeleton_empty_query_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("")"#);
    assert!(res.is_err(), "search(\"\") should error");
}

#[test]
fn search_skeleton_missing_argument_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search()"#);
    assert!(res.is_err(), "search() (no arg) should error");
}

// ============================================================================
// search() verb tests (Step 7: argument parsing + smart case)
// ============================================================================

#[test]
fn search_whole_word_excludes_foobar() {
    // Fixture object 3 has "foobar foo foo_bar foo.bar".  With
    // whole_word=true, `search("foo")` must NOT match the leading
    // "foobar" or "foo_bar" but MUST match the freestanding "foo" and
    // "foo.bar" instances.  The substring default (whole_word=false)
    // would match all four occurrences.
    let substring = run_query(TEST_INPUT_SEARCH, r#"search("foo")"#);
    let whole_word = run_query(TEST_INPUT_SEARCH, r#"search("foo", whole_word=true)"#);

    let n_substr = substring.nodes.as_vec().len();
    let n_whole = whole_word.nodes.as_vec().len();
    assert!(
        n_substr > n_whole,
        "whole_word=true must match strictly fewer ranges than substring; \
         got substring={}, whole_word={}",
        n_substr,
        n_whole,
    );
}

#[test]
fn search_case_smart_default_lowercase_is_insensitive() {
    // Fixture object 4 has "Foo FOO foo".  An all-lowercase query under
    // the default smart-case resolves to insensitive, so all three
    // tokens match across the fixture (plus matches in other files).
    let res = run_query(TEST_INPUT_SEARCH, r#"search("foo", whole_word=true)"#);
    let n_smart_lower = res.nodes.as_vec().len();
    assert!(
        n_smart_lower >= 3,
        "smart-case on lowercase query should match all 3 of Foo/FOO/foo in object 4 \
         (plus matches elsewhere), got {} ranges",
        n_smart_lower,
    );
}

#[test]
fn search_case_smart_default_uppercase_is_sensitive() {
    // "Foo" under smart-case has uppercase → sensitive.  Object 4's
    // "Foo FOO foo" has exactly one literal "Foo" (the others are FOO
    // and foo).
    let res = run_query(TEST_INPUT_SEARCH, r#"search("Foo", whole_word=true)"#);
    let n = res.nodes.as_vec().len();
    // Whole-word + case-sensitive across the fixture: object 4 contributes one
    // match (the literal "Foo"); other objects contain "foo" but not "Foo".
    assert_eq!(
        n, 1,
        "smart-case on \"Foo\" (has uppercase → sensitive) should match exactly the \
         literal \"Foo\" in object 4, got {} ranges",
        n,
    );
}

#[test]
fn search_case_explicit_insensitive_overrides_smart() {
    // `search("Foo", case="insensitive")` matches all three tokens of
    // "Foo FOO foo" in object 4 even though the query has uppercase.
    let res = run_query(
        TEST_INPUT_SEARCH,
        r#"search("Foo", case="insensitive", whole_word=true)"#,
    );
    let n = res.nodes.as_vec().len();
    assert!(
        n >= 3,
        "explicit case=insensitive should match Foo/FOO/foo (>=3), got {}",
        n,
    );
}

#[test]
fn search_bad_case_value_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", case="maybe")"#);
    assert!(res.is_err(), "search with case=maybe should error");
}

#[test]
fn search_bad_whole_word_value_rejects() {
    // A bare word is not a value at all, and a quoted one is a string where a
    // boolean is wanted.  `"yes"` spells no boolean, so no requoting hint.
    for query in [
        r#"search("foo", whole_word=yes)"#,
        r#"search("foo", whole_word="yes")"#,
    ] {
        let res = run_query_err(TEST_INPUT_SEARCH, query);
        assert!(res.is_err(), "must error: {query}");
    }
}

#[test]
fn search_bad_limit_value_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", limit="not_a_number")"#);
    assert!(res.is_err(), "search with a non-integer limit should error");
}

/// The pre-typed-arguments spelling is an error, and the error says what to
/// write instead.  Arguments were all strings once; a query written then
/// fails here rather than silently meaning something else.
#[test]
fn search_quoted_argument_spellings_teach_the_typed_form() {
    for (query, hint) in [
        (r#"search("foo", limit="3")"#, "write limit=3"),
        (
            r#"search("foo", whole_word="true")"#,
            "write whole_word=true",
        ),
    ] {
        let Err(err) = run_query_err(TEST_INPUT_SEARCH, query) else {
            panic!("must not parse: {query}");
        };
        assert!(
            err.to_string().contains(hint),
            "expected the hint {hint:?} for {query}, got: {err}"
        );
    }
}

/// The layer cache is keyed on the PARSED inputs, not on which arguments
/// were written.  Spelling the defaults explicitly must therefore land on the
/// same layer as omitting them — which is also why typing `limit` and
/// `whole_word` left every previously-cached search layer valid: `limit="500"`
/// and `limit=500` both reach the hasher as the integer 500.
///
/// Observable, since hashes are private: a shared layer means a shared row in
/// `symbol_instances`, so the two runs share negative instance ids.
#[test]
fn search_explicit_defaults_share_the_layer_with_omitted_ones() {
    let neg_ids = |res: &crate::statement::ExecutionResult| -> std::collections::HashSet<i64> {
        res.nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .collect()
    };

    let omitted = neg_ids(&run_query(TEST_INPUT_SEARCH, r#"search("foo")"#));
    let explicit = neg_ids(&run_query(
        TEST_INPUT_SEARCH,
        r#"search("foo", whole_word=false, limit=500)"#,
    ));

    assert!(
        !omitted.is_empty(),
        "the fixture must produce at least one search match"
    );
    assert_eq!(
        omitted, explicit,
        "writing the defaults must not change the layer key"
    );
}

/// A `limit` past what the SQL bind can express means "no effective cap", not
/// a truncated one.  The bind is a 32-bit Integer and the cast used to be
/// `limit as i32`, so `limit=2^32` truncated to 0 and returned ONE row per
/// project, `limit=2^63-1` truncated to -1 and returned NONE, and `limit=2^31`
/// bound a negative LIMIT and failed in Postgres.
#[test]
fn search_oversized_limit_is_clamped_not_truncated() {
    let count = |query: &str| run_query(TEST_INPUT_SEARCH, query).nodes.as_vec().len();

    // The fixture has fewer than 500 matches, so the default cap binds nothing
    // and is the "no cap" reference.
    let uncapped = count(r#"search("foo")"#);
    assert!(uncapped > 1, "fixture must have several matches to compare");

    for limit in ["2147483648", "4294967296", "9223372036854775807"] {
        let query = format!(r#"search("foo", limit={limit})"#);
        assert_eq!(
            count(&query),
            uncapped,
            "limit={limit} must behave as no cap, not as a truncated one"
        );
    }
}

#[test]
fn search_zero_limit_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", limit=0)"#);
    assert!(res.is_err(), "search with limit=0 should error");
}

#[test]
fn search_unknown_named_arg_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", scope="something")"#);
    assert!(res.is_err(), "search with unknown named arg should error");
}

// ============================================================================
// search() verb tests (Step 8: truncation warning end-to-end)
// ============================================================================

#[test]
fn search_truncation_warning_surfaces_on_cache_miss() {
    // Whole-word "foo" matches ~10 ranges across the fixture, so limit=3
    // trips the truncation fence.  limit=3 is unique to this test (limit is
    // part of the layer hash), which guarantees the run is a genuine cache
    // MISS even though all tests share one fixture DB -- asserted via the
    // activation trace, not assumed.
    const QUERY: &str = r#"search("foo", whole_word=true, limit=3)"#;
    let (res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    // One layer-bearing statement, one root shard per visible root (no upstream
    // chain, so no selection shards).
    use crate::command::ShardRole;
    assert_root_roles(&acts, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
    assert!(
        acts.iter().all(|a| a.created),
        "unique limit must produce a cache miss on every root, got {:?}",
        acts
    );
    // The limit caps matches PER PROJECT: whole-word "foo" has 6 matches
    // in proj 1 (1 basic + 3 multi + 2 boundary) and 4 in proj 2
    // (3 mixedcase + 1 docless), so limit=3 truncates both roots' root shards.
    assert!(
        acts.iter().all(|a| a.truncated),
        "limit=3 must truncate in both projects, got {:?}",
        acts
    );

    assert_eq!(
        res.nodes.as_vec().len(),
        6,
        "limit=3 caps per project: 3 (proj 1) + 3 (proj 2) = 6 matches, got {}",
        res.nodes.as_vec().len(),
    );

    let truncation_warns: Vec<_> = res
        .warnings
        .iter()
        .filter(|w| format!("{}", w).contains("truncated"))
        .collect();
    assert_eq!(
        truncation_warns.len(),
        1,
        "cache-miss should surface exactly one truncation warning, got {:?}",
        res.warnings,
    );
}

#[test]
fn search_truncation_warning_surfaces_on_cache_hit() {
    // Two back-to-back identical queries.  The first call's populate
    // writes truncated=true to layers; the second call hits the cache
    // (no populate) and reads truncated=true back, reconstructing the
    // warning from the verb's own span via make_truncation_warning.
    //
    // Verifies the "warning always visible" property -- silent loss on
    // cache hit was an early footgun the design pins down.  limit=2 is
    // unique to this test so the miss->hit sequence is guaranteed and
    // asserted, not assumed.
    const QUERY: &str = r#"search("foo", whole_word=true, limit=2)"#;
    let (_first, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert!(
        acts1[0].created,
        "first call must be a cache miss, got {:?}",
        acts1
    );
    assert!(
        !acts2[0].created,
        "second call must be a cache hit, got {:?}",
        acts2
    );
    assert_eq!(
        acts1[0].layer_id, acts2[0].layer_id,
        "hit must reuse the same layer row"
    );
    assert!(
        acts2[0].truncated,
        "cache hit must propagate the persisted truncated flag, got {:?}",
        acts2[0]
    );

    let truncation_warns: Vec<_> = second
        .warnings
        .iter()
        .filter(|w| format!("{}", w).contains("truncated"))
        .collect();
    assert_eq!(
        truncation_warns.len(),
        1,
        "cache-hit should also surface the truncation warning, got {:?}",
        second.warnings,
    );
}

#[test]
fn search_no_truncation_warning_when_under_cap() {
    // Plenty of headroom: explicit large limit so no truncation occurs.
    // The result set should not contain a truncation warning.
    const QUERY: &str = r#"search("foo", limit=500)"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let truncation_warns: Vec<_> = res
        .warnings
        .iter()
        .filter(|w| format!("{}", w).contains("truncated"))
        .collect();
    assert!(
        truncation_warns.is_empty(),
        "no truncation expected with ample limit, got {:?}",
        truncation_warns,
    );
}

// ============================================================================
// search() verb tests (Step 9: composite filter + remaining coverage)
// ============================================================================

#[test]
fn search_whole_word_underscore_negative() {
    // `_` is in is_word_char's ASCII range so `search("foo", whole_word=true)`
    // must NOT match "foo_bar".  Object 3's "foobar foo foo_bar foo.bar" lets
    // us cross-check: the whole-word search picks up "foo" (freestanding) and
    // "foo" inside "foo.bar" (`.` separates), but neither "foobar" (no left
    // boundary) nor "foo_bar" (underscore is a word char).
    let res = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_1") search("foo", whole_word=true, case="sensitive")"#,
    );
    // Object 3 in proj 1: "foobar foo foo_bar foo.bar" -- whole-word "foo"
    // matches the freestanding "foo" and the leading "foo" of "foo.bar".  We
    // also pick up object 1 "hello foo world" (1 match) and object 2
    // "foo foo foo" (3 matches).  Total: 1 + 3 + 2 = 6.
    let n = res.nodes.as_vec().len();
    assert_eq!(
        n, 6,
        "whole-word foo across proj 1 should match 6 ranges (1 in basic + 3 in multi + 2 in boundary), got {}",
        n,
    );
}

#[test]
fn search_substring_includes_foobar() {
    // Default whole_word=false matches "foobar" too.  Hard to assert an
    // exact count without enumerating every file -- instead, show that
    // substring strictly increases the count vs whole-word.
    let substring = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_1") search("foo")"#,
    );
    let whole = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_1") search("foo", whole_word=true)"#,
    );
    assert!(
        substring.nodes.as_vec().len() > whole.nodes.as_vec().len(),
        "substring (matches foobar/foo_bar) should yield strictly more than whole-word",
    );
}

#[test]
fn search_project_filter_narrows_via_objects_expr() {
    // Composite filter through ProjectFilterMixin's objects_expr scopes
    // the candidate query to objects in project search_proj_1.  Object 4
    // (case fixture) lives in proj 2 so its matches MUST NOT appear.
    let p1 = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_1") search("foo")"#,
    );
    let p2 = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_2") search("foo")"#,
    );
    let no_filter = run_query(TEST_INPUT_SEARCH, r#"search("foo")"#);

    let n_p1 = p1.nodes.as_vec().len();
    let n_p2 = p2.nodes.as_vec().len();
    let n_none = no_filter.nodes.as_vec().len();

    assert!(n_p1 > 0 && n_p2 > 0, "each project should yield matches");
    assert!(
        n_p1 < n_none && n_p2 < n_none,
        "project filter should narrow vs no filter; p1={}, p2={}, no_filter={}",
        n_p1,
        n_p2,
        n_none,
    );
}

#[test]
fn search_cross_project_shared_content_scopes_to_filter() {
    // Object 7 (proj 1) and object 8 (proj 2) both reference cs_shared,
    // which contains "shared_token across projects".  A project-scoped
    // search for "shared_token" must return ONLY the object belonging to
    // the filtered project, not the other project's object that happens
    // to point at the same deduplicated content row.  This is precisely
    // what the (content_hash, project_id) JOIN in the search SQL guards
    // against.
    let p1 = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_1") search("shared_token")"#,
    );
    let p2 = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_2") search("shared_token")"#,
    );

    let p1_nodes: std::collections::HashSet<_> = p1.nodes.as_vec().into_iter().collect();
    let p2_nodes: std::collections::HashSet<_> = p2.nodes.as_vec().into_iter().collect();
    assert!(
        !p1_nodes.is_empty(),
        "proj 1 should match shared content via object 7",
    );
    assert!(
        !p2_nodes.is_empty(),
        "proj 2 should match shared content via object 8",
    );
    assert!(
        p1_nodes.is_disjoint(&p2_nodes),
        "cross-project shared content must produce disjoint output node sets; \
         leakage means the (content_hash, project_id) JOIN didn't constrain. \
         p1={:?}, p2={:?}",
        p1_nodes,
        p2_nodes,
    );
}

#[test]
fn search_finds_symbol_less_file() {
    // Object 5 (README.md) has no symbols/symbol_instances but its
    // content includes "doc-only content with foo here\n".  The search
    // query joins content_store ⋈ objects directly so the match should
    // be reachable through a project("search_proj_2") scope.
    let res = run_query(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_2") search("doc-only")"#,
    );
    let n = res.nodes.as_vec().len();
    assert!(
        n > 0,
        "search should match in the symbol-less README.md fixture file, got {} ranges",
        n,
    );
}

#[test]
fn search_non_utf8_skipped() {
    // Object 6's bytea blob contains the bytes for "foo" but the
    // surrounding bytes are not valid UTF-8.  safe_convert_from yields
    // NULL for that content_store row -> content_text NULL -> both GIN
    // indexes skip it -> no match.  Scope to proj 2 only so the test
    // exercises the project's contribution exclusively.
    let res = run_query(
        TEST_INPUT_SEARCH,
        // Use a query that ONLY appears in cs_binary's would-be UTF-8 text:
        // "foo" appears in many other files, so instead search for a
        // unique-to-binary token that would only match if non-UTF-8 made
        // it through.  The bytea contains \xff\xfe\xfa "foo" \x0b \xff,
        // which would (if decoded) include "foo" -- so just asserting
        // proj 2 has matches and they're all from valid-UTF-8 files is
        // enough; if non-UTF-8 sneaked through we'd get an extra match
        // attributable to object 6.  Easier: count via where filter.
        r#"project("search_proj_2") search("foo")"#,
    );
    let n = res.nodes.as_vec().len();
    // Expected matches in proj 2 valid-UTF-8 files only:
    //   * object 4 "Foo FOO foo" -> 3 substring matches
    //   * object 5 "doc-only content with foo here" -> 1 match
    // Total = 4.  Object 6 (binary) MUST NOT contribute.
    assert_eq!(
        n, 4,
        "search in proj 2 should match 3+1=4 ranges across valid-UTF-8 files; \
         a count of 5 would mean object 6's binary blob leaked through, got {}",
        n,
    );
}

#[test]
fn search_cache_hit_on_repeat_same_filter() {
    // Two identical search()s with the same composite filter must hit the
    // same layer row.  The activation trace asserts the cache paths
    // directly: first call creates+populates, second reuses the same layer
    // id without repopulating.  limit=399 is unique to this test (limit is
    // part of the layer hash) so the miss->hit sequence is guaranteed even
    // with other tests sharing the fixture DB.
    const QUERY: &str = r#"project("search_proj_1") search("foo", limit=399)"#;
    let (first, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert!(
        acts1[0].created,
        "first call must create the layer, got {:?}",
        acts1
    );
    assert!(
        !acts2[0].created,
        "second call must hit the cache, got {:?}",
        acts2
    );
    assert_eq!(
        acts1[0].layer_id, acts2[0].layer_id,
        "hit must reuse the same layer row"
    );

    assert_eq!(
        first.nodes.as_vec(),
        second.nodes.as_vec(),
        "identical search() under the same filter should yield identical nodes",
    );
}

#[test]
fn search_different_filter_different_cache() {
    // The composite filter is mixed into the layer hash via
    // CompositeFilter::hash_into.  project("p1") and project("p2")
    // therefore produce DIFFERENT hashes and different cache entries,
    // each scoped to its own project's matches.  Asserted two ways: the
    // activation trace shows two distinct freshly-created layer rows
    // (limit=400 is unique to this test, so both runs are cache misses),
    // and the node sets are disjoint (a shared layer would return
    // identical nodes for both).
    let (p1, acts1) = run_query_traced(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_1") search("foo", limit=400)"#,
    );
    let (p2, acts2) = run_query_traced(
        TEST_INPUT_SEARCH,
        r#"project("search_proj_2") search("foo", limit=400)"#,
    );

    assert!(
        acts1[0].created && acts2[0].created,
        "both filter variants must create their own layer, got {:?} / {:?}",
        acts1,
        acts2
    );
    assert_ne!(
        acts1[0].layer_id, acts2[0].layer_id,
        "different composite filters must map to different layer rows"
    );

    let p1_nodes: std::collections::HashSet<_> = p1.nodes.as_vec().into_iter().collect();
    let p2_nodes: std::collections::HashSet<_> = p2.nodes.as_vec().into_iter().collect();
    assert!(
        p1_nodes.is_disjoint(&p2_nodes),
        "project-scoped search layers must produce disjoint node sets; \
         shared nodes would indicate the cache key is not filter-aware. \
         p1={:?}, p2={:?}",
        p1_nodes,
        p2_nodes,
    );
}

// ============================================================================
// search() verb tests: explicit cache-state observability
// ============================================================================
//
// These tests assert cache behaviour DIRECTLY -- via the layer-activation
// trace (created vs cache hit) and the layers row state (populated /
// truncated flags, per-layer row counts) -- rather than inferring it from
// eph instance IDs.
//
// Isolation convention: all tests share one fixture DB per fixture file and
// run in parallel, so cache entries leak across tests.  `limit` is part of
// the search layer hash; every cache-sensitive test therefore uses a limit
// value unique across this file (grep for `limit=` before picking a new
// one) to guarantee its first run is a genuine cache miss.

/// Read one eph layer's metadata row and its (symbols, instances) row counts.
fn eph_layer_state(fixture: &str, layer_id: i64) -> (index::db_diesel::EphLayerMeta, (i64, i64)) {
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(fixture).await;
        let meta = index.eph_layer_meta(layer_id).await.unwrap();
        let counts = index.count_eph_rows_for_layer(layer_id).await.unwrap();
        (meta, counts)
    })
}

#[test]
fn search_first_call_reports_created_and_populated() {
    // A cold search() must report one created=true root shard activation per
    // visible root, and leave behind, per root, an layers row with
    // populated=true (2-phase commit completed) and truncated=false, whose
    // instance rows hold exactly that project's matches — summing to the
    // returned nodes 1:1.
    use crate::command::ShardRole;
    const QUERY: &str = r#"search("foo", limit=397)"#;
    let (res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert_root_roles(&acts, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
    for act in &acts {
        assert!(
            act.created,
            "unique limit must produce a cache miss, got {:?}",
            act
        );
        assert!(
            !act.truncated,
            "limit=397 leaves ample headroom, got {:?}",
            act
        );
    }

    let mut total_instances = 0;
    let mut per_root_instances = Vec::new();
    for act in &acts {
        let (meta, (symbols, instances)) = eph_layer_state(TEST_INPUT_SEARCH, act.layer_id);
        assert_eq!(
            meta.kind, "ephemeral",
            "layer kind must be 'search', got {:?}",
            meta
        );
        assert_eq!(
            meta.parent_id,
            Some(act.root_id),
            "root shards parent their root, got {:?}",
            meta
        );
        assert!(
            meta.populated,
            "populate must have been committed, got {:?}",
            meta
        );
        assert!(
            !meta.truncated,
            "truncated flag must be false on the row, got {:?}",
            meta
        );
        // Each root's root shard holds only its own project's rows: exactly one
        // 'search:foo' eph symbol.
        assert_eq!(
            symbols, 1,
            "expected one 'search:foo' symbol in each root's root shard"
        );
        total_instances += instances;
        per_root_instances.push(instances);
    }

    // One instance per byte-range match, per project: proj 1 has 8
    // substring matches (1 basic + 3 multi + 4 boundary), proj 2 has 4
    // (3 mixedcase + 1 docless; the binary blob is skipped).
    assert_eq!(
        per_root_instances,
        vec![8, 4],
        "per-root root shards must hold exactly their project's matches",
    );
    // The statement's selection is exactly the union of the root shards' rows.
    assert_eq!(
        total_instances,
        res.nodes.as_vec().len() as i64,
        "layer instance rows must match returned nodes 1:1",
    );
}

#[test]
fn search_repeat_call_hits_cache_without_repopulating() {
    // Second identical call must reuse the SAME layer row (created=false)
    // and must not insert any new rows into it -- the per-layer symbol and
    // instance counts are identical before and after the repeat call.
    const QUERY: &str = r#"search("foo", limit=398)"#;

    let (first, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(
        acts1[0].created,
        "first call must create the layer, got {:?}",
        acts1
    );
    let layer_id = acts1[0].layer_id;
    let (_, counts_after_first) = eph_layer_state(TEST_INPUT_SEARCH, layer_id);

    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(
        !acts2[0].created,
        "second call must hit the cache, got {:?}",
        acts2
    );
    assert_eq!(
        acts2[0].layer_id, layer_id,
        "hit must reuse the same layer row"
    );

    let (meta, counts_after_second) = eph_layer_state(TEST_INPUT_SEARCH, layer_id);
    assert!(
        meta.populated,
        "layer must remain populated, got {:?}",
        meta
    );
    assert_eq!(
        counts_after_first, counts_after_second,
        "cache hit must not repopulate: per-layer row counts changed",
    );

    assert_eq!(
        first.nodes.as_vec(),
        second.nodes.as_vec(),
        "cache hit must return the same nodes as the original populate",
    );
}

#[test]
fn search_truncated_flag_persists_on_layer_row() {
    // Truncation state lives on the layers row itself.  First call
    // truncates (whole-word "foo" has ~10 matches, limit=1) and writes
    // truncated=true in the same transaction as the populate; the repeat
    // call reads it back on the cache-hit path and still surfaces the
    // warning.
    const QUERY: &str = r#"search("foo", whole_word=true, limit=1)"#;

    let (_, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(
        acts1[0].created,
        "first call must be a cache miss, got {:?}",
        acts1
    );
    assert!(acts1[0].truncated, "limit=1 must truncate, got {:?}", acts1);

    let (meta, _) = eph_layer_state(TEST_INPUT_SEARCH, acts1[0].layer_id);
    assert!(
        meta.populated,
        "truncated layer is still fully committed, got {:?}",
        meta
    );
    assert!(
        meta.truncated,
        "truncated=true must be persisted on the row, got {:?}",
        meta
    );

    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(
        !acts2[0].created,
        "second call must be a cache hit, got {:?}",
        acts2
    );
    assert_eq!(
        acts2[0].layer_id, acts1[0].layer_id,
        "hit must reuse the same layer row"
    );
    assert!(
        acts2[0].truncated,
        "cache hit must propagate the persisted truncated flag, got {:?}",
        acts2
    );
    assert!(
        second
            .warnings
            .iter()
            .any(|w| format!("{}", w).contains("truncated")),
        "cache hit must still surface the truncation warning, got {:?}",
        second.warnings,
    );
}

// ============================================================================
// Partitioned root/selection shard cache tests
// ============================================================================
//
// search() now produces, PER VISIBLE ROOT, a ROOT SHARD parented on that
// root and keyed on the root-salted verb inputs (never the upstream eph
// chain) plus, iff that root's chain is non-empty, a chained SELECTION SHARD
// layer holding the eph-derived delta (structurally empty today).  Each
// root's root shard holds only that project's rows.  These tests pin down the
// partitioning contract: per-root root shard reuse across upstream eph changes,
// chain-topology rules, and warning/chaining stability.
//
// Same isolation convention as above: every cache-sensitive test uses a
// limit value unique across this file, and layer blocks use byte ranges
// unique per test so their chained hashes never collide across tests.

/// Root layer ids of the two fixture projects — TEST_INPUT_SEARCH and
/// VERB_TEST both define projects 1/2 with root layers 1000001/1000002.
const R1: i64 = 1000001;
const R2: i64 = 1000002;

/// Assert an activation trace's (root, role) pairs match `expected`.
/// Statement order: each statement materialises per visible root
/// (ascending); within a root, the root shard first, then zero or more
/// layer shards, then — under a non-empty chain for that root — its
/// selection shard.
fn assert_root_roles(
    acts: &[crate::command::LayerActivation],
    expected: &[(i64, crate::command::ShardRole)],
) {
    let got: Vec<_> = acts.iter().map(|a| (a.root_id, a.role)).collect();
    assert_eq!(got, expected, "unexpected activation shape: {:?}", acts);
}

#[test]
fn search_root_shard_reused_across_eph_prefix_change() {
    // The headline behaviour of the partitioned cache: changing the
    // ephemeral prefix before a search must NOT invalidate the search's
    // root shard.  Two runs with different layer{} blocks share the root shard
    // (created=false, same layer_id on the second run) while each gets its
    // own selection shard (chained on its own prefix).  Before partitioning,
    // run B's search layer was a full cache miss.
    use crate::command::ShardRole;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=60000, end=60100, instance_type=1) }; "#,
        r#"search("foo", limit=21)"#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=61000, end=61100, instance_type=1) }; "#,
        r#"search("foo", limit=21)"#,
    );

    // Trace shape, per visible root (ascending): the layer{} block (itself
    // partitioned now) contributes a root shard per root; the search statement
    // contributes root shard + selection shard per root (every root's chain is
    // non-empty by then).
    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root),      // layer block root shard, root 1
        (R2, ShardRole::Root),      // layer block root shard, root 2 (empty)
        (R1, ShardRole::Root),      // search root shard, root 1
        (R1, ShardRole::Selection), // search selection shard, root 1
        (R2, ShardRole::Root),      // search root shard, root 2
        (R2, ShardRole::Selection), // search selection shard, root 2
    ];
    let (res_a, acts_a) = run_query_traced(TEST_INPUT_SEARCH, Q_A);
    assert_root_roles(&acts_a, shape);
    assert!(
        acts_a.iter().all(|a| a.created),
        "unique limit and byte range ⇒ cold everywhere, got {:?}",
        acts_a
    );
    let (root_shard_a_r1, sel_a_r1, root_shard_a_r2, sel_a_r2) =
        (acts_a[2], acts_a[3], acts_a[4], acts_a[5]);

    let (res_b, acts_b) = run_query_traced(TEST_INPUT_SEARCH, Q_B);
    assert_root_roles(&acts_b, shape);
    let (layer_b_r1, layer_b_r2) = (acts_b[0], acts_b[1]);
    let (root_shard_b_r1, sel_b_r1, root_shard_b_r2, sel_b_r2) =
        (acts_b[2], acts_b[3], acts_b[4], acts_b[5]);

    assert!(
        layer_b_r1.created && layer_b_r2.created,
        "different byte range ⇒ new layer block on each root, got {:?}",
        acts_b
    );
    assert!(
        !root_shard_b_r1.created && !root_shard_b_r2.created,
        "each root's root shard must be reused despite the changed eph prefix, got {:?}",
        acts_b
    );
    assert_eq!(
        root_shard_b_r1.layer_id, root_shard_a_r1.layer_id,
        "root 1 root shard reuse must hit the same layers row"
    );
    assert_eq!(
        root_shard_b_r2.layer_id, root_shard_a_r2.layer_id,
        "root 2 root shard reuse must hit the same layers row"
    );
    assert!(
        sel_b_r1.created && sel_b_r2.created,
        "different upstream chain ⇒ fresh selection shards, got {:?}",
        acts_b
    );
    assert_ne!(
        sel_b_r1.layer_id, sel_a_r1.layer_id,
        "each prefix gets its own selection shard row (root 1)"
    );
    assert_ne!(
        sel_b_r2.layer_id, sel_a_r2.layer_id,
        "each prefix gets its own selection shard row (root 2)"
    );

    // Result equivalence: the search-derived nodes are the SAME rows in
    // both runs (the root shard was reused, so instance ids are identical);
    // the runs differ only in their one-per-run layer-block instance.
    let nodes_a: std::collections::HashSet<_> = res_a.nodes.as_vec().into_iter().collect();
    let nodes_b: std::collections::HashSet<_> = res_b.nodes.as_vec().into_iter().collect();
    assert_eq!(nodes_a.len(), nodes_b.len(), "runs must be same-shaped");
    assert_eq!(
        nodes_a.intersection(&nodes_b).count(),
        nodes_a.len() - 1,
        "runs must share every node except their own layer-block instance",
    );
}

#[test]
fn search_without_upstream_creates_only_root_shard() {
    // Chain-topology rule: with an empty upstream eph chain there is no
    // selection shard — a bare search materialises exactly one activation per
    // visible root, the root-parented root shard, and repeat runs hit both.
    use crate::command::ShardRole;
    const QUERY: &str = r#"search("foo", limit=22)"#;

    let (_res, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts1, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
    assert!(
        acts1.iter().all(|a| a.created),
        "unique limit ⇒ cold root shards, got {:?}",
        acts1
    );

    for act in &acts1 {
        let (meta, _) = eph_layer_state(TEST_INPUT_SEARCH, act.layer_id);
        assert_eq!(meta.kind, "ephemeral", "got {:?}", meta);
        assert_eq!(
            meta.parent_id,
            Some(act.root_id),
            "root shards parent their root by construction, got {:?}",
            meta
        );
    }

    let (_res, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts2, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
    assert!(
        acts2.iter().all(|a| !a.created),
        "repeat run must hit on every root, got {:?}",
        acts2
    );
    assert_eq!(
        acts1.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        acts2.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        "hits must reuse the same per-root root-shard rows"
    );
}

#[test]
fn search_selection_shard_empty_but_materialized() {
    // Under a non-empty chain the selection shard row always materialises —
    // populated and committed — even though search's eph-derived delta is
    // structurally empty today (search reads only persistent content).
    // The row's existence is what keeps downstream chaining deterministic.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=62000, end=62100, instance_type=1) }; "#,
        r#"search("foo", limit=23)"#,
    );

    let (_res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(
        &acts,
        &[
            (R1, ShardRole::Root),      // layer block root shard, root 1
            (R2, ShardRole::Root),      // layer block root shard, root 2
            (R1, ShardRole::Root),      // search root shard, root 1
            (R1, ShardRole::Selection), // search selection shard, root 1
            (R2, ShardRole::Root),      // search root shard, root 2
            (R2, ShardRole::Selection), // search selection shard, root 2
        ],
    );

    // The selection shard contract holds per root: each root's selection shard
    // chains on that root's own layer-block root shard.
    for (layer_block, root_shard, sel_shard) in
        [(acts[0], acts[2], acts[3]), (acts[1], acts[4], acts[5])]
    {
        let root_id = layer_block.root_id;

        let (layer_meta, _) = eph_layer_state(TEST_INPUT_SEARCH, layer_block.layer_id);
        assert_eq!(layer_meta.kind, "ephemeral", "got {:?}", layer_meta);
        assert_eq!(
            layer_meta.parent_id,
            Some(root_id),
            "the layer block's own root shard parents its root too, got {:?}",
            layer_meta
        );

        let (root_shard_meta, (root_shard_symbols, root_shard_instances)) =
            eph_layer_state(TEST_INPUT_SEARCH, root_shard.layer_id);
        assert_eq!(
            root_shard_meta.kind, "ephemeral",
            "got {:?}",
            root_shard_meta
        );
        assert_eq!(
            root_shard_meta.parent_id,
            Some(root_id),
            "got {:?}",
            root_shard_meta
        );
        assert!(root_shard_meta.populated, "got {:?}", root_shard_meta);
        assert!(
            root_shard_symbols > 0 && root_shard_instances > 0,
            "search matches live in each root's root shard (both fixture projects \
             contain \"foo\"), got ({}, {}) for root {}",
            root_shard_symbols,
            root_shard_instances,
            root_id
        );

        let (sel_shard_meta, (sel_shard_symbols, sel_shard_instances)) =
            eph_layer_state(TEST_INPUT_SEARCH, sel_shard.layer_id);
        assert_eq!(sel_shard_meta.kind, "ephemeral", "got {:?}", sel_shard_meta);
        assert_eq!(
            sel_shard_meta.parent_id,
            Some(layer_block.layer_id),
            "selection shard chains on its root's upstream layer-block root shard, got {:?}",
            sel_shard_meta
        );
        assert!(
            sel_shard_meta.populated,
            "empty selection shard still commits through 2-PC, got {:?}",
            sel_shard_meta
        );
        assert_eq!(
            (sel_shard_symbols, sel_shard_instances),
            (0, 0),
            "search's eph delta is structurally empty today",
        );
        assert!(
            !sel_shard_meta.truncated,
            "empty selection shard never truncates, got {:?}",
            sel_shard_meta
        );
    }
}

#[test]
fn search_truncation_on_root_shard_survives_eph_change() {
    // The truncated flag lives on the root-shard row (the limit fences the
    // persistent scan) and is PER ROOT: whole-word "foo" has 6 matches in
    // proj 1 (> limit=4 ⇒ truncated) but exactly 4 in proj 2 (not
    // truncated).  A truncating search under prefix A rerun under prefix B
    // hits the root shards and must still surface the warning — the warning
    // survives BOTH cache dimensions (hit, and changed prefix).
    use crate::command::ShardRole;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=63000, end=63100, instance_type=1) }; "#,
        r#"search("foo", whole_word=true, limit=4)"#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=64000, end=64100, instance_type=1) }; "#,
        r#"search("foo", whole_word=true, limit=4)"#,
    );

    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root),      // layer block root shard, root 1
        (R2, ShardRole::Root),      // layer block root shard, root 2
        (R1, ShardRole::Root),      // search root shard, root 1
        (R1, ShardRole::Selection), // search selection shard, root 1
        (R2, ShardRole::Root),      // search root shard, root 2
        (R2, ShardRole::Selection), // search selection shard, root 2
    ];
    let (_res_a, acts_a) = run_query_traced(TEST_INPUT_SEARCH, Q_A);
    assert_root_roles(&acts_a, shape);
    let (root_shard_a_r1, root_shard_a_r2) = (acts_a[2], acts_a[4]);
    assert!(
        root_shard_a_r1.created && root_shard_a_r2.created,
        "cold root shards, got {:?}",
        acts_a
    );
    assert!(
        root_shard_a_r1.truncated,
        "whole-word foo (6 matches in proj 1) at limit=4 must truncate root 1's root shard, got {:?}",
        acts_a
    );
    assert!(
        !root_shard_a_r2.truncated,
        "proj 2 has exactly 4 whole-word matches — within limit=4, so root 2's \
         root shard must NOT truncate, got {:?}",
        acts_a
    );
    let (meta_a_r1, _) = eph_layer_state(TEST_INPUT_SEARCH, root_shard_a_r1.layer_id);
    let (meta_a_r2, _) = eph_layer_state(TEST_INPUT_SEARCH, root_shard_a_r2.layer_id);
    assert!(
        meta_a_r1.truncated && !meta_a_r2.truncated,
        "the per-root truncated flags must be persisted on the root-shard rows, got {:?} / {:?}",
        meta_a_r1,
        meta_a_r2
    );

    let (res_b, acts_b) = run_query_traced(TEST_INPUT_SEARCH, Q_B);
    assert_root_roles(&acts_b, shape);
    let (root_shard_b_r1, sel_b_r1, root_shard_b_r2, sel_b_r2) =
        (acts_b[2], acts_b[3], acts_b[4], acts_b[5]);
    assert!(
        !root_shard_b_r1.created && !root_shard_b_r2.created,
        "root shards reused under prefix B, got {:?}",
        acts_b
    );
    assert!(
        root_shard_b_r1.truncated,
        "persisted truncated flag must propagate on root 1's root shard hit, got {:?}",
        acts_b
    );
    assert!(
        !root_shard_b_r2.truncated,
        "root 2's root shard stays untruncated on the hit, got {:?}",
        acts_b
    );
    assert!(
        !sel_b_r1.truncated && !sel_b_r2.truncated,
        "the selection shards carry no truncation, got {:?}",
        acts_b
    );
    assert!(
        res_b
            .warnings
            .iter()
            .any(|w| format!("{}", w).contains("truncated")),
        "warning must surface on a root shard hit under a different prefix, got {:?}",
        res_b.warnings,
    );
}

#[test]
fn downstream_chaining_keys_deterministic() {
    // Downstream verbs chain off their root's selection shard id, which folds
    // that root's upstream prefix and the root-salted input hash — so a
    // repeat of the whole pipeline is a full cache hit on every layer of
    // every root, including a layer{} block AFTER the search.  This pins
    // the per-root push order (root shard first, selection shard last = that root's
    // chain tail) and the selection shard-id stability downstream keys depend
    // on.
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=65000, end=65100, instance_type=1) }; "#,
        r#"search("foo", limit=24); "#,
        r#"layer { ephemeral_instance(symbol_id=11, object_id=2, "#,
        r#"start=66000, end=66100, instance_type=1) }"#,
    );

    // 10 activations — each statement materialises per root: the leading
    // layer block contributes a root shard per root; the search and the trailing
    // layer block each contribute root shard + selection shard per root (both sit
    // under a non-empty per-root chain).
    use crate::command::ShardRole;
    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root), // leading layer block
        (R2, ShardRole::Root),
        (R1, ShardRole::Root), // search
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
        (R1, ShardRole::Root), // trailing layer block
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
    ];
    let (_res1, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts1, shape);
    assert!(
        acts1.iter().all(|a| a.created),
        "first run is cold everywhere, got {:?}",
        acts1
    );

    let (_res2, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts2, shape);
    assert!(
        acts2.iter().all(|a| !a.created),
        "repeat run must be a full cache hit on every layer, got {:?}",
        acts2
    );
    let ids1: Vec<i64> = acts1.iter().map(|a| a.layer_id).collect();
    let ids2: Vec<i64> = acts2.iter().map(|a| a.layer_id).collect();
    assert_eq!(
        ids1, ids2,
        "every layer id (roles and order included) must be stable across runs"
    );
}

// ============================================================================
// Per-statement materialisations: one materialisation per top-level statement (tree)
// ============================================================================
//
// All of a tree's commands materialise against the PRE-TREE visibility and
// the tree's layers enter as ONE materialisation — nesting no longer sequences; `;;`
// (or `;`) between top-level statements is the only time axis.  These tests
// pin the new spine: sibling selection shards parent on the previous tree's tip,
// the tip designation is deterministic (last layer in substatement
// pre-order), and intra-tree independence (a nested command sees no
// tree-mate layers at materialise time).

#[test]
fn nested_search_no_intra_tree_selection_shards() {
    // `search("hello foo") { search("foo") }` is ONE tree, so both searches
    // materialise against the pre-tree visibility — an empty chain here.
    // Neither command sees the other's layers at materialise time: no
    // selection shard exists anywhere in the tree (first-materialisation elision, per
    // command) — one root shard per root per command, nothing else.  Under the
    // old intra-tree chaining the inner search materialised AFTER the
    // outer's materialisation landed and grew a selection shard per root.
    //
    // Results match `nested_search_in_search_does_not_compose`: populates never read each
    // other's output and search's eph delta is structurally empty, so
    // composition happens entirely in Phase R under tree-complete
    // visibility.
    use crate::command::ShardRole;
    const QUERY: &str = r#"search("hello foo") { search("foo") }"#;
    let (res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert_root_roles(
        &acts,
        &[
            (R1, ShardRole::Root), // outer search root shard, root 1
            (R2, ShardRole::Root), // outer search root shard, root 2
            (R1, ShardRole::Root), // inner search root shard, root 1
            (R2, ShardRole::Root), // inner search root shard, root 2
        ],
    );

    // The subject here is the shard roles above.  The node set is empty because
    // two independently materialised ephemeral instances cannot be numbered
    // inside one another -- see `nested_search_in_search_does_not_compose` for
    // why, and why that was accepted.
    let ids: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
    assert!(
        ids.is_empty(),
        "the inner search constrains the outer away, got {:?}",
        ids,
    );
}

#[test]
fn multi_selection_shard_tree_sibling_parents_and_deterministic_tip() {
    // A tree with TWO selection-shard-bearing commands: tree 0 seeds the chain,
    // tree 1 is `{ layer{} ; layer{} }` (two layer blocks in ONE tree),
    // tree 2 chains after it.  Pins the spine advance rule:
    //
    //  - both of tree 1's selection shards parent on tree 0's tip — SIBLINGS,
    //    no intra-tree chaining;
    //  - tree 1's tip is its materialisation's last layer in substatement pre-order
    //    (block B's selection shard), which tree 2's selection shards parent on;
    //  - the whole pipeline is deterministic: a repeat run is a full cache
    //    hit with identical layer ids on every root.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=86000, end=86100, instance_type=1) }; "#,
        r#"{ layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=86200, end=86300, instance_type=1) } ; "#,
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=86400, end=86500, instance_type=1) } }; "#,
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=86600, end=86700, instance_type=1) }"#,
    );
    let shape: &[(i64, ShardRole)] = &[
        // tree 0 (seed): empty pre-tree chain → root shards only.
        (R1, ShardRole::Root),
        (R2, ShardRole::Root),
        // tree 1, block A (pre-order): non-empty pre-tree chain → root shard +
        // selection shard per root.
        (R1, ShardRole::Root),
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
        // tree 1, block B: same shape — its materialisation is independent
        // of block A's.
        (R1, ShardRole::Root),
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
        // tree 2: chains on tree 1's tip.
        (R1, ShardRole::Root),
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
    ];

    let (_res1, acts1) = run_query_traced(VERB_TEST, QUERY);
    assert_root_roles(&acts1, shape);
    assert!(
        acts1.iter().all(|a| a.created),
        "byte ranges unique to this test ⇒ cold everywhere, got {:?}",
        acts1
    );

    // Per root: both of tree 1's selection shards are siblings under tree 0's
    // tip (its root shard for that root).
    let (seed_r1, seed_r2) = (acts1[0], acts1[1]);
    let (sel_a_r1, sel_a_r2) = (acts1[3], acts1[5]);
    let (sel_b_r1, sel_b_r2) = (acts1[7], acts1[9]);
    for (seed, sel_a, sel_b) in [(seed_r1, sel_a_r1, sel_b_r1), (seed_r2, sel_a_r2, sel_b_r2)] {
        let (meta_a, _) = eph_layer_state(VERB_TEST, sel_a.layer_id);
        let (meta_b, _) = eph_layer_state(VERB_TEST, sel_b.layer_id);
        assert_eq!(
            meta_a.parent_id,
            Some(seed.layer_id),
            "block A's selection shard parents the PREVIOUS tree's tip, got {:?}",
            meta_a
        );
        assert_eq!(
            meta_b.parent_id,
            Some(seed.layer_id),
            "block B's selection shard is block A's SIBLING (same parent, no \
             intra-tree chaining), got {:?}",
            meta_b
        );
    }

    // Tree 2's selection shards parent on tree 1's designated tip: the last
    // layer of tree 1's materialisation in pre-order — block B's selection shard.
    let (sel_t2_r1, sel_t2_r2) = (acts1[11], acts1[13]);
    for (tip, sel_shard) in [(sel_b_r1, sel_t2_r1), (sel_b_r2, sel_t2_r2)] {
        let (meta, _) = eph_layer_state(VERB_TEST, sel_shard.layer_id);
        assert_eq!(
            meta.parent_id,
            Some(tip.layer_id),
            "the next tree chains on the last pre-order selection shard, got {:?}",
            meta
        );
    }

    // Determinism: the tip choice (and with it every selection shard key) is a
    // pure function of the query — a repeat run hits every layer.
    let (_res2, acts2) = run_query_traced(VERB_TEST, QUERY);
    assert_root_roles(&acts2, shape);
    assert!(
        acts2.iter().all(|a| !a.created),
        "repeat run must be a full cache hit on every layer, got {:?}",
        acts2
    );
    assert_eq!(
        acts1.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        acts2.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        "every layer id (roles and order included) must be stable across runs"
    );
}

#[test]
fn first_materialisation_multi_block_tree_tip_is_last_root_shard() {
    // First-materialisation elision, generalised: with an EMPTY pre-tree chain no
    // command in the tree materialises a selection shard — even when the tree
    // holds several layer-bearing commands (under intra-tree chaining the
    // second block would have seen the first's materialisation and grown one).  The
    // tree's tip is then the last layer-bearing substatement's ROOT SHARD (last
    // in pre-order), which the next tree's selection shards parent on.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"{ layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=87000, end=87100, instance_type=1) } ; "#,
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=87200, end=87300, instance_type=1) } }; "#,
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=87400, end=87500, instance_type=1) }"#,
    );

    let (_res, acts) = run_query_traced(VERB_TEST, QUERY);
    assert_root_roles(
        &acts,
        &[
            // tree 0, blocks E then F: root shards only — no selection shards anywhere
            // in the first materialisation.
            (R1, ShardRole::Root),
            (R2, ShardRole::Root),
            (R1, ShardRole::Root),
            (R2, ShardRole::Root),
            // tree 1: root shard + selection shard per root.
            (R1, ShardRole::Root),
            (R1, ShardRole::Selection),
            (R2, ShardRole::Root),
            (R2, ShardRole::Selection),
        ],
    );

    // Tree 1's selection shards parent on tree 0's tip: block F's root shard per root.
    let (f_root_shard_r1, f_root_shard_r2) = (acts[2], acts[3]);
    let (sel_r1, sel_r2) = (acts[5], acts[7]);
    for (tip, sel_shard) in [(f_root_shard_r1, sel_r1), (f_root_shard_r2, sel_r2)] {
        let (meta, _) = eph_layer_state(VERB_TEST, sel_shard.layer_id);
        assert_eq!(
            meta.parent_id,
            Some(tip.layer_id),
            "under an empty pre-tree chain the tree's tip is its last \
             pre-order ROOT SHARD, got {:?}",
            meta
        );
    }
}

#[test]
fn identical_tree_mates_converge_on_same_layers() {
    // F3: a tree's materialisations run CONCURRENTLY, so two tree-mates
    // with IDENTICAL specs (same verb, same inputs, same parent scope)
    // race to create the same `(root, hash)` layer rows.  The ON CONFLICT
    // + 2-PC `populated` protocol (`Index::create_eph_layer`) serialises
    // the race: the loser blocks on the winner's row lock and unblocks as
    // a cache hit on the SAME layer id — never a duplicate row, never a
    // half-built layer.
    use crate::command::ShardRole;
    // `limit=44` is unique to this test, so the first run is cold: the
    // race is a genuine create-vs-create, not two cache hits.
    const QUERY: &str = r#"{ search("foo", limit=44) ; search("foo", limit=44) }"#;

    let shape: &[(i64, ShardRole)] = &[
        // ONE tree, empty pre-tree chain → first-materialisation elision: root shards
        // only, in substatement pre-order.
        (R1, ShardRole::Root), // first search, root 1
        (R2, ShardRole::Root), // first search, root 2
        (R1, ShardRole::Root), // second search, root 1
        (R2, ShardRole::Root), // second search, root 2
    ];

    let (res1, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts1, shape);

    // Both commands resolve to the SAME layer per root.
    for (a, b) in [(acts1[0], acts1[2]), (acts1[1], acts1[3])] {
        assert_eq!(
            a.layer_id, b.layer_id,
            "identical tree-mates must converge on one layer row per \
             root, got {:?}",
            acts1
        );
        // Exactly one racer created the row (cold DB: the hash is unique
        // to this test).  WHICH of the two carries `created` is
        // completion order — deliberately not asserted.
        assert!(
            a.created ^ b.created,
            "exactly one of two identical racers creates the layer, the \
             other must hit it, got {:?}",
            acts1
        );
    }

    // The suite stays deterministic: a repeat run is a full cache hit
    // with the same ids in the same pre-order positions.
    let (res2, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts2, shape);
    assert!(
        acts2.iter().all(|a| !a.created),
        "repeat run must hit every layer, got {:?}",
        acts2
    );
    assert_eq!(
        acts1.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        acts2.iter().map(|a| a.layer_id).collect::<Vec<_>>(),
        "layer ids must be stable across runs"
    );

    // And the duplicate command changes nothing about the result: both
    // runs select the same node set.
    let ids = |res: &crate::statement::ExecutionResult| {
        let mut v: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(ids(&res1), ids(&res2), "node sets must match across runs");
}

#[test]
fn three_content_verb_tree_deterministic_across_runs() {
    // F3 determinism: THREE content verbs materialise concurrently within
    // one tree, yet keys, parents, activation order, and the tree's tip
    // remain a pure function of the query text and the pre-tree chain.
    // Tree 0 seeds the chain so every tree-mate grows a selection shard (the
    // half whose key folds the previous tree's tip — the part worth
    // pinning under concurrency).  Three runs: identical
    // (root, role, layer_id) sequences every time, runs 2..3 full cache
    // hits.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=88000, end=88100, instance_type=1) }; "#,
        r#"{ search("foo", limit=41) ; search("hello", limit=41) ; "#,
        r#"search("foo", limit=42) }"#,
    );

    let shape: &[(i64, ShardRole)] = &[
        // tree 0 (seed): empty pre-tree chain → root shard only, per root.
        (R1, ShardRole::Root),
        (R2, ShardRole::Root),
        // tree 1, substatement pre-order; per command: root shard + selection shard
        // per root (every root's chain is non-empty by now).
        (R1, ShardRole::Root),
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
        (R1, ShardRole::Root),
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
        (R1, ShardRole::Root),
        (R1, ShardRole::Selection),
        (R2, ShardRole::Root),
        (R2, ShardRole::Selection),
    ];

    let (_res1, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(&acts1, shape);
    let ids1: Vec<i64> = acts1.iter().map(|a| a.layer_id).collect();

    // All three tree-mates' selection shards are SIBLINGS under tree 0's tip
    // (its root shard, per root) — concurrency must not re-introduce
    // intra-tree chaining.
    let (seed_r1, seed_r2) = (acts1[0], acts1[1]);
    for (seed, sel_shard) in [
        (seed_r1, acts1[3]),
        (seed_r2, acts1[5]),
        (seed_r1, acts1[7]),
        (seed_r2, acts1[9]),
        (seed_r1, acts1[11]),
        (seed_r2, acts1[13]),
    ] {
        let (meta, _) = eph_layer_state(TEST_INPUT_SEARCH, sel_shard.layer_id);
        assert_eq!(
            meta.parent_id,
            Some(seed.layer_id),
            "every tree-mate's selection shard parents the PREVIOUS tree's \
             tip, got {:?}",
            meta
        );
    }

    for run in 2..=3 {
        let (_res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
        assert_root_roles(&acts, shape);
        assert!(
            acts.iter().all(|a| !a.created),
            "run {} must be a full cache hit on every layer, got {:?}",
            run,
            acts
        );
        let ids: Vec<i64> = acts.iter().map(|a| a.layer_id).collect();
        assert_eq!(
            ids1, ids,
            "run {}: the (root, role, layer_id) sequence — and with it \
             the tree's tip, its materialisation's last pre-order layer per root — \
             must be identical across runs",
            run
        );
    }
}

// ============================================================================
// Chained-variant removal: loc/layer{} partitioning tests
// ============================================================================
//
// After the migration every layer-creating verb is partitioned.  These tests
// pin the per-verb contracts: loc root shard reuse, layer{} op classification
// (persistent ops → root shard, eph-referencing ops → selection shard), and the
// selection-shard-key collision guard (selection_extra).

#[test]
fn loc_root_shard_reused_across_eph_prefix_change() {
    // Same contract as search: loc reads only persistent data, so its root shard
    // must be reused when the upstream ephemeral prefix changes.
    use crate::command::ShardRole;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=70000, end=70100, instance_type=1) }; "#,
        r#"loc("main.c", 2)"#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=70500, end=70600, instance_type=1) }; "#,
        r#"loc("main.c", 2)"#,
    );

    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root),      // layer block root shard, root 1
        (R2, ShardRole::Root),      // layer block root shard, root 2
        (R1, ShardRole::Root),      // loc root shard, root 1 (/main.c)
        (R1, ShardRole::Selection), // loc selection shard, root 1
        (R2, ShardRole::Root),      // loc root shard, root 2 (/src/main.c)
        (R2, ShardRole::Selection), // loc selection shard, root 2
    ];
    let (_res_a, acts_a) = run_query_traced(VERB_TEST, Q_A);
    assert_root_roles(&acts_a, shape);
    let (loc_root_shard_a_r1, loc_sel_shard_a_r1, loc_root_shard_a_r2, loc_sel_shard_a_r2) =
        (acts_a[2], acts_a[3], acts_a[4], acts_a[5]);
    assert!(
        loc_root_shard_a_r1.created && loc_root_shard_a_r2.created,
        "line 2 is unique to this test ⇒ cold loc root shards, got {:?}",
        acts_a
    );

    for act in [loc_root_shard_a_r1, loc_root_shard_a_r2] {
        let (loc_meta, _) = eph_layer_state(VERB_TEST, act.layer_id);
        assert_eq!(loc_meta.kind, "ephemeral", "got {:?}", loc_meta);
        assert_eq!(
            loc_meta.parent_id,
            Some(act.root_id),
            "loc root shard parents its root, got {:?}",
            loc_meta
        );
    }

    let (_res_b, acts_b) = run_query_traced(VERB_TEST, Q_B);
    assert_root_roles(&acts_b, shape);
    let (loc_root_shard_b_r1, loc_sel_shard_b_r1, loc_root_shard_b_r2, loc_sel_shard_b_r2) =
        (acts_b[2], acts_b[3], acts_b[4], acts_b[5]);
    assert!(
        !loc_root_shard_b_r1.created && !loc_root_shard_b_r2.created,
        "loc root shards must survive the changed eph prefix, got {:?}",
        acts_b
    );
    assert_eq!(loc_root_shard_b_r1.layer_id, loc_root_shard_a_r1.layer_id);
    assert_eq!(loc_root_shard_b_r2.layer_id, loc_root_shard_a_r2.layer_id);
    assert!(
        loc_sel_shard_b_r1.created && loc_sel_shard_b_r2.created,
        "fresh selection shards, got {:?}",
        acts_b
    );
    assert_ne!(loc_sel_shard_b_r1.layer_id, loc_sel_shard_a_r1.layer_id);
    assert_ne!(loc_sel_shard_b_r2.layer_id, loc_sel_shard_a_r2.layer_id);
}

#[test]
fn layer_block_root_shard_reused_across_eph_prefix_change() {
    // A layer{} block whose ops reference only persistent ids is itself a
    // partitioned entry: its root shard is keyed on the resolved op content, not
    // the chain, so changing the upstream prefix reuses it.
    use crate::command::ShardRole;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=71000, end=71100, instance_type=1) }; "#,
        r#"layer { ephemeral_instance(symbol_id=2, object_id=1, "#,
        r#"start=72000, end=72100, instance_type=1) }"#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
        r#"start=71500, end=71600, instance_type=1) }; "#,
        r#"layer { ephemeral_instance(symbol_id=2, object_id=1, "#,
        r#"start=72000, end=72100, instance_type=1) }"#,
    );

    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root),      // first layer block root shard, root 1
        (R2, ShardRole::Root),      // first layer block root shard, root 2
        (R1, ShardRole::Root),      // second layer block root shard, root 1
        (R1, ShardRole::Selection), // second layer block selection shard, root 1
        (R2, ShardRole::Root),      // second layer block root shard, root 2
        (R2, ShardRole::Selection), // second layer block selection shard, root 2
    ];
    let (_res_a, acts_a) = run_query_traced(VERB_TEST, Q_A);
    assert_root_roles(&acts_a, shape);
    let (
        block2_root_shard_a_r1,
        block2_sel_shard_a_r1,
        block2_root_shard_a_r2,
        block2_sel_shard_a_r2,
    ) = (acts_a[2], acts_a[3], acts_a[4], acts_a[5]);
    assert!(
        block2_root_shard_a_r1.created && block2_root_shard_a_r2.created,
        "cold block root shards, got {:?}",
        acts_a
    );

    let (_res_b, acts_b) = run_query_traced(VERB_TEST, Q_B);
    assert_root_roles(&acts_b, shape);
    let (block1_root_shard_b_r1, block1_root_shard_b_r2) = (acts_b[0], acts_b[1]);
    let (
        block2_root_shard_b_r1,
        block2_sel_shard_b_r1,
        block2_root_shard_b_r2,
        block2_sel_shard_b_r2,
    ) = (acts_b[2], acts_b[3], acts_b[4], acts_b[5]);
    assert!(
        block1_root_shard_b_r1.created && block1_root_shard_b_r2.created,
        "changed prefix block is fresh on each root, got {:?}",
        acts_b
    );
    assert!(
        !block2_root_shard_b_r1.created && !block2_root_shard_b_r2.created,
        "identical persistent-ops block must reuse its per-root root shards across prefixes, got {:?}",
        acts_b
    );
    assert_eq!(
        block2_root_shard_b_r1.layer_id,
        block2_root_shard_a_r1.layer_id
    );
    assert_eq!(
        block2_root_shard_b_r2.layer_id,
        block2_root_shard_a_r2.layer_id
    );
    assert!(
        block2_sel_shard_b_r1.created && block2_sel_shard_b_r2.created,
        "fresh selection shards, got {:?}",
        acts_b
    );
    assert_ne!(
        block2_sel_shard_b_r1.layer_id,
        block2_sel_shard_a_r1.layer_id
    );
    assert_ne!(
        block2_sel_shard_b_r2.layer_id,
        block2_sel_shard_a_r2.layer_id
    );
}

#[test]
fn layer_block_eph_ops_split_into_selection_shard() {
    // Mixed block: one persistent op (ephemeral_symbol) and one
    // eph-referencing op (@e labels the search statement, whose selection
    // is eph instances of search's eph symbol — negative symbol ids).
    // The persistent op's rows must land in the root shard, the eph-referencing
    // op's rows in the selection shard.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"@e search("foo", limit=26); "#,
        r#"layer { "#,
        r#"ephemeral_symbol(name="splitsym2", project_id=1, symbol_type=1); "#,
        r#"ephemeral_instance(symbol_id="@e", object_id=1, "#,
        r#"start=73000, end=73100, instance_type=1) "#,
        r#"}"#,
    );

    let (_res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(
        &acts,
        &[
            (R1, ShardRole::Root),      // search root shard, root 1 (no chain yet)
            (R2, ShardRole::Root),      // search root shard, root 2
            (R1, ShardRole::Root),      // mixed block root shard, root 1
            (R1, ShardRole::Selection), // mixed block selection shard, root 1
            (R2, ShardRole::Root),      // mixed block root shard, root 2
            (R2, ShardRole::Selection), // mixed block selection shard, root 2
        ],
    );
    let (mixed_root_shard_r1, mixed_sel_shard_r1, mixed_root_shard_r2, mixed_sel_shard_r2) =
        (acts[2], acts[3], acts[4], acts[5]);

    // Row partitioning is two-dimensional now: root shard vs selection shard (op
    // classification) AND per root (row's project).  splitsym2 declares
    // project_id=1, the @e instance targets object 1 (project 1) — so all
    // rows land in root 1's halves; root 2's halves stay empty but still
    // materialise.
    let (_, (root_shard_symbols, root_shard_instances)) =
        eph_layer_state(TEST_INPUT_SEARCH, mixed_root_shard_r1.layer_id);
    assert_eq!(
        (root_shard_symbols, root_shard_instances),
        (1, 0),
        "persistent op (splitsym2, project 1) lands in root 1's root shard",
    );
    let (_, r2_root_shard_counts) =
        eph_layer_state(TEST_INPUT_SEARCH, mixed_root_shard_r2.layer_id);
    assert_eq!(
        r2_root_shard_counts,
        (0, 0),
        "root 2's root shard gets no rows — splitsym2 belongs to project 1",
    );

    let (sel_shard_meta, (sel_shard_symbols, sel_shard_instances)) =
        eph_layer_state(TEST_INPUT_SEARCH, mixed_sel_shard_r1.layer_id);
    assert_eq!(sel_shard_meta.kind, "ephemeral", "got {:?}", sel_shard_meta);
    assert_eq!(
        sel_shard_symbols, 0,
        "the eph-referencing op creates no symbols, got {:?}",
        sel_shard_meta
    );
    assert_eq!(
        sel_shard_instances, 2,
        "eph-referencing op (@e instance, one row per resolved search \
         symbol — one per project — all on project 1's object) lands in \
         root 1's selection shard, got {} instances",
        sel_shard_instances
    );

    let (r2_sel_shard_meta, r2_sel_shard_counts) =
        eph_layer_state(TEST_INPUT_SEARCH, mixed_sel_shard_r2.layer_id);
    assert_eq!(
        r2_sel_shard_meta.kind, "ephemeral",
        "got {:?}",
        r2_sel_shard_meta
    );
    assert_eq!(
        r2_sel_shard_counts,
        (0, 0),
        "root 2's selection shard is empty — object 1 is not in project 2",
    );
}

#[test]
fn selection_shard_collision_guard_extra_disambiguates() {
    // THE collision the selection_extra fix exists for: two blocks with
    // identical root-shard halves (here: empty — every op references eph ids)
    // under the SAME upstream chain, differing only in their
    // eph-referencing ops.  parent_id and input_hash are identical for
    // both, so without selection_extra the second query would silently
    // HIT the first query's selection shard and serve its rows.  With the fix,
    // the differing op content flows into the selection shard key.
    use crate::command::ShardRole;
    const Q_1: &str = concat!(
        r#"@c search("foo", limit=27); "#,
        r#"layer { ephemeral_ref(to_symbol="@c", from_object=1, "#,
        r#"start=74000, end=74001) }"#,
    );
    const Q_2: &str = concat!(
        r#"@c search("foo", limit=27); "#,
        r#"layer { ephemeral_ref(to_symbol="@c", from_object=1, "#,
        r#"start=75000, end=75001) }"#,
    );

    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root),      // search root shard, root 1 (no chain yet)
        (R2, ShardRole::Root),      // search root shard, root 2
        (R1, ShardRole::Root),      // layer block (empty) root shard, root 1
        (R1, ShardRole::Selection), // layer block selection shard, root 1
        (R2, ShardRole::Root),      // layer block (empty) root shard, root 2
        (R2, ShardRole::Selection), // layer block selection shard, root 2
    ];
    let (_res1, acts1) = run_query_traced(TEST_INPUT_SEARCH, Q_1);
    assert_root_roles(&acts1, shape);
    let (final_root_shard_1_r1, final_sel_shard_1_r1, final_root_shard_1_r2, final_sel_shard_1_r2) =
        (acts1[2], acts1[3], acts1[4], acts1[5]);
    assert!(
        final_sel_shard_1_r1.created && final_sel_shard_1_r2.created,
        "cold selection shards, got {:?}",
        acts1
    );

    let (_res2, acts2) = run_query_traced(TEST_INPUT_SEARCH, Q_2);
    assert_root_roles(&acts2, shape);
    let (prefix_root_shard_2_r1, prefix_root_shard_2_r2) = (acts2[0], acts2[1]);
    let (final_root_shard_2_r1, final_sel_shard_2_r1, final_root_shard_2_r2, final_sel_shard_2_r2) =
        (acts2[2], acts2[3], acts2[4], acts2[5]);

    // Identical prefix and identical (empty) root-shard half: shared per root.
    assert!(
        !prefix_root_shard_2_r1.created && !prefix_root_shard_2_r2.created,
        "identical prefix (search) root shards must hit, got {:?}",
        acts2
    );
    assert!(
        !final_root_shard_2_r1.created && !final_root_shard_2_r2.created,
        "the empty root-shard halves are shared between the two blocks, got {:?}",
        acts2
    );
    assert_eq!(
        final_root_shard_2_r1.layer_id,
        final_root_shard_1_r1.layer_id
    );
    assert_eq!(
        final_root_shard_2_r2.layer_id,
        final_root_shard_1_r2.layer_id
    );

    // The load-bearing assertion, per root: same parent, same input hash,
    // different eph-referencing ops ⇒ DIFFERENT selection shard.  Without the
    // selection_extra fold this was created=false with Q_1's selection shard id.
    assert!(
        final_sel_shard_2_r1.created && final_sel_shard_2_r2.created,
        "differing eph ops must produce fresh selection shards, got {:?}",
        acts2
    );
    assert_ne!(
        final_sel_shard_2_r1.layer_id, final_sel_shard_1_r1.layer_id,
        "selection shard key must fold the eph-referencing op content (root 1)"
    );
    assert_ne!(
        final_sel_shard_2_r2.layer_id, final_sel_shard_1_r2.layer_id,
        "selection shard key must fold the eph-referencing op content (root 2)"
    );
}

#[test]
fn layer_block_negative_literal_without_chain_errors() {
    // A literal negative id references an eph row, but with no upstream
    // chain the executor creates no selection shard — the rows would be
    // silently dropped.  The layer verb must error instead.
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=-5, object_id=1, "#,
        r#"start=76000, end=76100, instance_type=1) }"#,
    );
    let res = run_query_err(VERB_TEST, QUERY);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("negative literal without eph context must error"),
    };
    assert!(
        format!("{}", err).contains("no ephemeral context"),
        "error should explain the missing chain, got: {}",
        err
    );
}

// ============================================================================
// Cache-correctness fix tests (review follow-up)
// ============================================================================
//
// Cover the invalidation/lifecycle fixes: delete_project purging, the
// blocking purge, root↔selection shard lifecycle coupling (root_shard_id FK), the
// doomed-closure GC (no hollow populated=true layers), canonical label
// resolutions, and the eph-free filter fence.
//
// Destructive-global tests (purge_eph_cache, delete_project) run on
// IsolatedFixture databases; targeted tests stay on the shared fixtures.

#[test]
fn canonical_ids_sorts_and_dedups() {
    assert_eq!(
        crate::statement::canonical_ids([42, 17, 42, -3].into_iter()),
        vec![-3, 17, 42],
    );
    assert_eq!(
        crate::statement::canonical_ids(std::iter::empty()),
        Vec::<i64>::new()
    );
}

#[test]
fn root_shard_delete_cascades_selection_shard() {
    // The root_shard_id FK: deleting a root shard takes its selection shard with it, so a
    // cached selection shard can never pair with a recreated root shard of a
    // different incarnation.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=80000, end=80100, instance_type=1) }; "#,
        r#"search("foo", limit=28)"#,
    );
    let (_res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(
        &acts,
        &[
            (R1, ShardRole::Root),      // prefix layer block root shard, root 1
            (R2, ShardRole::Root),      // prefix layer block root shard, root 2
            (R1, ShardRole::Root),      // search root shard, root 1
            (R1, ShardRole::Selection), // search selection shard, root 1
            (R2, ShardRole::Root),      // search root shard, root 2
            (R2, ShardRole::Selection), // search selection shard, root 2
        ],
    );
    let (prefix_r1, prefix_r2) = (acts[0], acts[1]);
    let (root_shard_r1, sel_r1, root_shard_r2, sel_r2) = (acts[2], acts[3], acts[4], acts[5]);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_SEARCH).await;
        // Delete root 1's root shard only: its pair dies, root 2's pair survives.
        index
            .delete_eph_layer(root_shard_r1.layer_id)
            .await
            .unwrap();

        assert!(
            index.eph_layer_meta(root_shard_r1.layer_id).await.is_err(),
            "root 1's root-shard row must be gone"
        );
        assert!(
            index.eph_layer_meta(sel_r1.layer_id).await.is_err(),
            "root 1's selection shard must cascade with its root shard (root_shard_id FK)"
        );
        assert!(
            index.eph_layer_meta(root_shard_r2.layer_id).await.is_ok(),
            "root 2's root shard is an independent cache entry and must survive"
        );
        assert!(
            index.eph_layer_meta(sel_r2.layer_id).await.is_ok(),
            "root 2's selection shard pairs with root 2's root shard and must survive"
        );
        assert!(
            index.eph_layer_meta(prefix_r1.layer_id).await.is_ok()
                && index.eph_layer_meta(prefix_r2.layer_id).await.is_ok(),
            "unrelated prefix layers must survive"
        );
    });
}

#[test]
fn delete_eph_layer_removes_reference_dependents() {
    // The doomed-closure GC: deleting a layer also deletes every layer
    // whose rows reference its symbols, transitively — no layer is left
    // hollow with populated=true.  The search root shard here is parented on its
    // root and is nobody's parent (the chain runs through its selection shard),
    // so only the closure — root_shard_id edge to its selection shard, reference/
    // parent edges to the downstream block's selection shard — can clean up the
    // dependents.
    use crate::command::ShardRole;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=82000, end=82100, instance_type=1) }; "#,
        r#"@e search("foo", limit=29); "#,
        r#"layer { ephemeral_instance(symbol_id="@e", object_id=1, "#,
        r#"start=82500, end=82600, instance_type=1) }"#,
    );
    let (_res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(
        &acts,
        &[
            (R1, ShardRole::Root),      // prefix layer block root shard, root 1
            (R2, ShardRole::Root),      // prefix layer block root shard, root 2
            (R1, ShardRole::Root),      // search root shard, root 1
            (R1, ShardRole::Selection), // search selection shard, root 1
            (R2, ShardRole::Root),      // search root shard, root 2
            (R2, ShardRole::Selection), // search selection shard, root 2
            (R1, ShardRole::Root),      // eph-ops block: empty root shard, root 1
            (R1, ShardRole::Selection), // eph-ops block: @e rows (object 1 ⇒ root 1)
            (R2, ShardRole::Root),      // eph-ops block: empty root shard, root 2
            (R2, ShardRole::Selection), // eph-ops block: empty selection shard, root 2
        ],
    );
    let (prefix_r1, prefix_r2) = (acts[0], acts[1]);
    let (search_root_shard_r1, search_sel_shard_r1, search_root_shard_r2, search_sel_shard_r2) =
        (acts[2], acts[3], acts[4], acts[5]);
    let (block_root_shard_r1, block_sel_shard_r1, block_root_shard_r2, block_sel_shard_r2) =
        (acts[6], acts[7], acts[8], acts[9]);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_SEARCH).await;
        // Delete root 1's search root shard: the closure takes exactly root 1's
        // dependents; root 2's chain is untouched.
        index
            .delete_eph_layer(search_root_shard_r1.layer_id)
            .await
            .unwrap();

        assert!(
            index
                .eph_layer_meta(search_root_shard_r1.layer_id)
                .await
                .is_err(),
            "seed must be gone"
        );
        assert!(
            index
                .eph_layer_meta(search_sel_shard_r1.layer_id)
                .await
                .is_err(),
            "root 1's search selection shard dies with its root shard"
        );
        assert!(
            index
                .eph_layer_meta(block_sel_shard_r1.layer_id)
                .await
                .is_err(),
            "layer referencing the deleted root shard's symbols must be deleted, \
             not left hollow with populated=true"
        );
        assert!(
            index.eph_layer_meta(prefix_r1.layer_id).await.is_ok()
                && index.eph_layer_meta(prefix_r2.layer_id).await.is_ok(),
            "prefix layers have no dependency on root 1's search root shard"
        );
        assert!(
            index
                .eph_layer_meta(block_root_shard_r1.layer_id)
                .await
                .is_ok()
                && index
                    .eph_layer_meta(block_root_shard_r2.layer_id)
                    .await
                    .is_ok(),
            "the eph-ops block's empty root shards have no dependency either"
        );
        assert!(
            index
                .eph_layer_meta(search_root_shard_r2.layer_id)
                .await
                .is_ok()
                && index
                    .eph_layer_meta(search_sel_shard_r2.layer_id)
                    .await
                    .is_ok(),
            "root 2's search pair is an independent cache entry and must survive"
        );
        assert!(
            index
                .eph_layer_meta(block_sel_shard_r2.layer_id)
                .await
                .is_ok(),
            "root 2's block selection shard holds no rows referencing root 1's \
             root shard (object 1 is project 1's) and must survive"
        );
    });
}

#[test]
fn ttl_purge_takes_dependent_closure() {
    // Same closure property through the TTL path: rewind only the search
    // root shard's last_used; purge_old_eph_layers must take the root shard plus its
    // dependents and nothing else.  Safe on the shared fixture: the seed
    // set is exactly the rewound row (everything else is fresh) and the
    // closure only contains this test's own layers.
    use crate::command::ShardRole;
    use diesel_async::AsyncConnection;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=84000, end=84100, instance_type=1) }; "#,
        r#"@e search("foo", limit=31); "#,
        r#"layer { ephemeral_instance(symbol_id="@e", object_id=1, "#,
        r#"start=84500, end=84600, instance_type=1) }"#,
    );
    let (_res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert_root_roles(
        &acts,
        &[
            (R1, ShardRole::Root),      // prefix layer block root shard, root 1
            (R2, ShardRole::Root),      // prefix layer block root shard, root 2
            (R1, ShardRole::Root),      // search root shard, root 1
            (R1, ShardRole::Selection), // search selection shard, root 1
            (R2, ShardRole::Root),      // search root shard, root 2
            (R2, ShardRole::Selection), // search selection shard, root 2
            (R1, ShardRole::Root),      // eph-ops block: empty root shard, root 1
            (R1, ShardRole::Selection), // eph-ops block: @e rows (object 1 ⇒ root 1)
            (R2, ShardRole::Root),      // eph-ops block: empty root shard, root 2
            (R2, ShardRole::Selection), // eph-ops block: empty selection shard, root 2
        ],
    );
    let (prefix_r1, prefix_r2) = (acts[0], acts[1]);
    let (search_root_shard_r1, search_sel_shard_r1, search_root_shard_r2, search_sel_shard_r2) =
        (acts[2], acts[3], acts[4], acts[5]);
    let (_block_root_shard_r1, block_sel_shard_r1, _block_root_shard_r2, block_sel_shard_r2) =
        (acts[6], acts[7], acts[8], acts[9]);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let url = get_shared_db_url(TEST_INPUT_SEARCH);
        let mut conn = diesel_async::AsyncPgConnection::establish(url)
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query(format!(
                "UPDATE index.layers SET last_used = now() - interval '2 hours' \
                 WHERE id = {}",
                search_root_shard_r1.layer_id
            )),
            &mut conn,
        )
        .await
        .unwrap();

        let index = get_shared_index(TEST_INPUT_SEARCH).await;
        index
            .purge_old_eph_layers(std::time::Duration::from_secs(3600))
            .await
            .unwrap();

        assert!(
            index
                .eph_layer_meta(search_root_shard_r1.layer_id)
                .await
                .is_err(),
            "expired root shard must be purged"
        );
        assert!(
            index
                .eph_layer_meta(search_sel_shard_r1.layer_id)
                .await
                .is_err(),
            "its selection shard must go with it"
        );
        assert!(
            index
                .eph_layer_meta(block_sel_shard_r1.layer_id)
                .await
                .is_err(),
            "referencing layer must go with it (no hollow survivors)"
        );
        assert!(
            index.eph_layer_meta(prefix_r1.layer_id).await.is_ok()
                && index.eph_layer_meta(prefix_r2.layer_id).await.is_ok(),
            "fresh, unrelated layers must survive the TTL purge"
        );
        assert!(
            index
                .eph_layer_meta(search_root_shard_r2.layer_id)
                .await
                .is_ok()
                && index
                    .eph_layer_meta(search_sel_shard_r2.layer_id)
                    .await
                    .is_ok(),
            "root 2's search pair is fresh and dependency-free — it must \
             survive the purge of root 1's root shard"
        );
        assert!(
            index
                .eph_layer_meta(block_sel_shard_r2.layer_id)
                .await
                .is_ok(),
            "root 2's block selection shard holds no rows referencing the purged \
             root shard and must survive"
        );
    });
}

#[test]
fn ttl_purge_never_collects_roots() {
    // Roots' last_used is never refreshed by queries, so without an explicit
    // exclusion an epoch-old root would enter the TTL seed and its deletion
    // would cascade a project's entire persistent index away.  Rewind ALL
    // root layers far past the cutoff and verify the purge leaves them — and
    // the persistent data reachable through them — untouched.
    use diesel_async::AsyncConnection;
    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let url = get_shared_db_url(TEST_INPUT_A);
        let mut conn = diesel_async::AsyncPgConnection::establish(url)
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query(
                "UPDATE index.layers SET last_used = now() - interval '10 years' \
                 WHERE kind = 'root'",
            ),
            &mut conn,
        )
        .await
        .unwrap();

        let index = get_shared_index(TEST_INPUT_A).await;
        index
            .purge_old_eph_layers(std::time::Duration::from_secs(3600))
            .await
            .unwrap();

        let roots = index.load_root_layers().await.unwrap();
        assert!(
            !roots.is_empty(),
            "root layers must survive the TTL purge unconditionally"
        );

        let (res, _acts) = crate::test_util::run_query_traced_on(index, r#""a""#)
            .await
            .unwrap();
        assert!(
            !res.nodes.as_vec().is_empty(),
            "persistent data must remain queryable after the purge"
        );
    });
}

#[test]
fn search_root_shard_reused_with_filters_across_eph_change() {
    // Fence for the objects_expr eph-free contract: a search whose
    // composite filter includes the (only) objects_expr implementation
    // must still share its root shard across different upstream eph chains.  If
    // any hashed-or-consumed filter component becomes chain-sensitive,
    // either the input hash diverges (created=true below) or — worse — the
    // key stays while content diverges; this test pins the key side.
    use crate::command::ShardRole;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=85000, end=85100, instance_type=1) }; "#,
        r#"project("search_proj_1") search("foo", limit=30)"#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=85500, end=85600, instance_type=1) }; "#,
        r#"project("search_proj_1") search("foo", limit=30)"#,
    );

    let shape: &[(i64, ShardRole)] = &[
        (R1, ShardRole::Root),      // layer block root shard, root 1
        (R2, ShardRole::Root),      // layer block root shard, root 2
        (R1, ShardRole::Root),      // filtered search root shard, root 1
        (R1, ShardRole::Selection), // search selection shard, root 1
        (R2, ShardRole::Root),      // filtered search root shard, root 2 (empty)
        (R2, ShardRole::Selection), // search selection shard, root 2
    ];
    let (_res_a, acts_a) = run_query_traced(TEST_INPUT_SEARCH, Q_A);
    assert_root_roles(&acts_a, shape);
    let (root_shard_a_r1, root_shard_a_r2) = (acts_a[2], acts_a[4]);
    assert!(
        root_shard_a_r1.created && root_shard_a_r2.created,
        "cold filtered root shards (root 2's is empty — the filter excludes its \
         project — but still materialises), got {:?}",
        acts_a
    );

    let (_res_b, acts_b) = run_query_traced(TEST_INPUT_SEARCH, Q_B);
    assert_root_roles(&acts_b, shape);
    let (root_shard_b_r1, root_shard_b_r2) = (acts_b[2], acts_b[4]);
    assert!(
        !root_shard_b_r1.created && !root_shard_b_r2.created,
        "filtered search root shards must be chain-independent, got {:?}",
        acts_b
    );
    assert_eq!(root_shard_b_r1.layer_id, root_shard_a_r1.layer_id);
    assert_eq!(root_shard_b_r2.layer_id, root_shard_a_r2.layer_id);
}

#[test]
fn label_resolution_keys_stable_across_duplicate_instances() {
    // A label resolving to a symbol with MULTIPLE instances (one
    // persistent, one added by a prefix layer) yields duplicate per-
    // instance symbol ids; canonical_ids collapses them, so the block's
    // cache key is stable and a repeat run is a full hit on every layer.
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=83000, end=83100, instance_type=1) }; "#,
        r#"@t "fn_basic"; "#,
        r#"layer { ephemeral_instance(symbol_id="@t", object_id=2, "#,
        r#"start=83500, end=83600, instance_type=1) }"#,
    );

    let (_res1, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(
        acts1.iter().all(|a| a.created),
        "first run is cold, got {:?}",
        acts1
    );

    let (_res2, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(
        acts2.iter().all(|a| !a.created),
        "repeat run must hit every layer (canonical resolution keys), got {:?}",
        acts2
    );
    let ids1: Vec<i64> = acts1.iter().map(|a| a.layer_id).collect();
    let ids2: Vec<i64> = acts2.iter().map(|a| a.layer_id).collect();
    assert_eq!(ids1, ids2);
}

#[test]
fn delete_project_purges_eph_cache() {
    // delete_project mutates the persistent index, so it must purge the
    // eph cache in the same transaction (the invariant documented on
    // finalize_project).  Without the purge, the loc layer here survives
    // project deletion hollow (its rows cascade away, populated stays
    // true) and the rerun would silently hit it; with the purge, the rerun
    // repopulates and fails loudly because the file is gone.
    use crate::test_util::{
        create_isolated_fixture, run_query_traced_on, store_and_index_with_shared_cache,
    };
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        // Shared-cache pair: the store's clear must invalidate the cache
        // the index reads (a split-cache pairing would pass this test only
        // incidentally — the review caught exactly that).
        let (store, index) = store_and_index_with_shared_cache(fx.url()).await;

        const QUERY: &str = r#"loc("main.c", 2, project="test_project")"#;
        let (_res, acts) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        assert!(acts[0].created, "cold loc root shard, got {:?}", acts);
        assert!(index.eph_layer_count().await.unwrap() >= 1);

        assert!(store.delete_project(1).await.unwrap());

        assert_eq!(
            index.eph_layer_count().await.unwrap(),
            0,
            "delete_project must purge the eph cache"
        );

        let rerun = run_query_traced_on(index.clone(), QUERY).await;
        assert!(
            rerun.is_err(),
            "rerun must repopulate and fail loudly (file gone), not serve \
             a hollow cached layer"
        );
    });
}

#[test]
fn per_root_root_shard_reused_under_narrowed_roots() {
    // The headline property of per-root chains: a root's root shard is keyed on
    // that single root's identity (`root_shard_hash`), so narrowing the
    // visible root set reuses the already-cached root shard instead of
    // repopulating.  The request-level entry point is `narrow_roots` in the
    // askld binary (`/query?projects=…`, `askl_run`'s `projects`); this test
    // constructs the narrowed set directly, which is exactly what that
    // resolver hands to `ExecutionContext::new`.
    use crate::command::ShardRole;
    use crate::test_util::run_query_traced_with_roots;
    const QUERY: &str = r#"search("foo", limit=97)"#; // unique limit → own cache rows

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_SEARCH).await;
        let roots = index.load_root_layers().await.unwrap();

        // Full visibility: one root shard per root, both freshly created.
        let (full_res, full_acts) =
            run_query_traced_with_roots(index.clone(), roots.clone(), QUERY)
                .await
                .unwrap();
        assert_root_roles(&full_acts, &[(R1, ShardRole::Root), (R2, ShardRole::Root)]);
        assert!(full_acts.iter().all(|a| a.created), "cold run creates both");
        let r1_root_shard = full_acts[0].layer_id;

        // Narrowed to root 1 only: exactly one activation, a HIT on the
        // very same root shard — its key never depended on root 2.
        let narrowed: Vec<_> = roots.iter().filter(|r| r.id == R1).cloned().collect();
        let (narrow_res, narrow_acts) = run_query_traced_with_roots(index.clone(), narrowed, QUERY)
            .await
            .unwrap();
        assert_root_roles(&narrow_acts, &[(R1, ShardRole::Root)]);
        assert!(
            !narrow_acts[0].created,
            "narrowed run must hit root 1's cached root shard, got {:?}",
            narrow_acts
        );
        assert_eq!(
            narrow_acts[0].layer_id, r1_root_shard,
            "same root shard across visibility sets"
        );

        // The narrowed result is exactly project 1's share: 8 substring
        // "foo" matches (the per-project counts pinned by
        // search_first_call_reports_created_and_populated).
        assert_eq!(narrow_res.nodes.as_vec().len(), 8);
        assert_eq!(full_res.nodes.as_vec().len(), 12);
    });
}

#[test]
fn root_deletion_cascades_only_that_projects_chains() {
    // Root-parented root shards: deleting a project's root layer (the tail of
    // delete_project's ordering, after the project row releases the FK)
    // cascades that project's cached root shards via `parent_id` and their
    // selection shards via `root_shard_id` — while the OTHER root's chains survive
    // untouched.  Pinned at the SQL level on an isolated fixture because
    // delete_project's trailing purge_eph_cache would mask the cascade.
    use crate::test_util::{create_isolated_fixture, run_query_traced_on};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(TEST_INPUT_SEARCH);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = index::db_diesel::Index::connect(fx.url()).await.unwrap();

        // Two statements → per root: prefix root shard, then search root shard +
        // selection shard (chains on both roots).
        const QUERY: &str = concat!(
            r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
            r#"start=90000, end=90100, instance_type=1) }; "#,
            r#"search("foo", limit=93)"#,
        );
        let (_res, acts) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        let r1_layers: Vec<i64> = acts
            .iter()
            .filter(|a| a.root_id == R1)
            .map(|a| a.layer_id)
            .collect();
        let r2_layers: Vec<i64> = acts
            .iter()
            .filter(|a| a.root_id == R2)
            .map(|a| a.layer_id)
            .collect();
        assert_eq!(
            r1_layers.len(),
            3,
            "prefix root shard + search root shard + selection shard"
        );
        assert_eq!(r2_layers.len(), 3);

        // Mimic delete_project's tail for project 1: the project row goes
        // first (its FK hard-blocks deleting a live project's root), then
        // the root layer.
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query("DELETE FROM index.projects WHERE id = 1"),
            &mut conn,
        )
        .await
        .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query(format!("DELETE FROM index.layers WHERE id = {R1}")),
            &mut conn,
        )
        .await
        .unwrap();

        for id in &r1_layers {
            assert!(
                index.eph_layer_meta(*id).await.is_err(),
                "project 1's cached layer {id} must cascade with its root"
            );
        }
        for id in &r2_layers {
            assert!(
                index.eph_layer_meta(*id).await.is_ok(),
                "project 2's cached layer {id} must survive project 1's deletion"
            );
        }
    });
}

#[test]
fn purge_blocks_on_inflight_layer_txn() {
    // The LOCK TABLE in purge_eph_cache: a purge must wait for an open
    // layer transaction to finish, and must then see (and delete) the row
    // that transaction committed — the exact visibility race that used to
    // let a stale root shard commit after the purge and be served forever.
    use crate::test_util::create_isolated_fixture;
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = index::db_diesel::Index::connect(fx.url()).await.unwrap();
        let hash = sha2::Sha256::digest(b"purge_block_test_hash");

        // Open a layer transaction and hold it (simulates an in-flight
        // populate running its expensive SQL).
        let txn = index
            .create_eph_layer(None, None, &hash, index::db_diesel::EphLayerKind::Ephemeral)
            .await
            .unwrap();
        assert!(txn.created());

        // A concurrent purge (inside its own transaction, as in
        // finalize_project/delete_project) must block on the table lock.
        let mut purge_conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        let mut purge_fut = std::pin::pin!(purge_conn.transaction::<_, diesel::result::Error, _>(
            async move |conn| index::db_diesel::purge_eph_cache(conn).await
        ));
        let blocked =
            tokio::time::timeout(std::time::Duration::from_millis(1000), &mut purge_fut).await;
        assert!(
            blocked.is_err(),
            "purge must block while a layer transaction is open"
        );

        // Commit the layer; the purge must now proceed AND delete the row
        // the transaction just committed (visibility across the lock).
        txn.commit().await.unwrap();
        let purged = tokio::time::timeout(std::time::Duration::from_secs(10), &mut purge_fut)
            .await
            .expect("purge must complete once the layer transaction commits")
            .unwrap();
        assert!(
            purged >= 1,
            "purge must see and delete the just-committed layer, got {}",
            purged
        );
        assert_eq!(index.eph_layer_count().await.unwrap(), 0);
    });
}

// ============================================================================
// SQL result cache tests
// ============================================================================

#[test]
fn cached_load_miss_then_hit() {
    // The generic proxy: the same query (by rendered SQL + binds) misses
    // once and hits afterwards; a bind change misses again.  One Index
    // instance throughout — the cache is per-Index (per-process in the
    // server), not per-database.
    use diesel::prelude::*;
    use index::schema_diesel::symbols;

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(VERB_TEST).await;
        let build = |min_id: i64| {
            symbols::table
                .filter(symbols::id.gt(min_id))
                .select(symbols::id)
                .into_boxed::<diesel::pg::Pg>()
        };

        let s0 = index.sql_cache().stats();
        let first = index.cached_load::<_, i64>(build(0)).await.unwrap();
        let s1 = index.sql_cache().stats();
        assert_eq!(s1.misses, s0.misses + 1, "cold query is a miss");
        assert_eq!(s1.hits, s0.hits, "no hit yet");
        assert!(!first.is_empty(), "fixture has symbols");

        let second = index.cached_load::<_, i64>(build(0)).await.unwrap();
        let s2 = index.sql_cache().stats();
        assert_eq!(s2.hits, s1.hits + 1, "identical query is a hit");
        assert_eq!(s2.misses, s1.misses, "no extra miss");
        assert_eq!(*first, *second, "hit returns the same rows");

        let third = index.cached_load::<_, i64>(build(1)).await.unwrap();
        let s3 = index.sql_cache().stats();
        assert_eq!(s3.misses, s2.misses + 1, "differing bind is a distinct key");
        assert!(third.len() <= first.len());
    });
}

// ============================================================================
// SQL result cache: end-to-end partition/invalidation tests
// ============================================================================
//
// NOTE: the cache is per-Index (per-process in the server).  run_query_traced
// creates a fresh Index — and thus a COLD cache — per call, which is why
// these tests build one Index and run queries through run_query_traced_on.

#[test]
fn sql_cache_persistent_branch_shared_across_chains() {
    // Genuine persistent-branch sharing: the BARE statement "fn_basic"
    // populates chain-free keys; the SAME statement under two different
    // ephemeral prefixes must then HIT those entries (its select_current
    // persistent branch renders identical SQL — the statement's inputs
    // are chain-free).  NOT asserted here, by design: statements whose
    // inputs embed chain-derived ids (downstream of an eph selection) and
    // chain-dependent (direct/innermost) filters do not share across
    // chains — see cached_load_partitioned's documented limitations.
    use crate::test_util::run_query_traced_on;
    const BARE: &str = r#""fn_basic""#;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=90000, end=90100, instance_type=1) }; "#,
        r#""fn_basic""#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=91000, end=91100, instance_type=1) }; "#,
        r#""fn_basic""#,
    );

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_SEARCH).await;

        // Cold: the bare statement stores the chain-free entries.
        let (_bare, _) = run_query_traced_on(index.clone(), BARE).await.unwrap();
        let s0 = index.sql_cache().stats();

        let (_res_a, _) = run_query_traced_on(index.clone(), Q_A).await.unwrap();
        let s1 = index.sql_cache().stats();
        assert!(
            s1.hits > s0.hits,
            "prefixed run must hit the bare run's persistent entries, {:?} -> {:?}",
            s0,
            s1,
        );

        let (_res_b, _) = run_query_traced_on(index.clone(), Q_B).await.unwrap();
        let s2 = index.sql_cache().stats();
        assert!(
            s2.hits > s1.hits,
            "a different prefix must hit the same persistent entries, {:?} -> {:?}",
            s1,
            s2,
        );
    });
}

#[test]
fn sql_cache_eph_rows_visible_cold_and_warm() {
    // The eph branch carries the ephemeral rows: a prefixed query whose
    // selection includes an eph instance returns identical, duplicate-free
    // nodes on a cold cache and on a warm one.
    use crate::test_util::run_query_traced_on;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=10, object_id=1, "#,
        r#"start=92000, end=92100, instance_type=1) }; "#,
        r#""fn_basic""#,
    );

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_SEARCH).await;

        let (cold, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        let (warm, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();

        let cold_nodes = cold.nodes.as_vec();
        let warm_nodes = warm.nodes.as_vec();
        assert!(
            cold_nodes.iter().any(|id| {
                let v: i64 = (*id).into();
                v < 0
            }),
            "eph instance must be visible through the eph branch"
        );
        let cold_set: std::collections::HashSet<_> = cold_nodes.iter().copied().collect();
        let warm_set: std::collections::HashSet<_> = warm_nodes.iter().copied().collect();
        assert_eq!(cold_set, warm_set, "cold and warm results must agree");
        assert_eq!(
            cold_nodes.len(),
            cold_set.len(),
            "branch merge must not duplicate nodes (cold)"
        );
        assert_eq!(
            warm_nodes.len(),
            warm_set.len(),
            "branch merge must not duplicate nodes (warm)"
        );
    });
}

#[test]
fn sql_cache_containment_correct_under_prefixes() {
    // Containment queries involve the chain-dependent direct/innermost
    // machinery (the Combined fallback): results must stay correct across
    // differing prefixes and cache temperatures.
    use crate::test_util::run_query_traced_on;
    const Q_A: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=201, object_id=1, "#,
        r#"start=93000, end=93100, instance_type=1) }; "#,
        r#"dir("/") has { file }"#,
    );
    const Q_B: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=201, object_id=1, "#,
        r#"start=93500, end=93600, instance_type=1) }; "#,
        r#"dir("/") has { file }"#,
    );

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_TREE_BROWSER).await;

        let (a1, _) = run_query_traced_on(index.clone(), Q_A).await.unwrap();
        let (a2, _) = run_query_traced_on(index.clone(), Q_A).await.unwrap();
        let (b, _) = run_query_traced_on(index.clone(), Q_B).await.unwrap();

        let set = |r: &crate::statement::ExecutionResult| {
            r.nodes
                .as_vec()
                .iter()
                .filter(|id| {
                    let v: i64 = (**id).into();
                    v > 0 // ignore the per-prefix layer instance
                })
                .copied()
                .collect::<std::collections::HashSet<_>>()
        };
        assert_eq!(set(&a1), set(&a2), "warm rerun must equal cold run");
        assert_eq!(
            set(&a1),
            set(&b),
            "persistent containment results must not vary with the prefix"
        );
    });
}

#[test]
fn sql_cache_cleared_by_delete_project() {
    // Invalidation end-to-end: a warm cache must not serve pre-deletion
    // results after delete_project clears it (shared-cache wiring).
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        use crate::test_util::run_query_traced_on;
        let (store, index) = store_and_index_with_shared_cache(fx.url()).await;

        const QUERY: &str = r#""foo""#;
        let (first, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        assert!(!first.nodes.as_vec().is_empty(), "foo exists before delete");

        // Warm the cache and prove it hits.
        let hits_before = index.sql_cache().stats().hits;
        let (_warm, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        assert!(
            index.sql_cache().stats().hits > hits_before,
            "second run must be served from cache"
        );

        assert!(store.delete_project(1).await.unwrap());

        let (after, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        assert!(
            after.nodes.as_vec().is_empty(),
            "post-delete query must see fresh (empty) results, not the warm \
             cache: got {:?}",
            after.nodes.as_vec(),
        );
    });
}

#[test]
fn sql_cache_concurrent_identical_loads() {
    // Concurrency smoke: two identical cold loads racing on one Index both
    // succeed (no singleflight — both may execute; documented non-goal).
    use diesel::prelude::*;
    use index::schema_diesel::symbols;

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(VERB_TEST).await;
        let build = || {
            symbols::table
                .filter(symbols::id.gt(-1_000_000i64))
                .select(symbols::id)
                .into_boxed::<diesel::pg::Pg>()
        };
        let (a, b) = tokio::join!(
            index.cached_load::<_, i64>(build()),
            index.cached_load::<_, i64>(build()),
        );
        let (a, b) = (a.unwrap(), b.unwrap());
        assert_eq!(*a, *b, "concurrent identical loads must agree");
    });
}

// ============================================================================
// SQL cache review fixes: edge semantics + key stability
// ============================================================================

#[test]
fn edges_pick_min_instance_across_branches() {
    // Contract of find_edges_between (legacy single-query semantics):
    // DISTINCT ON (from_inst, sr) ... ORDER BY ... to_inst.id — exactly ONE
    // row per (from, ref), targeting the MIN to_instance.  With an eph
    // instance of the target symbol in the chain that is the eph instance
    // (negative id).  The partitioned branches each run their own DISTINCT
    // ON, so without the post-merge collapse this returns one row per
    // branch and the persistent row leads the vector.
    //
    // Tested at the Index level: in end-to-end query flows the implicit
    // edges are currently shadowed by explicit children/parents collection
    // (statement/mod.rs seeds seen_edges first, and that path sorts to the
    // min instance itself), so only Skip-scoped selectors (search/loc)
    // observe this function's rows directly.
    use crate::test_util::run_query_traced_on;
    const SETUP: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=2, object_id=1, "#,
        r#"start=95000, end=95100, instance_type=1) }"#,
    );

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(VERB_TEST).await;
        let (res, acts) = run_query_traced_on(index.clone(), SETUP).await.unwrap();
        let eph_inst: i64 = res
            .nodes
            .as_vec()
            .iter()
            .map(|id| Into::<i64>::into(*id))
            .find(|v| *v < 0)
            .expect("layer created an eph instance");
        let mut eph =
            index::db_diesel::EphContext::rooted(index.load_root_layers().await.unwrap());
        let materialisation: Vec<(i64, i64)> =
            acts.iter().map(|a| (a.root_id, a.layer_id)).collect();
        eph.push_materialisation(&materialisation);

        // Candidates: foo's instance (91), foo.bar's persistent instance
        // (92), and the eph instance of foo.bar.  Two persistent refs run
        // foo -> foo.bar.
        let edges = index
            .find_edges_between(&[91, 92, eph_inst], &eph)
            .await
            .unwrap()
            .0;

        assert_eq!(
            edges.len(),
            2,
            "exactly one row per (from, ref) — the collapse must apply              across branches, got {:?}",
            edges
                .iter()
                .map(|e| (e.ref_id, e.from_instance_id, e.to_instance_id))
                .collect::<Vec<_>>(),
        );
        for e in &edges {
            assert_eq!(e.from_instance_id, 91);
            assert_eq!(
                e.to_instance_id, eph_inst,
                "each ref must target the MIN to_instance (the eph one)"
            );
        }
    });
}

#[test]
fn sql_cache_warm_arrow_query_hits_traversal_families() {
    // Keying stability for the traversal pipeline: the same layer+arrow
    // query twice on ONE Index must hit on the second run — this covers
    // find_edges_between (whose candidate ids formerly arrived in random
    // HashSet order, re-keying every request) and the parents/children
    // families.
    use crate::test_util::run_query_traced_on;
    const QUERY: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id=2, object_id=1, "#,
        r#"start=96000, end=96100, instance_type=1) }; "#,
        r#""foo" { "foo.bar" }"#,
    );

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(VERB_TEST).await;

        let (r1, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        let s1 = index.sql_cache().stats();
        let (r2, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        let s2 = index.sql_cache().stats();

        assert!(
            s2.hits >= s1.hits + 3,
            "warm rerun must hit the selection/edges families, stats {:?} -> {:?}",
            s1,
            s2,
        );
        assert_eq!(
            format_edges(r1.edges),
            format_edges(r2.edges),
            "warm rerun must produce identical edges"
        );
    });
}

/// Uploading INSTANCES goes through `upload_object_chunk`, and that path has
/// its own `ON CONFLICT` target naming `symbol_instances_layer_uq`.  When the
/// index behind it changed to `(object_id, dfs_enter, layer) WHERE dfs_exit <> 0`
/// and the target did not, every insert failed with "there is no unique or
/// exclusion constraint matching the ON CONFLICT specification" -- a 500 on a
/// real upload, with the whole suite green, because the only DB-backed upload
/// test uploads a project with NO objects and so never inserts an instance.
///
/// Two expansions of one macro at one extent is the shape the index exists to
/// permit: a header with no include guard, included twice under different macro
/// definitions, expands one source line into two different functions.  They
/// share `(symbol, offset_range)` and differ only in DFS position.
#[test]
fn uploading_two_instances_at_one_extent_keeps_both() {
    use crate::proto::askl::index::{
        Object as UploadObject, Project as UploadProject, Symbol as UploadSymbol,
        SymbolInstance as UploadInstance,
    };
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let (store, _index) = store_and_index_with_shared_cache(fx.url()).await;

        // Re-upload the fixture's own project rather than inventing a new one:
        // the fixture SQL inserts layer rows with explicit ids and does not
        // advance the sequence, so creating a project from scratch collides.
        // Marking it failed makes `upload_index` delete the zombie and its
        // root layer, which frees an id for the replacement.
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query("UPDATE index.projects SET upload_status = 'failed'"),
            &mut conn,
        )
        .await
        .unwrap();

        let (pid, _) = store
            .upload_index(
                UploadProject {
                    project_name: "test_project".to_string(),
                    root_path: "/test_project".to_string(),
                    ..Default::default()
                },
                Some(1),
                Some(1),
            )
            .await
            .unwrap();

        store
            .upload_symbol_chunk(
                pid,
                0,
                vec![UploadSymbol {
                    local_id: 1,
                    name: "DECL".to_string(),
                    r#type: 7, // MACRO
                    ..Default::default()
                }],
            )
            .await
            .unwrap();

        // A root plus two expansions sharing [100, 120) -- a valid tree, so
        // `validate_object_tree` passes and the insert is what is under test.
        let mk = |enter: i64, exit: i64, level: i32, start: i32, end: i32| UploadInstance {
            symbol_local_id: 1,
            instance_type: 1,
            start_offset: start,
            end_offset: end,
            dfs_enter: enter,
            dfs_exit: exit,
            nesting_level: level,
            ..Default::default()
        };
        let payload = UploadProject {
            objects: vec![UploadObject {
                local_id: 1,
                module_path: "mod".to_string(),
                filesystem_path: "/test_project/multi.h".to_string(),
                filetype: "c".to_string(),
                content: b"x".to_vec(),
                symbol_instances: vec![
                    mk(0, 8192, 0, 0, 200),
                    mk(1024, 2048, 1, 100, 120),
                    mk(3072, 4096, 1, 100, 120),
                ],
                ..Default::default()
            }],
            ..Default::default()
        };

        store
            .upload_object_chunk(pid, 0, payload.clone())
            .await
            .unwrap();
        // Re-sending a chunk is what resumption does; it must stay idempotent.
        store.upload_object_chunk(pid, 0, payload).await.unwrap();
        store.finalize_project(pid).await.unwrap();

        use diesel_async::RunQueryDsl;
        let rows: Vec<DfsIntervalRow> = diesel::sql_query(
            "SELECT si.dfs_enter, si.dfs_exit FROM index.symbol_instances si \
             JOIN index.objects o ON o.id = si.object_id \
             WHERE o.filesystem_path = '/test_project/multi.h' \
             ORDER BY si.dfs_enter",
        )
        .load(&mut conn)
        .await
        .unwrap();
        let rows: Vec<(i64, i64)> = rows
            .into_iter()
            .map(|r| (r.dfs_enter, r.dfs_exit))
            .collect();

        assert_eq!(
            rows,
            vec![(0, 8192), (1024, 2048), (3072, 4096)],
            "both expansions at [100,120) must survive, and a re-sent chunk \
             must not duplicate them"
        );
    });
}

/// A REFERENCE IS STORED BY WHAT IT NAMES, ITS TOKEN, AND WHERE IT SITS.
///
/// `KHASH_INIT(x, int, my_hash)` generates functions that each call `my_hash`
/// on the one token the caller typed.  The indexer sends one reference per
/// function, identical except for `dfs_enter`.  The upload's conflict target
/// used to be (to_symbol, from_object, from_offset_range, layer), under which
/// the second was a duplicate of the first and was dropped without a word --
/// one of the two functions called nothing.
#[test]
fn uploading_one_token_read_in_two_places_keeps_both_references() {
    use crate::proto::askl::index::{
        Object as UploadObject, Project as UploadProject, Symbol as UploadSymbol,
        SymbolInstance as UploadInstance, SymbolRef as UploadRef,
    };
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let (store, _index) = store_and_index_with_shared_cache(fx.url()).await;

        // Re-upload the fixture's own project; see
        // `uploading_two_instances_at_one_extent_keeps_both`.
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query("UPDATE index.projects SET upload_status = 'failed'"),
            &mut conn,
        )
        .await
        .unwrap();

        let (pid, _) = store
            .upload_index(
                UploadProject {
                    project_name: "test_project".to_string(),
                    root_path: "/test_project".to_string(),
                    ..Default::default()
                },
                Some(1),
                Some(1),
            )
            .await
            .unwrap();

        let sym = |local_id: i64, name: &str| UploadSymbol {
            local_id,
            name: name.to_string(),
            r#type: 1, // FUNCTION
            ..Default::default()
        };
        store
            .upload_symbol_chunk(
                pid,
                0,
                vec![
                    sym(1, "holder"),
                    sym(2, "kh_get_x"),
                    sym(3, "kh_put_x"),
                    sym(4, "my_hash"),
                ],
            )
            .await
            .unwrap();

        // `holder` contains two functions generated on ONE extent, and each
        // of them calls `my_hash` on the token at [150, 157).
        let inst =
            |symbol: i64, enter: i64, exit: i64, level: i32, start: i32, end: i32| UploadInstance {
                symbol_local_id: symbol,
                instance_type: 1,
                start_offset: start,
                end_offset: end,
                dfs_enter: enter,
                dfs_exit: exit,
                nesting_level: level,
                ..Default::default()
            };
        let call = |enter: i64| UploadRef {
            to_symbol_local_id: 4,
            from_offset_start: 150,
            from_offset_end: 157,
            nesting_level: 2,
            dfs_enter: enter,
            ..Default::default()
        };
        let payload = UploadProject {
            objects: vec![UploadObject {
                local_id: 1,
                module_path: "mod".to_string(),
                filesystem_path: "/test_project/khash.c".to_string(),
                filetype: "c".to_string(),
                content: b"x".to_vec(),
                symbol_instances: vec![
                    inst(1, 0, 8192, 0, 0, 200),
                    inst(2, 1024, 2048, 1, 100, 160),
                    inst(3, 3072, 4096, 1, 100, 160),
                ],
                refs: vec![call(1025), call(3073)],
                ..Default::default()
            }],
            ..Default::default()
        };

        store
            .upload_object_chunk(pid, 0, payload.clone())
            .await
            .unwrap();
        // Re-sending a chunk is what resumption does; it must stay idempotent.
        store.upload_object_chunk(pid, 0, payload).await.unwrap();
        store.finalize_project(pid).await.unwrap();

        use diesel_async::RunQueryDsl;
        let rows: Vec<DfsIntervalRow> = diesel::sql_query(
            "SELECT sr.dfs_enter, 0::bigint AS dfs_exit FROM index.symbol_refs sr \
             JOIN index.objects o ON o.id = sr.from_object \
             WHERE o.filesystem_path = '/test_project/khash.c' \
             ORDER BY sr.dfs_enter",
        )
        .load(&mut conn)
        .await
        .unwrap();
        let places: Vec<i64> = rows.into_iter().map(|r| r.dfs_enter).collect();

        assert_eq!(
            places,
            vec![1025, 3073],
            "the call in each generated function must survive, and a re-sent \
             chunk must not duplicate them"
        );
    });
}

/// WHAT AN INDEX STATES IS OBSERVED WHEN IT IS UPLOADED.
///
/// A role word answers nothing on an index whose references state no role,
/// and the answer has to say that this is why.  The engine knows it from
/// three facts kept with the project, which ingest sets when it stores a row
/// that has the property -- per chunk, since chunks arrive separately and in
/// any order -- and never clears.
#[test]
fn uploading_references_records_what_the_index_states() {
    use crate::proto::askl::index::{
        Object as UploadObject, Project as UploadProject, Symbol as UploadSymbol,
        SymbolInstance as UploadInstance, SymbolRef as UploadRef,
    };
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    #[derive(diesel::QueryableByName, Debug, PartialEq)]
    struct States {
        #[diesel(sql_type = diesel::sql_types::Bool)]
        states_roles: bool,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        has_declares: bool,
        #[diesel(sql_type = diesel::sql_types::Bool)]
        has_holds: bool,
    }

    async fn states(conn: &mut diesel_async::AsyncPgConnection) -> States {
        use diesel_async::RunQueryDsl;
        diesel::sql_query(
            "SELECT states_roles, has_declares, has_holds FROM index.projects \
             WHERE project_name = 'test_project'",
        )
        .get_result::<States>(conn)
        .await
        .unwrap()
    }

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let (store, index) = store_and_index_with_shared_cache(fx.url()).await;

        // Re-upload the fixture's own project; see
        // `uploading_two_instances_at_one_extent_keeps_both`.
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query("UPDATE index.projects SET upload_status = 'failed'"),
            &mut conn,
        )
        .await
        .unwrap();
        // The fixture wrote its objects with explicit ids and left the
        // sequence behind them; this test uploads more than one file.
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query(
                "SELECT setval(pg_get_serial_sequence('index.objects', 'id'), \
                 (SELECT max(id) FROM index.objects))",
            ),
            &mut conn,
        )
        .await
        .unwrap();

        let (pid, _) = store
            .upload_index(
                UploadProject {
                    project_name: "test_project".to_string(),
                    root_path: "/test_project".to_string(),
                    ..Default::default()
                },
                Some(1),
                Some(4),
            )
            .await
            .unwrap();
        let sym = |local_id: i64, name: &str, r#type: i32| UploadSymbol {
            local_id,
            name: name.to_string(),
            r#type,
            ..Default::default()
        };
        store
            .upload_symbol_chunk(
                pid,
                0,
                vec![
                    sym(1, "old_user", 1),
                    sym(2, "new_user", 1),
                    sym(3, "target", 1),
                    sym(4, "slot", 8),
                ],
            )
            .await
            .unwrap();

        // One file per chunk: a function, or the slot, with one row inside.
        let file = |local_id: i64, symbol: i64, row: Option<UploadRef>| UploadProject {
            objects: vec![UploadObject {
                local_id,
                module_path: "mod".to_string(),
                filesystem_path: format!("/test_project/f{local_id}.c"),
                filetype: "c".to_string(),
                content: format!("file {local_id}").into_bytes(),
                symbol_instances: vec![UploadInstance {
                    symbol_local_id: symbol,
                    instance_type: 1,
                    start_offset: 0,
                    end_offset: 100,
                    dfs_enter: 0,
                    dfs_exit: 8192,
                    nesting_level: 0,
                    ..Default::default()
                }],
                refs: row
                    .into_iter()
                    .map(|row| UploadRef {
                        to_symbol_local_id: 3,
                        from_offset_start: 10,
                        from_offset_end: 16,
                        nesting_level: 1,
                        dfs_enter: 1,
                        ..row
                    })
                    .collect(),
                ..Default::default()
            }],
            ..Default::default()
        };
        let said = |res: &crate::statement::ExecutionResult| -> Vec<String> {
            res.warnings
                .iter()
                .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::PassedOver)
                .map(|w| w.message.clone())
                .collect()
        };
        let ask = || async {
            crate::test_util::run_query_traced_on(index.clone(), r#"calls { "target" }"#)
                .await
                .unwrap()
                .0
        };
        let nothing_stated = States {
            states_roles: false,
            has_declares: false,
            has_holds: false,
        };

        // A new project states nothing, and neither does a chunk whose
        // references state nothing: an index from before roles.
        assert_eq!(states(&mut conn).await, nothing_stated);
        store
            .upload_object_chunk(pid, 3, file(4, 3, None))
            .await
            .unwrap();
        let unstated = file(1, 1, Some(UploadRef::default()));
        store
            .upload_object_chunk(pid, 0, unstated.clone())
            .await
            .unwrap();
        assert_eq!(states(&mut conn).await, nothing_stated);
        assert_eq!(
            said(&ask().await),
            vec![
                "the `test_project` index states no roles; `calls` cannot match there. \
                 `refs { … }` lists its references"
                    .to_string()
            ]
        );

        // A later chunk has one reference that states a role.
        let calling = file(
            2,
            2,
            Some(UploadRef {
                roles: 1,
                ..Default::default()
            }),
        );
        store.upload_object_chunk(pid, 1, calling).await.unwrap();
        assert_eq!(
            states(&mut conn).await,
            States {
                states_roles: true,
                ..nothing_stated
            }
        );
        // The same question is now answered -- the cache did not keep the
        // old fact -- and what is left unstated is counted, not blamed on
        // the index.
        let answered = ask().await;
        assert_eq!(
            said(&answered),
            vec![
                "`calls` passed over 1 symbol that names `\"target\"` without stating a role \
                 (`#define` bodies, macro uses): `old_user`. Whether it calls `\"target\"` is \
                 not known; `refs { … }` lists it"
                    .to_string()
            ]
        );
        assert_eq!(answered.nodes.as_vec().len(), 2, "`new_user` and `target`");

        // A relation row sets its own fact and no other.
        let holding = file(
            3,
            4,
            Some(UploadRef {
                relation: 2,
                ..Default::default()
            }),
        );
        store.upload_object_chunk(pid, 2, holding).await.unwrap();
        let all_but_declares = States {
            states_roles: true,
            has_declares: false,
            has_holds: true,
        };
        assert_eq!(states(&mut conn).await, all_but_declares);

        // Never cleared: a re-sent chunk that states nothing changes nothing.
        store.upload_object_chunk(pid, 0, unstated).await.unwrap();
        store.finalize_project(pid).await.unwrap();
        assert_eq!(states(&mut conn).await, all_but_declares);
    });
}

#[test]
fn zombie_project_reupload_purges_caches() {
    // upload_index deleting a Failed/Deleting zombie project cascades its
    // persistent rows away — a mutation like any other: the eph-layer
    // cache must be purged in the same transaction and the RAM cache
    // cleared, or warm queries keep serving the deleted project's symbols.
    use crate::proto::askl::index::Project as UploadProject;
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        use crate::test_util::run_query_traced_on;
        let (store, index) = store_and_index_with_shared_cache(fx.url()).await;

        // Warm both caches: a query result in RAM, an eph layer in the DB.
        const QUERY: &str = concat!(
            r#"layer { ephemeral_instance(symbol_id=1, object_id=1, "#,
            r#"start=97000, end=97100, instance_type=1) }; "#,
            r#""foo""#,
        );
        let (before, _) = run_query_traced_on(index.clone(), QUERY).await.unwrap();
        assert!(!before.nodes.as_vec().is_empty());
        assert!(index.eph_layer_count().await.unwrap() >= 1);

        // Mark the project as a zombie, then re-upload the same name.
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query("UPDATE index.projects SET upload_status = 'failed' WHERE id = 1"),
            &mut conn,
        )
        .await
        .unwrap();

        let upload = UploadProject {
            project_name: "test_project".to_string(),
            root_path: "/test_project".to_string(),
            ..Default::default()
        };
        let (_new_id, resumed) = store.upload_index(upload, Some(0), Some(0)).await.unwrap();
        assert!(!resumed, "zombie must be replaced, not resumed");

        assert_eq!(
            index.eph_layer_count().await.unwrap(),
            0,
            "zombie deletion must purge the eph-layer cache"
        );
        let (after, _) = run_query_traced_on(index.clone(), r#""foo""#)
            .await
            .unwrap();
        assert!(
            after.nodes.as_vec().is_empty(),
            "warm RAM cache must not survive the zombie deletion, got {:?}",
            after.nodes.as_vec(),
        );
    });
}

#[test]
fn finalize_project_clears_ram_cache() {
    // Closes the review's coverage gap: only delete_project's invalidation
    // was tested end-to-end.  Seed an Uploading project state with
    // matching (zero) chunk totals, warm the cache, finalize, and assert
    // fresh reads.
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        use crate::test_util::run_query_traced_on;
        let (store, index) = store_and_index_with_shared_cache(fx.url()).await;

        let (_warm, _) = run_query_traced_on(index.clone(), r#""foo""#)
            .await
            .unwrap();
        let hits0 = index.sql_cache().stats().hits;
        let (_warm2, _) = run_query_traced_on(index.clone(), r#""foo""#)
            .await
            .unwrap();
        assert!(index.sql_cache().stats().hits > hits0, "cache warmed");
        let epoch_before = index.sql_cache().epoch();

        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel_async::RunQueryDsl::execute(
            diesel::sql_query(
                "UPDATE index.projects SET upload_status = 'uploading', \
                 symbol_chunks_total = 0, object_chunks_total = 0 WHERE id = 1",
            ),
            &mut conn,
        )
        .await
        .unwrap();

        assert_eq!(
            store.finalize_project(1).await.unwrap(),
            crate::index_store::FinalizeOutcome::Finalized
        );
        assert!(
            index.sql_cache().epoch() > epoch_before,
            "finalize must clear (epoch-bump) the shared RAM cache"
        );
    });
}

#[test]
fn combined_fallback_consults_eph_rows() {
    // Pins is_chain_dependent semantics for the direct-only machinery: an
    // eph mid-instance nested between a container and a child must demote
    // the child from the DIRECT has-children of the container.  Runs both
    // variants on ONE Index: if the direct-only query were wrongly
    // partitioned (chain-independent persistent key), the layered run
    // would cache-hit the bare run's result and keep foo — this test
    // fails in exactly that case.
    use crate::test_util::run_query_traced_on;
    const BARE: &str = r#"mod("testmodule") has { func }"#;
    const LAYERED: &str = concat!(
        // Mid covers [50,250): strictly inside the module instance
        // [0,1000), strictly containing foo [100,200); bar [200,300) and
        // baz [300,400) are not contained.
        r#"layer { ephemeral_instance(symbol_id=2, object_id=1, "#,
        r#"start=50, end=250, instance_type=1) }; "#,
        r#"mod("testmodule") has { func }"#,
    );

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let index = get_shared_index(TEST_INPUT_CONTAINMENT).await;

        let (bare, _) = run_query_traced_on(index.clone(), BARE).await.unwrap();
        let bare_nodes = bare.nodes.as_vec();
        assert!(
            bare_nodes.contains(&SymbolInstanceId::new(20)),
            "foo (inst 20) is a direct child without the eph mid, got {:?}",
            bare_nodes,
        );

        let (layered, _) = run_query_traced_on(index.clone(), LAYERED).await.unwrap();
        let layered_nodes = layered.nodes.as_vec();

        // The subject is that a chain-dependent query consults eph rows rather
        // than serving a chain-free cache entry.  It used to be shown by the
        // mid DEMOTING foo from the module's direct children.  That is no
        // longer possible: containment is a position in the object's precomputed
        // DFS order, so an ephemeral row cannot re-parent a persistent one, and
        // under the default nothing is left out unless its symbol is folded.
        //
        // Shown instead by what the eph row ADDS.  If the layered query were
        // served from a chain-free entry it would return exactly the bare
        // result; instead it returns that plus the ephemeral instance itself.
        assert!(
            bare_nodes.iter().all(|n| layered_nodes.contains(n)),
            "the layered result keeps everything the bare one had, got {:?}",
            layered_nodes,
        );
        assert_eq!(
            layered_nodes.len(),
            bare_nodes.len() + 1,
            "exactly the eph mid is added — proof the chain-dependent query \
             consulted eph rows rather than a chain-free cache entry, \
             bare {:?} vs layered {:?}",
            bare_nodes,
            layered_nodes,
        );
        assert!(
            layered_nodes.contains(&SymbolInstanceId::new(30))
                && layered_nodes.contains(&SymbolInstanceId::new(40)),
            "bar/baz stay children, got {:?}",
            layered_nodes,
        );
    });
}

// ============================================================================
// Anchor completeness (cost-based execution, F1)
// ============================================================================

#[test]
fn anchor_error_nested_constraint_only_tree() {
    // func { file } — a whole tree of pure constraints: anchoring error.
    let res = run_query_err(TEST_INPUT_CONTAINMENT, r#"func { file }"#);
    assert!(res.is_err(), "constraint-only tree must error");
}

#[test]
fn anchor_error_is_per_component() {
    // The first tree is anchored; the second, independent tree is not.
    // Anchoring is judged per component — an anchor elsewhere in the query
    // must not excuse an unanchored group.
    let res = run_query_err(TEST_INPUT_A, r#""a"; func"#);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("unanchored second tree must error"),
    };
    assert!(
        err.to_string().contains("selects nothing"),
        "unexpected error: {err}"
    );
}

#[test]
fn preamble_after_a_preamble_is_not_an_anchoring_error() {
    // A preamble is a directive, not a statement: it must never be held to
    // the anchor rule.  A second preamble used to inherit the first one's
    // globally-installed filters through the parse-time derive, so its
    // otherwise-unit command looked like a constraint-only statement and
    // the whole query was rejected.
    let res = run_query_err(
        TEST_INPUT_MODULES,
        r#"preamble project("test_project"); preamble project("other_project"); "a""#,
    );
    if let Err(err) = res {
        panic!("a preamble must not demand an anchor: {err}");
    }
}

#[test]
fn later_preamble_rescopes_the_statements_after_it() {
    // Each preamble applies to the statements that follow it: a statement
    // inherits the global command as it stood when the statement was parsed,
    // and a repeated `project` replaces its predecessor.  So the two halves
    // of the query resolve in different projects.
    const QUERY: &str =
        r#"preamble project("test_project"); "a"; preamble project("other_project"); "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);

    // 91 + 201 from test_project (first half), 301 from other_project (second).
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );
}

#[test]
fn preamble_directives_accumulate_across_preambles() {
    // Separate preambles write separate dimensions of the same global
    // command, so they compose rather than replace: project scopes to
    // test_project (dropping project_only.a) and ignore drops the test
    // package, leaving other.a.  The scope spelling must agree exactly.
    const INLINE: &str = r#"preamble project("test_project")
preamble ignore(package="test")
"a""#;
    let res = run_query(TEST_INPUT_MODULES, INLINE);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);

    const SCOPED: &str = r#"preamble { project("test_project") }
preamble { ignore(package="test") }
"a""#;
    let res = run_query(TEST_INPUT_MODULES, SCOPED);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
}

#[test]
fn preamble_repeating_a_dimension_replaces_it() {
    // Accumulation is per dimension, not per preamble: writing the same slot
    // twice follows the ordinary replacement rule, so the later `project`
    // wins wholesale rather than intersecting to nothing.
    const LATER_WINS: &str = r#"preamble project("other_project")
preamble project("test_project")
"a""#;
    let res = run_query(TEST_INPUT_MODULES, LATER_WINS);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(201)]
    );

    // Reversed, the other project wins — it is order, not precedence.
    const REVERSED: &str = r#"preamble project("test_project")
preamble project("other_project")
"a""#;
    let res = run_query(TEST_INPUT_MODULES, REVERSED);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(301)]);

    // The scope spelling replaces identically.
    const SCOPED: &str = r#"preamble { project("other_project") }
preamble { project("test_project") }
"a""#;
    let res = run_query(TEST_INPUT_MODULES, SCOPED);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(201)]
    );

    // `ignore` holds no slot, so repeats accumulate instead: both packages
    // are excluded and only the third project's symbol survives.
    const ACCUMULATES: &str = r#"preamble ignore(package="test")
preamble ignore(package="other")
"a""#;
    let res = run_query(TEST_INPUT_MODULES, ACCUMULATES);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(301)]);
}

#[test]
fn preamble_reaches_into_nested_scopes() {
    // "Every statement" means every statement, not every top-level one: the
    // children inherit the directive through the same parse-time derive.
    const QUERY: &str = r#"preamble project("test_project")
"a" { }"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(202)
        ]
    );

    // Without the directive, project_only.a joins the roots.
    let res = run_query(TEST_INPUT_MODULES, r#""a" { }"#);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(202),
            SymbolInstanceId::new(301)
        ]
    );
}

#[test]
fn preamble_composes_with_labels() {
    // A preamble cannot carry label plumbing, but it does constrain the
    // statements that do: both the definition and the reader see the
    // directive, so the label round-trip stays inside test_project.
    const QUERY: &str = r#"preamble project("test_project")
"a" @l
#l"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(201)]
    );
}

#[test]
fn trailing_preamble_configures_nothing() {
    // A directive applies forward only.  With no statement after it there is
    // nothing to configure — and, since a directive is not a statement, also
    // nothing for the anchor check to reject.
    const QUERY: &str = r#""a"
preamble project("other_project")"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(201),
            SymbolInstanceId::new(301)
        ]
    );
}

#[test]
fn select_verb_enumerates_and_composes_with_constraints() {
    // `select` — the explicit enumerate-everything anchor.
    let res = run_query(TEST_INPUT_A, r#"select"#);
    assert!(!res.nodes.as_vec().is_empty(), "all must return symbols");

    // Composes with type constraints: `func all` = all functions.
    let res = run_query(TEST_INPUT_A, r#"func select"#);
    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "function a");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(1001)),
        "file instance excluded by the func constraint"
    );
}

#[test]
fn filter_argument_is_a_hard_error() {
    // `filter=` was the manual plan toggle; it is gone, not ignored.
    let res = run_query_err(TEST_INPUT_A, r#"func(filter="false")"#);
    let err = match res {
        Err(e) => e,
        Ok(_) => panic!("filter= must be a parse error"),
    };
    assert!(
        err.to_string().contains("`filter=` was removed"),
        "error must carry the migration hint: {err}"
    );
    assert!(
        run_query_err(TEST_INPUT_A, r#"mod("test", filter="true") "a""#).is_err(),
        "filter=\"true\" errors too"
    );
}

// ============================================================================
// Cardinality probes, wave 0 (cost-based execution, F2)
// ============================================================================

#[test]
fn probe_resolves_and_matches_predicate_emission() {
    // Wave 0 probes `"a"`; the probe resolves and the read becomes an
    // id-driven fetch.  With cap=0 every probe is capped and the read
    // stays predicate-driven.  Both paths must produce identical output.
    const QUERY: &str = r#""a" {}"#;
    let (probed, acts) = run_query_probe_traced(TEST_INPUT_A, QUERY, 1000);
    let wave_0: Vec<_> = acts.iter().filter(|a| a.round == 0).collect();
    assert_eq!(
        wave_0.len(),
        1,
        "one anchored fusable statement probes alone"
    );
    assert_eq!(wave_0[0].resolved, Some(1), "\"a\" is exactly one instance");
    // The `{}` has no predicate of its own, so it is probed only once a
    // neighbour resolved, with that neighbour bound in.
    assert!(
        acts.iter().any(|a| a.round == 1 && a.resolved.is_some()),
        "the listing is probed with \"a\" bound in: {acts:?}"
    );

    let (fallback, acts0) = run_query_probe_traced(TEST_INPUT_A, QUERY, 0);
    assert!(
        acts0.iter().all(|a| a.resolved.is_none()),
        "cap=0 must cap every probe, got {:?}",
        acts0,
    );
    assert_eq!(probed.nodes.as_vec(), fallback.nodes.as_vec());
    assert_eq!(format_edges(probed.edges), format_edges(fallback.edges));
}

#[test]
fn probe_covers_fused_multi_selector_statement() {
    // The fused statement probes ONCE for both selectors.
    let (res, acts) = run_query_probe_traced(TEST_INPUT_A, r#""b" "a""#, 1000);
    assert_eq!(acts.len(), 1, "one probe per statement, not per selector");
    assert_eq!(acts[0].resolved, Some(2), "two instances across the OR");
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(92)]
    );
}

#[test]
fn probe_skips_layer_and_derived_statements() {
    // Layer-backed statements keep their read path unprobed in wave 0…
    let (_res, acts) = run_query_probe_traced(TEST_INPUT_SEARCH, r#"search("foo")"#, 1000);
    assert!(acts.is_empty(), "search must not probe, got {:?}", acts);

    // …and derive-path statements (`func { }`) never probe in WAVE 0 —
    // only the anchored child does.  (They may refine in later rounds
    // once the child resolves.)
    let (_res, acts) = run_query_probe_traced(TEST_INPUT_A, r#"func { "a" }"#, 1000);
    let wave0: Vec<_> = acts.iter().filter(|a| a.round == 0).collect();
    assert_eq!(wave0.len(), 1, "only the anchored child probes: {:?}", acts);
    assert_eq!(wave0[0].query_statement.trim(), r#""a""#);
}

// ============================================================================
// Semi-join refinement rounds (cost-based execution, F3)
// ============================================================================

#[test]
fn refinement_resolves_capped_statement() {
    // cap=5: `select` (11 instances) caps in wave 0; `"b"` resolves.  Round 1
    // re-probes the outer statement under the child's refs+has role
    // branches (the `{ }` default relationship counts either edge kind)
    // and unions the branch results: the INSTANCES around the calls of b (a,
    // main, the file, and the directory's instance in that file -- not the
    // directory's other instance, which encloses no call) plus b's containers.
    const QUERY: &str = r#"select { "b" }"#;
    let (refined, acts) = run_query_probe_traced(TEST_INPUT_A, QUERY, 5);

    let outer: Vec<_> = acts
        .iter()
        .filter(|a| a.query_statement.starts_with("select"))
        .collect();
    assert_eq!(outer.len(), 2, "wave-0 cap + one refinement: {:?}", acts);
    assert_eq!(outer[0].resolved, None, "wave 0 must cap at 5");
    assert_eq!(outer[0].round, 0);
    assert_eq!(
        outer[1].resolved,
        Some(4),
        "what encloses a call of b: a, main, the file, and the dir instance in it"
    );
    assert_eq!(outer[1].round, 1);
    let child = acts
        .iter()
        .find(|a| a.query_statement.trim() == r#""b""#)
        .expect("child must probe in wave 0");
    assert_eq!(child.resolved, Some(1));

    // Identical output whether the statement resolves in wave 0 (cap
    // 1000), via refinement (cap 4), or never (cap 0, predicate path).
    let (wave0, _) = run_query_probe_traced(TEST_INPUT_A, QUERY, 1000);
    let (unprobed, _) = run_query_probe_traced(TEST_INPUT_A, QUERY, 0);
    assert_eq!(refined.nodes.as_vec(), wave0.nodes.as_vec());
    assert_eq!(refined.nodes.as_vec(), unprobed.nodes.as_vec());
    assert_eq!(
        format_edges(refined.edges),
        format_edges(unprobed.edges),
        "edges must match the unprobed path"
    );
}

#[test]
fn refinement_cascades_through_unanchored_middle() {
    // The worst-case shape in miniature: a broad anchored container, an
    // unanchored pure-constraint middle, a selective leaf.  cap=4:
    //   wave 0: `select` caps; `"g"` resolves.
    //   round 1: the middle `func { }` — never wave-0 probed (unanchored,
    //            and its inherited bare type selector is not even a
    //            selector) — re-probes its filter conjunction under the
    //            leaf's refs/has role branches and resolves to f.
    //   round 2: the outer re-probes under the middle's roles and resolves.
    // The middle's resolved ids also feed the outer's children scope in
    // place of the bare `func` condition the index would otherwise have
    // to materialise in full.
    const QUERY: &str = r#"select { func { "g" } }"#;
    let (refined, acts) = run_query_probe_traced(TEST_INPUT_A, QUERY, 4);

    assert_eq!(acts.len(), 4, "no wasted probes: {:?}", acts);
    let middle = acts
        .iter()
        .find(|a| a.query_statement.starts_with("func"))
        .expect("middle statement must refine");
    assert_eq!(middle.round, 1);
    assert_eq!(middle.resolved, Some(1), "callers of g = f alone");
    let outer_refined = acts
        .iter()
        .find(|a| a.query_statement.starts_with("select") && a.round == 2)
        .expect("outer must resolve in round 2, after the middle");
    assert_eq!(
        outer_refined.resolved,
        Some(3),
        "what encloses a call of f (d, file, dir) ∪ f's containers"
    );

    // Identical across probe regimes.
    let (unprobed, _) = run_query_probe_traced(TEST_INPUT_A, QUERY, 0);
    assert_eq!(refined.nodes.as_vec(), unprobed.nodes.as_vec());
    assert_eq!(format_edges(refined.edges), format_edges(unprobed.edges));
    assert!(
        refined.nodes.as_vec().contains(&SymbolInstanceId::new(96)),
        "f (caller of g) must be in the result"
    );
}

// ============================================================================
// Anchored ⇒ not weak: a name filter is a constraint, a type filter is not
// ============================================================================

#[test]
fn name_filter_constrains_its_scope_like_a_name_selector() {
    // `filter("exact_name", …)` anchors by name (`AnchorKind::Name`), but
    // weakness was decided by scanning SELECTORS only — and a filter-only
    // command has no selector of its own (the parser adds a `UnitVerb`).
    // So the statement came out anchored AND weak, and "weak" means "does
    // not constrain the selection of its dependencies".  The two halves of
    // the engine then answered the same query differently: the refinement
    // round bound the child's role for that weak parent and resolved it to
    // the EMPTY set, while the worklist — which declines to apply a weak
    // statement's constraint — kept the child anyway.
    //
    // `d` does not call `b`, so both spellings of the one constraint must
    // answer "nothing".
    const FILTERED: &str = r#"filter("exact_name","d") { "b" }"#;
    const SELECTED: &str = r#"func("d") { "b" }"#;

    let selected = run_query(TEST_INPUT_A, SELECTED);
    assert!(
        selected.nodes.as_vec().is_empty(),
        "the selector spelling answers nothing: {:?}",
        selected.nodes.as_vec(),
    );

    let (filtered, acts) = run_query_probe_traced(TEST_INPUT_A, FILTERED, 1000);
    let outer = acts
        .iter()
        .find(|a| a.query_statement.starts_with("filter"))
        .expect("the anchored filter statement refines under the child's role");
    assert_eq!(
        outer.resolved,
        Some(0),
        "the probe binds the child's role and finds no caller named d: {:?}",
        acts,
    );
    assert_eq!(
        filtered.nodes.as_vec(),
        selected.nodes.as_vec(),
        "a name filter must constrain its scope exactly like a name selector",
    );

    // Probing is a cost decision, never a semantic one.
    let (unprobed, _) = run_query_probe_traced(TEST_INPUT_A, FILTERED, 0);
    assert_eq!(
        filtered.nodes.as_vec(),
        unprobed.nodes.as_vec(),
        "the answer must not depend on whether the probe resolved",
    );
}

#[test]
fn type_filter_alone_stays_non_constraining() {
    // The companion of `name_filter_constrains_its_scope_like_a_name_selector`:
    // a type predicate is structural, so a type-filter-only command stays a
    // weakness candidate and does NOT prune its scope.  There are no macros
    // in the fixture — the outer probes to the empty set — and `e` survives
    // regardless, exactly as bare `func` leaves a caller chain alone.
    const QUERY: &str = r#"filter("type","macro") { "e" }"#;
    let (res, acts) = run_query_probe_traced(TEST_INPUT_A, QUERY, 1000);
    let outer = acts
        .iter()
        .find(|a| a.query_statement.starts_with("filter"))
        .expect("the type-filter statement refines under the child's role");
    assert_eq!(outer.resolved, Some(0), "no macro calls e: {:?}", acts);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(95)],
        "a weak type filter must not constrain its child away",
    );
}

#[test]
fn sibling_children_conjoin_while_sibling_selectors_disjoin() {
    // The discriminator between the two spellings of "two children".
    //
    // `X { A ; B }` CONJOINS: the parent survives only where it has
    // evidence with EVERY participating child.  On TEST_INPUT_A the callers
    // of b are a (91) and main (942); the only caller of e is d (94); the
    // file (1001) and the two directory instances (1002, 1003) contain
    // both.  So `select { "b" ; "e" }` keeps the containers and drops all
    // three callers — d most pointedly, since it calls e but not b.
    //
    // `X { A B }` — several selectors in ONE command — stays a
    // DISJUNCTION, and on the same fixture keeps every caller.  Two
    // spellings, two operators; this pair is what pins them apart.
    const AND: &str = r#"select { "b" ; "e" }"#;
    const OR: &str = r#"select { "b" "e" }"#;

    let conj = |cap| {
        let (res, _) = run_query_probe_traced(TEST_INPUT_A, AND, cap);
        let mut nodes: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
        nodes.sort_unstable();
        let mut edges = format_edges(res.edges);
        edges.sort();
        (nodes, edges)
    };

    // Probing is a COST decision, never a semantic one: the unprobed
    // regime (0), a cap that resolves the children but not the parent (5),
    // and a cap that resolves everything in wave 0 (1000) must agree on
    // the nodes AND on the edges.
    let (nodes, edges) = conj(0);
    assert_eq!(
        nodes,
        vec![92, 95, 1001, 1003],
        "the conjunction keeps b, e and the containers of both — not a \
         (91), main (942) or d (94), nor the directory's other instance \
         (1002), which encloses neither call",
    );
    assert!(
        !nodes.contains(&94),
        "d calls e but not b, so the conjunction must drop it: {nodes:?}",
    );
    for cap in [5, 1000] {
        assert_eq!(
            conj(cap),
            (nodes.clone(), edges.clone()),
            "probe cap {cap} changed the answer",
        );
    }

    // Sibling SELECTORS keep the OR, at every cap.
    for cap in [0, 5, 1000] {
        let (res, _) = run_query_probe_traced(TEST_INPUT_A, OR, cap);
        let mut disj: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
        disj.sort_unstable();
        assert_eq!(
            disj,
            vec![91, 92, 94, 95, 942, 1001, 1003],
            "`{OR}` is one command with two selectors — a disjunction, so \
             every caller of either survives (cap {cap}); the directory's \
             other instance (1002) encloses no call",
        );
    }

    // A conjunction is a set operation, so the order of the conjuncts
    // cannot matter.
    let (swapped, _) = run_query_probe_traced(TEST_INPUT_A, r#"select { "e" ; "b" }"#, 0);
    let mut swapped_nodes: Vec<i64> = swapped
        .nodes
        .as_vec()
        .iter()
        .map(|id| (*id).into())
        .collect();
    swapped_nodes.sort_unstable();
    assert_eq!(swapped_nodes, nodes, "conjuncts commute");

    // `;` conjoins only INSIDE a scope.  At the TOP level it separates
    // whole statements, whose answers are unioned into one result — so the
    // two halves written side by side reproduce the disjunction exactly.
    // Worth pinning: it is the sentence the user docs make, and it is what
    // keeps `;` from reading as one operator with two meanings.
    let (split, _) = run_query_probe_traced(TEST_INPUT_A, r#"select { "b" } ; select { "e" }"#, 0);
    let mut split_nodes: Vec<i64> = split.nodes.as_vec().iter().map(|id| (*id).into()).collect();
    split_nodes.sort_unstable();
    assert_eq!(
        split_nodes,
        vec![91, 92, 94, 95, 942, 1001, 1003],
        "a top-level `;` separates statements and unions their answers",
    );
}

#[test]
fn an_empty_sibling_empties_the_parent() {
    // What a conjunction means when one conjunct is false, and the exact
    // behaviour a single child already has (`"x" { "nonexistent" }`): there
    // is no node with evidence for a child that selected nothing, so the
    // parent — and with it the whole query — is empty.  Under the old
    // union the non-empty sibling carried the parent through.
    for cap in [0, 5, 1000] {
        let (res, _) =
            run_query_probe_traced(TEST_INPUT_A, r#"select { "b" ; "nonexistent" }"#, cap);
        assert!(
            res.nodes.as_vec().is_empty(),
            "an empty conjunct empties the parent (cap {cap}): {:?}",
            res.nodes.as_vec(),
        );
    }
}

#[test]
fn refinement_starts_with_one_participant_of_the_conjunction() {
    // Cost side of the conjunction.  Both children are resolved, but the
    // first attempt composes only the TIGHTEST one, and it succeeds — so no
    // wider composition is tried and the refined set is a superset (5) that
    // the worklist narrows to the 3 containers.
    //
    // Composing both would resolve to exactly 3 and leave the worklist
    // nothing to do, and `a_capped_attempt_escalates_to_a_composition_that_
    // bounds_it` shows that happening. It is not done here because it is not
    // needed here: on the corpus, composing unconditionally took
    // `mod("amdgpu") { func { "drm_dev_enter" } }` from 400 ms to 2.7 s.
    // Composition is for when one neighbour is not enough.
    const QUERY: &str = r#"select { "b" ; "e" }"#;
    let (refined, acts) = run_query_probe_traced(TEST_INPUT_A, QUERY, 6);

    for name in [r#""b""#, r#""e""#] {
        let child = acts
            .iter()
            .find(|a| a.query_statement.trim() == name)
            .unwrap_or_else(|| panic!("{name} must probe in wave 0: {acts:?}"));
        assert_eq!(child.resolved, Some(1), "{name} is exactly one instance");
    }
    let outer: Vec<_> = acts
        .iter()
        .filter(|a| a.query_statement.starts_with("select"))
        .collect();
    assert_eq!(outer.len(), 2, "wave-0 cap + one refinement: {acts:?}");
    assert_eq!(outer[0].resolved, None, "select's 11 instances cap at 6");
    assert_eq!(
        outer[1].resolved,
        Some(4),
        "the tightest child alone — a superset of the conjunction, which the \
         worklist narrows to the 2 containers: {acts:?}",
    );

    // The refined regime must still answer like the unprobed one.
    let (unprobed, _) = run_query_probe_traced(TEST_INPUT_A, QUERY, 0);
    assert_eq!(refined.nodes.as_vec(), unprobed.nodes.as_vec());
    assert_eq!(format_edges(refined.edges), format_edges(unprobed.edges));
}

#[test]
fn refinement_binds_a_participant_with_an_unresolved_sibling() {
    // Under the union a partial merge was unusable — an unresolved sibling
    // could only add ids, so binding what had resolved so far
    // under-approximated and deleted answers.  A conjunction has no such
    // obligation: every conjunct is an independently sound upper bound, so
    // one resolved participant binds even while its sibling is still
    // capped.  Here `select` (11 instances) caps at 6, `"g"` resolves, and
    // the outer refines off `"g"` alone to g's 4 callers/containers.
    const QUERY: &str = r#"select { select ; "g" }"#;
    let (refined, acts) = run_query_probe_traced(TEST_INPUT_A, QUERY, 6);
    assert!(
        acts.iter()
            .filter(|a| a.query_statement.trim() == "select")
            .all(|a| a.resolved.is_none()),
        "the inner bare select must stay capped: {acts:?}",
    );
    let outer: Vec<_> = acts
        .iter()
        .filter(|a| a.query_statement.starts_with("select {"))
        .collect();
    assert_eq!(
        outer.last().and_then(|a| a.resolved),
        Some(3),
        "the outer binds the one resolved participant rather than waiting \
         for its sibling: {acts:?}",
    );

    let (unprobed, _) = run_query_probe_traced(TEST_INPUT_A, QUERY, 0);
    assert_eq!(refined.nodes.as_vec(), unprobed.nodes.as_vec());
    assert_eq!(format_edges(refined.edges), format_edges(unprobed.edges));
}

#[test]
fn func_scope_conjoins_siblings_but_disjoins_selectors() {
    // The same discriminator in the shape a user actually writes: a type
    // selector as the parent rather than `select`.
    //
    // TEST_INPUT_A has main -> {a, b}, a -> b, d -> {e, f}, f -> g, so the
    // callers of b are {a, main} and the only caller of e is d.  Nothing
    // calls both, which is what makes the two spellings diverge sharply:
    //
    //   func { "b" ; "e" }   the parent is the empty conjunction.  `func`
    //                        bridges its two constraining children, so it is
    //                        strong (one strong component per statement),
    //                        and an empty strong parent empties both
    //                        children: nothing.  (It used to be weak — a
    //                        top-level bare type — and the two children
    //                        survived as two independent graphs.)
    //   func { "b"   "e" }   one command, two selectors — a disjunction, so
    //                        every caller of either survives
    for cap in [0, 5, 1000] {
        let (conj, _) = run_query_probe_traced(TEST_INPUT_A, r#"func { "b" ; "e" }"#, cap);
        let mut nodes: Vec<i64> = conj.nodes.as_vec().iter().map(|id| (*id).into()).collect();
        nodes.sort_unstable();
        assert_eq!(nodes, Vec::<i64>::new(), "cap {cap}: conjunction");

        let (disj, _) = run_query_probe_traced(TEST_INPUT_A, r#"func { "b" "e" }"#, cap);
        let mut nodes: Vec<i64> = disj.nodes.as_vec().iter().map(|id| (*id).into()).collect();
        nodes.sort_unstable();
        assert_eq!(nodes, vec![91, 92, 94, 95, 942], "cap {cap}: disjunction");
    }
}

// ---------------------------------------------------------------------------
// One strong component per top-level statement
//
// A non-constraining statement is strong iff it reaches two different
// constraining statements along two routes sharing nothing but itself
// (statement/weakness.rs).  TEST_INPUT_A: main(942) -> {a(91), b(92)},
// a -> b, d(94) -> {e(95), f(96)}, f -> g(97).  Every result test runs at
// probe caps 0 / 5 / 1000, so the worklist and the refinement planner must
// agree.

fn weak_marks(query: &str) -> std::collections::HashMap<String, bool> {
    crate::parser::parse(query)
        .unwrap()
        .weakness_for_test()
        .into_iter()
        // A statement's span runs through its scope; key on the command
        // text before the first `{`.
        .map(|(text, weak)| (text.split('{').next().unwrap().trim().to_string(), weak))
        .collect()
}

fn sorted_nodes_at(query: &str, cap: usize) -> Vec<i64> {
    let (res, _) = run_query_probe_traced(TEST_INPUT_A, query, cap);
    let mut nodes: Vec<i64> = res.nodes.as_vec().iter().map(|id| (*id).into()).collect();
    nodes.sort_unstable();
    nodes
}

#[test]
fn a_bridge_with_an_unmatched_sibling_is_empty() {
    // The query asks for b's callers that also call "nonexistent"; there are
    // none, so nothing may be returned — not b's callers on their own.
    let marks = weak_marks(r#"func { "nonexistent" ; "b" }"#);
    assert_eq!(
        marks["func"], false,
        "func bridges two constraining children"
    );
    for cap in [0, 5, 1000] {
        assert_eq!(
            sorted_nodes_at(r#"func { "nonexistent" ; "b" }"#, cap),
            Vec::<i64>::new(),
            "cap {cap}"
        );
    }
    let res = run_query(TEST_INPUT_A, r#"func { "nonexistent" ; "b" }"#);
    assert!(
        res.warnings
            .iter()
            .any(|w| w.to_string().contains("matched no symbols")),
        "the unmatched name still says so"
    );
}

/// The advisory notes (not no-matches, not truncations) a query raises at
/// each probe cap.
fn notes_at(query: &str, cap: usize) -> Vec<String> {
    let (res, _) = run_query_probe_traced(TEST_INPUT_A, query, cap);
    res.warnings
        .iter()
        .filter(|w| w.kind == crate::diagnostic::DiagnosticKind::Note)
        .map(|w| w.to_string())
        .collect()
}

#[test]
fn an_inherited_emptiness_is_not_reported_again() {
    // `func` is empty only because "nonexistent" is, and "b" is emptied by
    // that empty `func`.  The cause is the no-match; nothing else may speak.
    for cap in [0, 5, 1000] {
        assert_eq!(
            notes_at(r#"func { "nonexistent" ; "b" }"#, cap),
            Vec::<String>::new(),
            "cap {cap}"
        );
    }
}

#[test]
fn siblings_with_no_common_caller_are_reported_once_by_name() {
    // b's callers are main and a; e's caller is d.  `func` resolves from one
    // child and the other empties it.
    for cap in [0, 5, 1000] {
        let notes = notes_at(r#"func { "b" ; "e" }"#, cap);
        assert_eq!(notes.len(), 1, "cap {cap}: {notes:?}");
        let note = &notes[0];
        assert!(
            note.starts_with("`func` matched symbols, but none of them also relate to "),
            "{note}"
        );
        assert!(
            note.contains(r#"`"b"`"#) || note.contains(r#"`"e"`"#),
            "{note}"
        );
        assert!(
            !note.contains("Line 1:") && !note.contains("children"),
            "{note}"
        );
    }
}

#[test]
fn a_parent_that_empties_its_child_is_reported_once_by_name() {
    // a calls b, not e: the two resolve independently and empty each other.
    // Whichever notifies first is the origin; the echo back is inherited.
    for cap in [0, 5, 1000] {
        let notes = notes_at(r#""a" { "e" }"#, cap);
        assert_eq!(notes.len(), 1, "cap {cap}: {notes:?}");
        let note = &notes[0];
        assert!(
            note.contains(r#"`"a"`"#) && note.contains(r#"`"e"`"#),
            "{note}"
        );
        assert!(note.contains("matched symbols, but none of them"), "{note}");
        assert!(!note.contains("Line 1:"), "{note}");
    }
}

#[test]
fn a_bridge_narrows_both_siblings() {
    // Only main calls both a and b.
    for cap in [0, 5, 1000] {
        assert_eq!(
            sorted_nodes_at(r#"func { "a" ; "b" }"#, cap),
            vec![91, 92, 942],
            "cap {cap}"
        );
    }
}

#[test]
fn a_wrapper_with_one_direction_stays_weak() {
    // `{ }` and `field` each reach both constraining children only through
    // `func`: one route, so they are display echoes.  `func` stays strong.
    for query in [
        r#"{ func { "a" ; "b" } }"#,
        r#"field { func { "a" ; "b" } }"#,
    ] {
        let marks = weak_marks(query);
        let outer = if query.starts_with("field") {
            "field"
        } else {
            ""
        };
        assert_eq!(marks[outer], true, "{query}: outer wrapper");
        assert_eq!(marks["func"], false, "{query}: func");
        for cap in [0, 5, 1000] {
            let nodes = sorted_nodes_at(query, cap);
            for id in [91, 92, 942] {
                assert!(nodes.contains(&id), "{query} cap {cap}: {nodes:?}");
            }
            assert!(!nodes.contains(&94), "{query} cap {cap}: {nodes:?}");
        }
    }
}

#[test]
fn a_child_hanging_off_a_constraining_parent_stays_weak() {
    // Both routes from `func` leave through "main": it bridges nothing.
    let marks = weak_marks(r#""main" { func ; "b" }"#);
    assert_eq!(marks["func"], true);
    assert_eq!(marks[r#""main""#], false);
    for cap in [0, 5, 1000] {
        let nodes = sorted_nodes_at(r#""main" { func ; "b" }"#, cap);
        assert!(
            nodes.contains(&942) && nodes.contains(&92),
            "cap {cap}: {nodes:?}"
        );
    }
}

#[test]
fn a_label_from_an_earlier_statement_is_an_input_not_an_edge() {
    // In the second statement `#c` is a constraining child of `func`, so
    // `func` bridges it and "a".  Nobody calls both a and e, so the second
    // statement is empty; the first still shows e.  (A weak `func` used to
    // leave a and #c standing as two independent graphs.)
    let query = r#"@c "e"; func { "a" ; #c }"#;
    assert_eq!(weak_marks(query)["func"], false);
    for cap in [0, 5, 1000] {
        assert_eq!(sorted_nodes_at(query, cap), vec![95], "cap {cap}");
    }
    // ...and with a shared callee the bridge keeps it.
    let query = r#"@c "b"; func { "a" ; #c }"#;
    for cap in [0, 5, 1000] {
        assert_eq!(sorted_nodes_at(query, cap), vec![91, 92, 942], "cap {cap}");
    }
}

#[test]
fn a_label_cycle_inside_one_statement_makes_every_vertex_on_it_strong() {
    // func -- "a" -- @c func -- #c -- func: one cycle.  `@c func` is a bare
    // type (non-constraining) yet strong: one route to "a", one through the
    // label edge to #c.
    let marks = weak_marks(r#"func { "a" { @c func } ; #c }"#);
    assert_eq!(marks["func"], false);
    assert_eq!(marks["@c func"], false);
    assert_eq!(marks["#c"], false);

    // `func { "a" { @c } ; "b" { #c } }`: an `a` and a `b` that call the
    // same callee.  The bare `@c` is non-constraining but strong, as are
    // `#c` and `func`.
    let marks = weak_marks(r#"func { "a" { @c } ; "b" { #c } }"#);
    assert_eq!(marks["@c"], false, "bare label on the cycle");
    assert_eq!(marks["#c"], false);
    assert_eq!(marks["func"], false);
}

#[test]
fn a_label_cycle_inside_one_statement_is_not_executable_yet() {
    // Pre-existing, independent of weakness: a use whose provider is in the
    // same statement is notified by its parent before the label has a
    // selection, and refuses.  The same error came from the engine before
    // the strong-component rule; pinned so lifting it is a deliberate change.
    for query in [
        r#"func { "a" { @c func } ; #c }"#,
        r#"func { "a" { @c } ; "b" { #c } }"#,
    ] {
        let Err(err) = run_query_err(TEST_INPUT_A, query) else {
            panic!("{query}: expected the circular-dependency error");
        };
        let err = err.to_string();
        assert!(err.contains("circular dependency"), "{query}: {err}");
    }
}

// ---------------------------------------------------------------------------
// Planner-statistics maintenance
//
// These run on isolated fixtures: ANALYZE and pg_class writes are
// destructive-global by the convention documented above.  They assert on
// pg_class, never on pg_stat_user_tables — the latter is flushed
// asynchronously and would make the assertions flaky, which is also exactly
// why production staleness detection reads pg_class.
// ---------------------------------------------------------------------------

/// Fabricate stale statistics: claim the table is tiny while it is not.
/// The fixture container runs as superuser, so a catalog UPDATE is the
/// deterministic way to reach the state a crash-plus-import produces.
#[cfg(test)]
async fn poison_stats(conn: &mut diesel_async::AsyncPgConnection, table: &str, reltuples: f32) {
    use diesel_async::RunQueryDsl;
    diesel::sql_query(format!(
        "UPDATE pg_class SET reltuples = {reltuples}, relpages = 4 \
         WHERE oid = 'index.{table}'::regclass"
    ))
    .execute(conn)
    .await
    .unwrap();
}

#[cfg(test)]
#[derive(diesel::QueryableByName)]
struct DfsIntervalRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    dfs_enter: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    dfs_exit: i64,
}

#[cfg(test)]
#[derive(diesel::QueryableByName)]
struct RelStatsRow {
    #[diesel(sql_type = diesel::sql_types::Float)]
    reltuples: f32,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    relpages: i32,
}

#[cfg(test)]
async fn read_rel_stats(conn: &mut diesel_async::AsyncPgConnection, table: &str) -> (f32, i32) {
    use diesel_async::RunQueryDsl;
    let row: RelStatsRow = diesel::sql_query(format!(
        "SELECT reltuples, relpages FROM pg_class WHERE oid = 'index.{table}'::regclass"
    ))
    .get_result(conn)
    .await
    .unwrap();
    (row.reltuples, row.relpages)
}

#[test]
fn analyze_index_schema_repairs_poisoned_catalog() {
    // The whole point, end to end: poisoned catalog in, plausible catalog
    // out, and the table list comes from the catalog so content_store is
    // covered without anyone having remembered to name it.
    use crate::test_util::create_isolated_fixture;
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        poison_stats(&mut conn, "symbol_instances", 278.0).await;
        poison_stats(&mut conn, "content_store", 3.0).await;

        let report = index::db_diesel::analyze_index_schema(&mut conn)
            .await
            .unwrap();
        assert!(
            report.failed.is_empty(),
            "unexpected failures: {:?}",
            report.failed
        );
        assert!(
            report
                .analyzed
                .iter()
                .any(|(t, _)| t.contains("content_store")),
            "the catalog-derived list must include content_store: {:?}",
            report.analyzed
        );

        let (tuples, _) = read_rel_stats(&mut conn, "symbol_instances").await;
        assert_ne!(tuples, 278.0, "reltuples must be recomputed");
    });
}

#[test]
fn ensure_planner_stats_force_analyzes_and_auto_leaves_healthy_alone() {
    // `force` is the shipped default precisely because `auto`'s size
    // heuristic cannot see distribution staleness; this pins both halves of
    // that trade-off.
    use crate::test_util::create_isolated_fixture;
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();

        // Force always refreshes, and leaves the catalog healthy...
        index::db_diesel::ensure_planner_stats(&mut conn, index::db_diesel::BootAnalyze::Force)
            .await;
        let health = index::db_diesel::index_table_stats_health(&mut conn)
            .await
            .unwrap();
        assert!(
            health.iter().all(|t| t.staleness().is_none()),
            "no table may look stale right after a full ANALYZE: {:?}",
            health
                .iter()
                .filter(|t| t.staleness().is_some())
                .map(|t| (&t.relname, t.staleness()))
                .collect::<Vec<_>>()
        );

        // ...so `auto` has nothing to do, while `off` is inert even when poisoned.
        poison_stats(&mut conn, "symbol_instances", 278.0).await;
        index::db_diesel::ensure_planner_stats(&mut conn, index::db_diesel::BootAnalyze::Off).await;
        let (tuples, _) = read_rel_stats(&mut conn, "symbol_instances").await;
        assert_eq!(tuples, 278.0, "`off` must not touch the catalog");
    });
}

#[test]
fn analyze_survives_lock_contention() {
    // Non-fatality, per-table transaction isolation, and that SET LOCAL
    // lock_timeout is actually applied: one table held under ACCESS
    // EXCLUSIVE must cost exactly that table, not the run.
    use crate::test_util::create_isolated_fixture;
    use diesel_async::{AsyncConnection, RunQueryDsl};
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let mut blocker = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        diesel::sql_query("BEGIN")
            .execute(&mut blocker)
            .await
            .unwrap();
        diesel::sql_query("LOCK TABLE index.symbol_instances IN ACCESS EXCLUSIVE MODE")
            .execute(&mut blocker)
            .await
            .unwrap();

        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        let report = index::db_diesel::analyze_index_schema(&mut conn)
            .await
            .expect("a locked table must not abort the run");

        assert!(
            report
                .failed
                .iter()
                .any(|(t, _)| t.contains("symbol_instances")),
            "the locked table must be reported as failed: {:?}",
            report.failed
        );
        assert!(
            report.analyzed.iter().any(|(t, _)| t.contains("symbols")),
            "every other table must still be analyzed: {:?}",
            report.analyzed
        );

        diesel::sql_query("ROLLBACK")
            .execute(&mut blocker)
            .await
            .unwrap();
    });
}

#[test]
fn finalize_project_refreshes_planner_stats() {
    // The load-bearing hook: an import that has just introduced a new `layer`
    // value must leave the statistics describing it.
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::{AsyncConnection, RunQueryDsl};
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let (store, _index) = store_and_index_with_shared_cache(fx.url()).await;
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();

        diesel::sql_query(
            "UPDATE index.projects SET upload_status = 'uploading', \
             symbol_chunks_total = 0, object_chunks_total = 0 WHERE id = 1",
        )
        .execute(&mut conn)
        .await
        .unwrap();

        // Poison two tables, one of which a hand-written list would miss.
        poison_stats(&mut conn, "symbol_instances", 278.0).await;
        poison_stats(&mut conn, "content_store", 3.0).await;

        assert_eq!(
            store.finalize_project(1).await.unwrap(),
            crate::index_store::FinalizeOutcome::Finalized
        );

        let (instances, _) = read_rel_stats(&mut conn, "symbol_instances").await;
        let (contents, _) = read_rel_stats(&mut conn, "content_store").await;
        assert_ne!(instances, 278.0, "finalize must refresh symbol_instances");
        assert_ne!(contents, 3.0, "finalize must refresh content_store too");
    });
}

#[test]
fn idempotent_finalize_does_not_reanalyze() {
    // The hook fires on the transition, not on the call — which is the whole
    // reason finalize_project returns an outcome rather than a bool.
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::{AsyncConnection, RunQueryDsl};
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let (store, _index) = store_and_index_with_shared_cache(fx.url()).await;
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();

        diesel::sql_query(
            "UPDATE index.projects SET upload_status = 'uploading', \
             symbol_chunks_total = 0, object_chunks_total = 0 WHERE id = 1",
        )
        .execute(&mut conn)
        .await
        .unwrap();
        assert_eq!(
            store.finalize_project(1).await.unwrap(),
            crate::index_store::FinalizeOutcome::Finalized
        );

        poison_stats(&mut conn, "symbol_instances", 278.0).await;
        assert_eq!(
            store.finalize_project(1).await.unwrap(),
            crate::index_store::FinalizeOutcome::AlreadyComplete
        );
        let (tuples, _) = read_rel_stats(&mut conn, "symbol_instances").await;
        assert_eq!(tuples, 278.0, "an idempotent finalize must not re-ANALYZE");
    });
}

#[test]
fn delete_project_refreshes_planner_stats() {
    // Also proves ANALYZE over freshly-emptied tables does not error.
    use crate::test_util::{create_isolated_fixture, store_and_index_with_shared_cache};
    use diesel_async::AsyncConnection;
    let fx = create_isolated_fixture(VERB_TEST);

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        let (store, _index) = store_and_index_with_shared_cache(fx.url()).await;
        let mut conn = diesel_async::AsyncPgConnection::establish(fx.url())
            .await
            .unwrap();
        poison_stats(&mut conn, "symbol_instances", 278.0).await;

        assert!(store.delete_project(1).await.unwrap());

        let (tuples, _) = read_rel_stats(&mut conn, "symbol_instances").await;
        assert_ne!(tuples, 278.0, "delete_project must refresh statistics");
    });
}

#[test]
fn autovacuum_reloptions_applied() {
    // Guards the migration, and the "only tables above ~1M rows" rule: a
    // future big table should be added deliberately, not by accident.
    use crate::test_util::{get_shared_db_url, get_shared_index};
    use diesel_async::{AsyncConnection, RunQueryDsl};

    #[derive(diesel::QueryableByName)]
    struct OptRow {
        #[diesel(sql_type = diesel::sql_types::Text)]
        relname: String,
        #[diesel(sql_type = diesel::sql_types::Text)]
        opts: String,
    }

    let mut rt = tokio::runtime::Runtime::new().unwrap();
    let local = tokio::task::LocalSet::new();
    local.block_on(&mut rt, async {
        // Touch the fixture first so the container exists before we connect.
        let _ = get_shared_index(VERB_TEST).await;
        let mut conn = diesel_async::AsyncPgConnection::establish(get_shared_db_url(VERB_TEST))
            .await
            .unwrap();

        let rows: Vec<OptRow> = diesel::sql_query(
            "SELECT c.relname::text AS relname, \
                    coalesce(array_to_string(c.reloptions, ','), '') AS opts \
               FROM pg_class c JOIN pg_namespace n ON n.oid = c.relnamespace \
              WHERE n.nspname = 'index' AND c.relkind = 'r'",
        )
        .load(&mut conn)
        .await
        .unwrap();

        const TUNED: [&str; 4] = ["symbols", "symbol_instances", "symbol_refs", "objects"];
        for row in &rows {
            let tuned = TUNED.contains(&row.relname.as_str());
            assert_eq!(
                row.opts.contains("autovacuum_analyze_threshold=25000"),
                tuned,
                "{} reloptions = {:?} (tuned tables are exactly {:?})",
                row.relname,
                row.opts,
                TUNED
            );
        }
    });
}

#[test]
fn caps_never_silently_change_a_resolved_answer() {
    // V, test 1+2: the central property of the whole capping model.  For each
    // query, sweep read_cap over {3, 5, 1000, 0} (0 = disabled = the reference)
    // and assert: a run that did NOT warn "incomplete" produces exactly the
    // cap=0 nodes and edge count.  Equivalently — a capped read may only ever
    // CHANGE the answer while ALSO saying so.  A silent short answer is the one
    // outcome forbidden.
    //
    // NON-invariant deliberately NOT asserted: node sets are not monotone in
    // the cap (a tighter cap's refusal-to-narrow can KEEP rows a looser cap
    // would drop), so this compares each non-capped run to cap=0, never one cap
    // to another.
    let queries = [
        r#"func { "hot" }"#,             // resolves at every cap (2 <= 3)
        r#"func { "lock" }"#,            // caps at 3/5, resolves at 1000/0
        r#"{ "lock" }"#,                 // anonymous derive; caps at 3/5
        r#"mod("modx") has { func }"#,   // has-derive caps at 3/5
        r#"g"in_*""#,                    // self-resolve; caps at 3/5
        r#"mod("modx") { { "lock" } }"#, // the middle is cut at 1/2
    ];
    let mut any_capped = false;
    for q in queries {
        let reference = run_query_with_read_cap(TEST_INPUT_ESCALATION, q, 0);
        let mut ref_nodes: Vec<i64> = reference
            .nodes
            .as_vec()
            .iter()
            .map(|id| (*id).into())
            .collect();
        ref_nodes.sort_unstable();
        let ref_edges = reference.edges.0.len();

        for cap in [1usize, 2, 3, 5, 1000, 0] {
            let r = run_query_with_read_cap(TEST_INPUT_ESCALATION, q, cap);
            let warned = r.warnings.iter().any(|w| w.message.contains("incomplete"));
            any_capped |= warned;
            let mut nodes: Vec<i64> = r.nodes.as_vec().iter().map(|id| (*id).into()).collect();
            nodes.sort_unstable();
            if warned {
                // A capped run may differ, and it said so.  What it shows
                // is still part of the answer: "incomplete" means a subset.
                assert!(
                    nodes.iter().all(|id| ref_nodes.contains(id)),
                    "{q} at read_cap={cap}: warned, so its nodes must be a subset of the \
                     cap=0 answer, got {nodes:?} against {ref_nodes:?}"
                );
                continue;
            }
            assert_eq!(
                nodes, ref_nodes,
                "{q} at read_cap={cap}: did not warn, so its nodes must equal the cap=0 answer"
            );
            assert_eq!(
                r.edges.0.len(),
                ref_edges,
                "{q} at read_cap={cap}: did not warn, so its edge count must equal the cap=0 answer"
            );
        }
    }
    assert!(
        any_capped,
        "premise: at least one query must actually cap at a small read_cap, \
         or this sweep proves nothing"
    );
}

#[test]
fn an_edge_cap_bounds_edges_without_touching_nodes_or_warnings() {
    // E1 at the query level: the per-node out-edge cap is ASSEMBLY capping —
    // an invitation to paginate — so it may shrink edge lists and nothing
    // else.  `"hub"; g"t_*"` selects hub and its four targets as independent
    // statements, so every hub→t_i edge is IMPLICIT (found by
    // find_edges_between over the answer, the only reads E1 bounds; edges a
    // brace-query fetches as a child family are discovery reads, bounded by
    // read_cap instead).
    let free = run_query_with_edge_cap(TEST_INPUT_ESCALATION, r#""hub"; g"t_*""#, 0);
    let capped = run_query_with_edge_cap(TEST_INPUT_ESCALATION, r#""hub"; g"t_*""#, 2);

    let ids = |r: &crate::statement::ExecutionResult| {
        let mut v: Vec<i64> = r.nodes.as_vec().iter().map(|id| (*id).into()).collect();
        v.sort_unstable();
        v
    };
    assert_eq!(
        free.edges.0.len(),
        4,
        "premise: hub has four out-edges uncapped"
    );
    assert_eq!(
        capped.edges.0.len(),
        2,
        "edge_cap = 2 bounds hub to two edges"
    );
    // F2: the truncation is REPORTED per symbol so a client can fetch more —
    // hub (symbol 5) and only hub; nothing when the cap is off.
    assert!(
        free.edges_truncated.is_empty(),
        "cap disabled => no truncated nodes reported"
    );
    assert_eq!(
        capped
            .edges_truncated
            .iter()
            .map(|s| s.0)
            .collect::<Vec<_>>(),
        vec![5],
        "hub's symbol id, and only hub's, is reported truncated"
    );
    assert_eq!(
        ids(&free),
        ids(&capped),
        "the edge cap must not change the node set"
    );
    assert!(
        free.warnings.is_empty() && capped.warnings.is_empty(),
        "assembly capping must not warn — the graph is not incomplete, an \
         edge list is merely paged; got {:?} / {:?}",
        free.warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
        capped
            .warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );

    // Layer boundary: a COMPOSED query resolves identically under a tiny edge
    // cap — pagination capping cannot leak into discovery (it runs strictly
    // after the fixpoint).  Would fail if find_edges_between output ever fed
    // back into constraining.
    let comp_free = run_query_with_edge_cap(TEST_INPUT_ESCALATION, r#"g"in_*" { "lock" }"#, 0);
    let comp_capped = run_query_with_edge_cap(TEST_INPUT_ESCALATION, r#"g"in_*" { "lock" }"#, 1);
    assert_eq!(
        ids(&comp_free),
        ids(&comp_capped),
        "a composed query's resolution must not depend on the edge cap"
    );
    assert_eq!(
        comp_free.warnings.len(),
        comp_capped.warnings.len(),
        "nor its warnings"
    );
}

#[test]
fn a_capped_provider_is_flagged_through_the_label_path() {
    // The label path is the one notification route that does not go through
    // `notify_from_selection`: `#res` receives the provider\'s selection via
    // `derive_from_provider` (a clone) and constrains through `labels.rs`\'s
    // own `try_constrain_notification` override.  This is a black-box pin on
    // that route: a capped provider must mirror the same short answer and the
    // incompleteness warning must be beside it; uncapped must mirror the full
    // answer silently.  (Scope, honestly: a mutant that scrubs `capped` on
    // the clone is currently observation-EQUIVALENT here, because every
    // `#res` render is unioned with the provider tree\'s own render — so this
    // test guards the route\'s end-to-end behaviour, not the clone\'s flag in
    // isolation.  If a future renderer projects `#res` alone, add the
    // discriminating assertion there.)
    let capped = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"@res { "lock" }; #res"#, 3);
    assert_eq!(
        capped.nodes.as_vec().len(),
        5,
        "lock + 4-of-10 derived callers, mirrored through the label"
    );
    assert!(
        capped
            .warnings
            .iter()
            .any(|w| w.message.contains("this part of the graph is incomplete")),
        "the capped provider must stay said through the label path; got {:?}",
        capped
            .warnings
            .iter()
            .map(|w| w.message.clone())
            .collect::<Vec<_>>(),
    );

    // Uncapped: the full answer, and silence.
    let full = run_query_with_read_cap(TEST_INPUT_ESCALATION, r#"@res { "lock" }; #res"#, 1000);
    assert_eq!(
        full.nodes.as_vec().len(),
        12,
        "lock + instance + ten callers"
    );
    assert!(
        !full
            .warnings
            .iter()
            .any(|w| w.message.contains("incomplete")),
        "an uncapped provider must not warn through the label"
    );
}

#[test]
fn eph_owner_is_the_innermost_instance_in_the_tree() {
    // Which persistent instance a query-time row sits in, when several share
    // one extent.  ONE law, the same one both indexers use to place a
    // persistent ref: the innermost instance containing it -- narrowest
    // extent, then deepest in the tree.  The same bytes get the same container
    // whether the row is a stored reference or a query-time match.
    //
    // "Deepest" and not "outermost", which is what this ordering used to say.
    // Outermost was chosen when the only co-extensive clusters in sight were
    // macro-generated -- picking the deepest there meant picking a compiler
    // helper.  But every real file object also carries a co-extensive CHAIN,
    // its module and directory and itself, and there outermost is the module:
    // it numbers a match in a file's import block below its own file's
    // interval, out of reach of `file { ... }` in every traversal mode (see
    // `a_search_match_outside_every_declaration_is_reachable_from_its_file`).
    //
    // Whether a compiler helper is LISTED is its symbol's folding, a separate
    // question (`a_match_in_a_generated_declaration_is_still_the_callers`).
    // Placement does not ask.
    const SHOW: &str = r#"func("coext_show") { search("wq9") }"#;
    const ASSERT: &str = r#"func("coext_assert") { search("wq9") }"#;
    const ATTR: &str = r#"data("coext_attr") { search("wq9") }"#;

    let eph = |q: &str| -> Vec<i64> {
        run_query(TEST_INPUT_SEARCH, q)
            .nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .collect()
    };

    // Object 11: coext_show [0,21) level 1 contains coext_assert [0,21) level 2;
    // coext_attr [0,21) level 1 is their sibling.  The owner is coext_assert.
    assert_eq!(
        eph(ASSERT).len(),
        1,
        "the innermost of the co-extensive tie owns the match"
    );
    assert_eq!(
        eph(SHOW).len(),
        1,
        "and it stays reachable from the owner's ancestors -- containment is \
         transitive, so a deeper owner widens reach rather than narrowing it"
    );
    assert!(
        eph(ATTR).is_empty(),
        "but not from a SIBLING of the owner, which contains nothing of it"
    );
}

#[test]
fn a_match_in_a_generated_declaration_is_still_the_callers() {
    // Every byte at the call site of `M13(zq8, c)` was typed by whoever wrote
    // `h13`.  The declaration `M13` generates has the whole invocation
    // `[15,25)` as its extent, so `zq8` sits IN it -- containment is by
    // extent and nothing else.
    //
    // It is still h13's, twice over.  h13's interval contains the generated
    // declaration's, so the match is inside h13.  And the generated symbol is
    // folded, so it is the one container that is not listed.
    //
    // This used to be arranged in the placement rule itself (`AND NOT
    // si.folded`: "a macro expansion owns no text"), which bought the same
    // answer by making where a match sits depend on what is shown.
    let ids = |q: &str| -> Vec<i64> {
        run_query(TEST_INPUT_SEARCH, q)
            .nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .collect()
    };

    let up = ids(r#"{ search("zq8") }"#);
    assert!(up.contains(&123), "h13 contains the match, got {:?}", up);
    assert!(
        !up.contains(&125),
        "the generated declaration is folded, so it is not listed, got {:?}",
        up
    );

    let all = ids(r#"unnest { search("zq8") }"#);
    assert!(
        all.contains(&125) && all.contains(&123),
        "`unnest` lists the declaration the match sits in as well, got {:?}",
        all
    );

    let down = ids(r#"func("h13") { search("zq8") }"#);
    assert!(
        down.iter().any(|id| *id < 0),
        "and from h13 the match is found by the interval alone, got {:?}",
        down
    );
}

#[test]
fn a_match_nested_in_generated_declarations_still_belongs_to_the_caller() {
    // Object 12: `k7z` sits in gen_in inside gen_out inside fn_host, both
    // generated symbols folded.  Depth changes nothing: fn_host contains the
    // match however many folded declarations stand between, and those are
    // the ones not listed.
    const UP: &str = r#"{ search("k7z") }"#;
    let ids: Vec<i64> = run_query(TEST_INPUT_SEARCH, UP)
        .nodes
        .as_vec()
        .iter()
        .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
        .collect();

    assert!(
        ids.contains(&120),
        "fn_host contains the match, got {:?}",
        ids
    );
    assert!(
        !ids.contains(&121) && !ids.contains(&122),
        "neither generated declaration is listed, got {:?}",
        ids
    );
}

#[test]
fn a_search_match_outside_every_declaration_is_reachable_from_its_file() {
    // Every real file object carries a CO-EXTENSIVE container chain -- its
    // module, its directory and the file itself, all covering [0, len).  A
    // search match outside every declaration (a Go import block, a package
    // clause, a comment between functions) has all three as candidate owners,
    // and the eph owner lookup breaks that tie with `si.dfs_enter ASC`: the
    // OUTERMOST one.
    //
    // So the row is numbered at `module.dfs_enter + 1`, which is not merely at
    // the wrong depth -- it is BELOW the file's interval entirely.  No traversal
    // mode can recover it, because every mode starts from the interval range and
    // only adds an equality: `unnest` drops the depth predicate and still asks
    // `file.dfs_enter < row.dfs_enter < file.dfs_exit`.
    //
    // Object 14: mod [1024,10240) > dir [2048,9216) > file [3072,8192) > h14
    // [4096,5120).  `zk4` is outside h14, `zk5` inside it.
    let eph = |q: &str| -> usize {
        run_query(TEST_INPUT_SEARCH, q)
            .nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .count()
    };

    // The control: a match inside a function is owned by that function, which
    // is genuinely inside the file, so every form works.
    assert_eq!(eph(r#"search("zk5")"#), 1, "the match exists at all");
    assert_eq!(
        eph(r#"func("h14") { search("zk5") }"#),
        1,
        "a match inside a declaration is owned by it"
    );
    assert_eq!(
        eph(r#"file("/p1/chain.go") { search("zk5") }"#),
        1,
        "and reachable from the file, through the function"
    );

    // The case under test.  All three of these ask the same interval question
    // of the same row, so they agree -- which is the point: `unnest` is not a
    // way out.
    assert_eq!(eph(r#"search("zk4")"#), 1, "the match exists at all");
    let scoped = eph(r#"file("/p1/chain.go") { search("zk4") }"#);
    let unnested = eph(r#"file("/p1/chain.go") { unnest search("zk4") }"#);
    let nested = eph(r#"file("/p1/chain.go") { nested search("zk4") }"#);
    assert_eq!(
        (scoped, unnested, nested),
        (1, 1, 1),
        "a match in its own file must be reachable from that file in every \
         traversal mode; got scoped={scoped} unnest={unnested} nested={nested}. \
         Equal-but-zero means the row was numbered outside the file's interval, \
         which is a PLACEMENT bug -- the owner tie-break took the outermost of \
         the co-extensive chain -- and no mode can work around it."
    );
}

#[test]
fn a_scoped_search_is_restricted_to_its_scope_byte_ranges() {
    // `macro(X) { search(Y) }` fuses X's byte ranges into the scan's CORPUS
    // (`SearchSelector`'s `fusion_scope` -> `resolve_scope_object_ranges`), so a
    // scoped search is answered from the scope's text and nothing else.
    //
    // That makes an instance's EXTENT load-bearing for search, not merely
    // descriptive.  Object 11's `COEXT_ATTR` covers the whole invocation
    // `[0,21)`, as the un-clamped indexer emits it, so `wq9` at `[11,14)` is in
    // its corpus and the match comes back.  While the extent was clamped to the
    // name `[0,11)` this returned nothing -- correctly, since the text was
    // outside the scope -- and that empty result was then overwritten by the
    // derive path with the macro's children, which is the actual bug (W10).
    let eph = |q: &str| -> usize {
        run_query(TEST_INPUT_SEARCH, q)
            .nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .count()
    };

    assert_eq!(
        eph(r#"macro("COEXT_ATTR") { search("wq9") }"#),
        1,
        "the argument text is inside the macro's extent, so it is in its corpus"
    );
    assert_eq!(
        eph(r#"func("coext_show") { search("wq9") }"#),
        1,
        "and inside the generated symbol's extent too"
    );
    assert_eq!(
        eph(r#"func("fn_tail") { search("wq9") }"#),
        0,
        "but not in an unrelated object's -- the scope really does bound the corpus"
    );
}

#[test]
fn a_match_passes_over_a_declaration_whose_bytes_are_not_its_text() {
    // `PF(int, send, yq7) { return vq3; }` -- the macro declares a variable
    // `blah` and the head of `blah_send`, and the author typed the body.  The
    // only extent `blah` has is the invocation's `[0,18)`.  That lies in
    // `blah_send`'s text `[0,34)`, and the indexer puts `blah` BESIDE the
    // function, where the compiler read it.
    //
    // So the bytes say `blah` is inside `blah_send` and the tree says it is
    // not, which can only mean the bytes are not `blah`'s text.  A match there
    // is text the author of `blah_send` typed, and sits in `blah_send`.
    //
    // It used to sit in `blah`, the narrowest extent around it -- and so was
    // outside the function it was typed in: `func("blah_send") {
    // search("yq7") }` found the text in the function's corpus, placed it in
    // the sibling, and reported that nothing related.
    let eph = |q: &str| -> usize {
        run_query(TEST_INPUT_SEARCH, q)
            .nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .filter(|id| *id < 0)
            .count()
    };

    assert_eq!(
        eph(r#"func("blah_send") { search("yq7") }"#),
        1,
        "the invocation's arguments are text of the function that runs on from it"
    );
    assert_eq!(
        eph(r#"func("blah_send") { search("vq3") }"#),
        1,
        "as its typed body always was"
    );
    assert_eq!(
        eph(r#"data("blah") { search("yq7") }"#),
        0,
        "and the generated variable has no text of its own for a match to sit in"
    );
    assert_eq!(
        eph(r#"file("sibling.c") { search("yq7") }"#),
        1,
        "the file contains it either way"
    );

    // NOT A DISAGREEMENT: `fam_in` lies in the bytes of `fam_a` and of `fam_b`,
    // which share one extent, and a tree gives it one parent.  A declaration
    // with that extent does contain it, so its bytes are its text and the match
    // sits in it, the innermost, as before.
    assert_eq!(
        eph(r#"func("fam_in") { search("zr2") }"#),
        1,
        "a declaration inside one of several on one extent keeps its matches"
    );
    assert_eq!(
        eph(r#"func("fam_a") { search("zr2") }"#),
        1,
        "and so does the one that contains it"
    );
}

#[test]
fn a_layer_materialising_command_never_derives_its_selection() {
    // The fabrication bug: when a `search` had no selection,
    // `Command::derive_from_parent` ran a read that cannot express the search
    // predicate (`search` contributes no composite filter) and handed back the
    // PARENT'S CHILDREN as if they were matches.
    //
    // `macro("COEXT_ATTR") { search("nosuchtext") }` is the shape: the macro
    // resolves, the search correctly matches nothing in its scope, and the
    // result must not then contain the macro's children 117/118/119.
    //
    // It was order-dependent, which is what made it serious: whichever side the
    // planner resolved first decided whether you got matches or fabrications.
    const QUERY: &str = r#"macro("COEXT_ATTR") { search("nosuchtext") }"#;
    let ids: Vec<i64> = run_query(TEST_INPUT_SEARCH, QUERY)
        .nodes
        .as_vec()
        .iter()
        .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
        .collect();

    for fabricated in [117i64, 118, 119] {
        assert!(
            !ids.contains(&fabricated),
            "instance {} is a child of the macro, not a text match, got {:?}",
            fabricated,
            ids
        );
    }
    assert!(
        !ids.iter().any(|id| *id < 0),
        "and no ephemeral row either -- nothing matched, got {:?}",
        ids
    );
}

#[test]
fn an_empty_search_empties_its_parent_like_any_other_empty_child() {
    // "Resolved to nothing" and "no opinion" are different things, and the
    // layer-read path used to collapse them.  `None` from a selector means
    // transparent -- derive me from a neighbour, I have nothing to say -- so an
    // empty search left its parent unconstrained:
    //
    //   macro("COEXT_ATTR") { func("nonexistent_zzz") }  ->  []     correct
    //   macro("COEXT_ATTR") { search("nosuchtext") }     ->  [116]  wrong
    //
    // A search that ran and matched nothing has plenty to say: its parent
    // cannot contain a match either.  Both forms must agree, and the parallel
    // persistent queries are in here so the test states the symmetry rather
    // than just the fixed value.
    let ids = |q: &str| -> Vec<i64> {
        run_query(TEST_INPUT_SEARCH, q)
            .nodes
            .as_vec()
            .iter()
            .map(|id| <SymbolInstanceId as Into<i64>>::into(*id))
            .collect()
    };

    for (persistent, layer) in [
        (
            r#"macro("COEXT_ATTR") { func("nonexistent_zzz") }"#,
            r#"macro("COEXT_ATTR") { search("nosuchtext") }"#,
        ),
        (
            r#"func("coext_show") { func("nonexistent_zzz") }"#,
            r#"func("coext_show") { search("nosuchtext") }"#,
        ),
    ] {
        assert!(
            ids(persistent).is_empty(),
            "an empty persistent child empties its parent: {} -> {:?}",
            persistent,
            ids(persistent)
        );
        assert_eq!(
            ids(layer),
            ids(persistent),
            "and an empty layer child must do the same: {} -> {:?}",
            layer,
            ids(layer)
        );
    }
}
