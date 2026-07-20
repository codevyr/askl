use crate::test_util::{
    format_edges, get_shared_db_url, get_shared_index, run_query, run_query_err, run_query_traced, TEST_INPUT_A, TEST_INPUT_B, TEST_INPUT_CONTAINMENT, TEST_INPUT_MODULES, TEST_INPUT_NESTED_FUNC, TEST_INPUT_SEARCH, TEST_INPUT_TREE_BROWSER, VERB_TEST,
};
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
    assert_eq!(edges, vec![
        "91-92", "91-92", "942-91", "942-92",
        "1001-91", "1001-92", "1001-92", "1001-92",
        "1003-91", "1003-92", "1003-92", "1003-92",
    ]);
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
    assert_eq!(edges, vec!["96-97", "1001-96", "1001-97", "1003-96", "1003-97"]);
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
    assert_eq!(edges, vec!["96-97", "1001-96", "1001-97", "1003-96", "1003-97"]);
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
    // mod("test", filter="true", inherit="true") acts as a namespace filter
    // that propagates into child scopes via inherit="true".
    const QUERY: &str = r#"mod("test", filter="true", inherit="true") {{"b"}}"#;
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
}

#[test]
fn module_filter_excludes_other_modules() {
    const FILTERED_QUERY: &str = r#"mod("test", filter="true") "a""#;
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

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"mod("test", filter="true") "a"; "a""#;
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

    const PREAMBLE_FILTERED_QUERY: &str = r#"preamble mod("test", filter="true", inherit="true"); "a""#;
    let preamble_filtered = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered.nodes.as_vec();

    assert_eq!(preamble_filtered_nodes, vec![SymbolInstanceId::new(91)]);
}

#[test]
fn module_filter_selects_other_module() {
    const QUERY: &str = r#"mod("other", filter="true") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn module_filter_replaced_by_second_invocation() {
    const QUERY: &str = r#"mod("test", filter="true") mod("other", filter="true") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn module_filter_children_scope_honors_filter() {
    const QUERY: &str = r#"mod("other", filter="true") "a" {}"#;
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
    const QUERY: &str = r#"project("test_project") mod("other", filter="true") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn conflicting_project_and_module_filters_return_empty() {
    const QUERY: &str = r#"project("other_project") mod("other", filter="true") "a""#;
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
    assert_eq!(edges.len(), 2, "Both edges with different offsets should be preserved");
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
    assert_eq!(edges.len(), 1, "Duplicate edges with same offset should be deduplicated");
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
    const QUERY: &str = r#""a"{;;;;;preamble}"#;
    let res = run_query_err(TEST_INPUT_A, QUERY);

    assert_eq!(res.is_err(), true);
    if let Err(e) = res {
        println!("{:#?}", e);
        assert!(e
            .to_string()
            .contains("Preamble verb can only be used as the first verb"));
    }
}

#[test]
fn preamble_isolated_scope() {
    const QUERY: &str = r#"preamble scope(isolated="true")"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
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
    assert_eq!(res.warnings.len(), 2);
}

#[test]
fn non_existent_child_warning() {
    const QUERY: &str = r#""a" { {"a"} }"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    assert_eq!(res.nodes.as_vec(), vec![]);
    println!("{:#?}", res.warnings);
    assert_eq!(res.warnings.len(), 2);
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
fn has_children_query() {
    // mod("testmodule") has { file has { "foo" } }
    // Bare `file` (no name) now inherits FILE type filter into children (inherit=true).
    // "foo" inherits the FILE type filter, but foo is a FUNCTION — type mismatch.
    // "foo" (strong) finds nothing → constrains file away → constrains mod away.
    // Result: 0 nodes.
    const QUERY: &str = r#"mod("testmodule") has { file has { "foo" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn has_parents_query() {
    // file has { "foo" }
    // Bare `file` (no name) now inherits FILE type filter into children (inherit=true).
    // "foo" inherits the FILE type filter, but foo is a FUNCTION — type mismatch.
    // "foo" (strong) finds nothing → constrains file away. Result: 0 nodes.
    const QUERY: &str = r#"file has { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn mixed_has_refs_query() {
    // mod("testmodule") has { file has { "foo" refs {} } }
    // Bare `file` (no name) now inherits FILE type filter into children (inherit=true).
    // "foo" inherits the FILE type filter, but foo is a FUNCTION — type mismatch.
    // "foo" (strong) finds nothing → constrains file away → constrains mod away.
    // Result: 0 nodes.
    const QUERY: &str = r#"mod("testmodule") has { file has { "foo" refs {} } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 0);
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
    // func (no name) at root acts as filter - returns empty
    // because there's no parent to derive from
    const QUERY: &str = r#"func"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Filter at root level = empty
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn type_selector_function_explicit_select_all() {
    // func(filter="false")
    // Explicitly select all functions (override default filter behavior)
    const QUERY: &str = r#"func(filter="false")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have foo (20), bar (30), baz (40)
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
    // file has { "foo" refs { "bar" } }
    // Bare `file` (no name) now inherits FILE type filter into children (inherit=true).
    // "foo" inherits the FILE type filter, but foo is a FUNCTION — type mismatch.
    // "foo" (strong) finds nothing → constrains file away. Result: 0 nodes.
    const QUERY: &str = r#"file has { "foo" refs { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec().len(), 0);
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
    assert!(nodes.contains(&SymbolInstanceId::new(10)), "Should include module");

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
    assert!(nodes.contains(&SymbolInstanceId::new(10)), "Should include parent module");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "Should include bar");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "Should include baz");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "Should include foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "Should include bar");

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
    // file (no name) at root acts as filter - returns empty
    // because there's no parent to derive from
    const QUERY: &str = r#"file"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Filter at root level = empty
    assert_eq!(res.nodes.as_vec().len(), 0);
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
    // dir (no name) at root acts as filter - returns empty
    // because there's no parent to derive from
    const QUERY: &str = r#"dir"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Filter at root level = empty
    assert_eq!(res.nodes.as_vec().len(), 0);
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
    // Bare `file` (no name, inherit=true) doesn't affect selection since file is a leaf.
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
    // Bare `file` (no name, inherit=true) doesn't affect selection since file is a leaf.
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
fn has_sibling_children_different_types() {
    // dir("/") has { mod ; file }
    // Test that sibling children of different types use UNION logic.
    // Directory "/" contains both modules and files via different instances.
    // Both sibling children should be found.
    const QUERY: &str = r#"dir("/") has { mod ; file }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Directory "/" should contain:
    // - module (testmodule) via one instance
    // - file (/main.go) via possibly another instance
    // The parent directory should NOT be filtered out by the union of both children
    assert!(
        res.nodes.as_vec().len() >= 2,
        "dir('/') has {{ mod ; file }} should return directory + module + file (union of both). Got {} nodes.",
        res.nodes.as_vec().len()
    );
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
    assert!(names.contains(&"/src/util"), "Should contain child /src/util");
    assert!(names.contains(&"/src/config"), "Should contain child /src/config");
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
    // filter("type", "func") — filter only, no selection (like bare func)
    // At root level with no parent to derive from, filter-only returns empty
    const QUERY: &str = r#"filter("type", "func")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    // Filter at root = empty (no selector to drive selection)
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn generic_filter_compound_name_inherit() {
    // filter("compound_name", "test", inherit="true") {{"b"}}
    // Namespace filter inherited through double parent.
    // The "test" compound name filter is inherited into child scopes,
    // constraining grandchildren to also match "test" in their name search.
    // Without the filter, "b" matches both test.b (92) and other.b (202).
    // With the filter, only test.* symbols survive.
    const QUERY: &str = r#"filter("compound_name", "test", inherit="true") {{"b"}}"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(res.warnings.is_empty(), "Should produce no warnings");
    // Grandchild "b" matches test.b (92). Intermediate derives callers of test.b:
    // test.a (91), test.c (93), test.main (942). Inherited "test" filter excludes other.*.
    assert_eq!(nodes.len(), 4);
    assert!(nodes.contains(&SymbolInstanceId::new(92)), "test.b");
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "test.a (caller of test.b)");
    assert!(nodes.contains(&SymbolInstanceId::new(93)), "test.c (caller of test.b)");
    assert!(nodes.contains(&SymbolInstanceId::new(942)), "test.main (caller of test.b)");
    // Verify the filter actually excluded other.* symbols
    assert!(!nodes.contains(&SymbolInstanceId::new(201)), "other.a should be excluded");
    assert!(!nodes.contains(&SymbolInstanceId::new(202)), "other.b should be excluded");
}

#[test]
fn generic_filter_type_replacement() {
    // filter("type", "func") filter("type", "mod") — second replaces first (same kind tag)
    // The final type filter should be "mod" only.
    const QUERY: &str =
        r#"filter("type", "func") filter("type", "mod") filter("compound_name", "testmodule") select"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    // Should find module "testmodule" (10) — func filter was replaced by mod filter
    assert_eq!(res.nodes.as_vec().len(), 1);
    assert!(res.nodes.as_vec().contains(&SymbolInstanceId::new(10)));
}

#[test]
fn generic_filter_type_comma_separated() {
    // filter("type", "func,mod") — OR semantics for multiple types
    const QUERY: &str = r#"filter("type", "func,mod") filter("compound_name", "testmodule") select"#;
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
    // filter("type", "func") alone — no selector, UnitVerb added, statement is weak
    const QUERY: &str = r#"filter("type", "func")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    assert_eq!(res.nodes.as_vec().len(), 0);
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
fn generic_select_alone_warns() {
    // select alone (no name filter) — should return warning
    const QUERY: &str = r#"select"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.warnings);
    // Should have a warning about missing name constraint
    assert!(
        res.warnings.len() >= 1,
        "Expected warning about missing name filter for select"
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
        res_generic.nodes.as_vec().contains(&SymbolInstanceId::new(10)),
        "GenericSelector should find module (queries all types)"
    );
    assert!(
        res_plain.nodes.as_vec().contains(&SymbolInstanceId::new(10)),
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
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "Should include bar");
    assert!(!nodes.contains(&SymbolInstanceId::new(20)), "Should NOT include foo (replaced by bar)");
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
    // derive(type="has", inherit="true") propagates to grandchildren
    // file derive(type="has", inherit="true") { { "foo" } }
    // Without inherit, the grandchild {} would reset to refs, but with inherit
    // the grandchild also uses has semantics
    // file has→ ??? has→ foo
    // Since file contains foo directly, the intermediate {} derives contained functions,
    // then the inner "foo" also uses has to constrain
    const QUERY: &str = r#"file("/main.go") derive(type="has", inherit="true") { func { "foo" } }"#;
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
    // With explicit inherit="false", grandchild resets to refs
    // file derive(type="has", inherit="false") { "foo" { "bar" } }
    // file has→ foo refs→ bar (grandchild resets to refs)
    const QUERY: &str = r#"file("/main.go") derive(type="has", inherit="false") { "foo" { "bar" } }"#;
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
    assert!(names.contains(&"/src/util/util.go"), "Should contain util.go");
    assert!(names.contains(&"/src/util/helper.go"), "Should contain helper.go");
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
    assert!(names.contains(&"/src/util"), "Should contain child dir /src/util");
    assert!(names.contains(&"/src/config"), "Should contain child dir /src/config");
    assert!(names.contains(&"/src/main.go"), "Should contain child file /src/main.go");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "Should include foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "Should include bar");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo (calls bar)");
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
        vec![SymbolInstanceId::new(20), SymbolInstanceId::new(25), SymbolInstanceId::new(26)]
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
        vec![SymbolInstanceId::new(20), SymbolInstanceId::new(25), SymbolInstanceId::new(26)]
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz should be in results");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz should be in results");
}

#[test]
fn non_constraining_bare_mod_does_not_narrow_caller_chain() {
    // mod {{ "baz" }} — bare mod now inherits MODULE type into children (inherit=true).
    // The inherited MODULE type propagates through the intermediate {} scope to "baz".
    // "baz" is a FUNCTION — type mismatch with inherited MODULE filter.
    // "baz" (strong) finds nothing → constrains intermediate away → constrains mod away.
    // Result: 0 nodes.
    const QUERY: &str = r#"mod {{ "baz" }}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();
    assert_eq!(nodes.len(), 0, "inherited MODULE type filter blocks FUNCTION baz");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar should be in results");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz should be in results");
}

#[test]
fn non_constraining_inside_selector_scope_extra_depth_is_empty() {
    // "foo" { func {{ "baz" }} } — extra {} level adds a 3rd hop,
    // but foo→bar→baz is only 2 hops. Result should be empty.
    const QUERY: &str = r#""foo" { func {{ "baz" }} }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("non_constraining_inside_selector_scope_extra_depth: {:#?}", res.nodes);
    assert_eq!(res.nodes.as_vec().len(), 0, "extra depth should yield no results");
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
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz should be in results");
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
    // data(inherit="true") "driver" {{{{"channels_a"}}}}
    // Should return ONLY the path: driver → id_table → info_a → config_a → channels_a
    // NOT info_b, config_b, channels_b
    const QUERY: &str = r#"data(inherit="true") "driver" {{{{"channels_a"}}}}"#;
    let res = run_query(VERB_TEST, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();

    assert!(nodes.contains(&SymbolInstanceId::new(200)), "driver");
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "id_table");
    assert!(nodes.contains(&SymbolInstanceId::new(220)), "info_a");
    assert!(nodes.contains(&SymbolInstanceId::new(240)), "config_a");
    assert!(nodes.contains(&SymbolInstanceId::new(260)), "channels_a");

    assert!(!nodes.contains(&SymbolInstanceId::new(230)), "info_b should be pruned");
    assert!(!nodes.contains(&SymbolInstanceId::new(250)), "config_b should be pruned");
    assert!(!nodes.contains(&SymbolInstanceId::new(270)), "channels_b should be pruned");
}

#[test]
fn data_inherit_without_name_prunes_to_target_path() {
    // Same query without the top-level name selector — should also prune correctly.
    // data(inherit="true") {{{{"channels_a"}}}}
    const QUERY: &str = r#"data(inherit="true") {{{{"channels_a"}}}}"#;
    let res = run_query(VERB_TEST, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();

    // Should contain the path to channels_a
    assert!(nodes.contains(&SymbolInstanceId::new(260)), "channels_a");

    // Should NOT contain the info_b/config_b/channels_b branch
    assert!(!nodes.contains(&SymbolInstanceId::new(230)), "info_b should be pruned");
    assert!(!nodes.contains(&SymbolInstanceId::new(250)), "config_b should be pruned");
    assert!(!nodes.contains(&SymbolInstanceId::new(270)), "channels_b should be pruned");
}

#[test]
fn data_inherit_weak_parent_derives_full_chain() {
    // Regression: weak UnitVerb children were skipped even when the parent was also weak,
    // preventing the full chain from being derived.
    // data(inherit="true") {{{"channels_a"}}} — 3-level deep, all intermediate statements are weak.
    // Expected chain: id_table → info_a → config_a → channels_a
    const QUERY: &str = r#"data(inherit="true") {{{"channels_a"}}}"#;
    let res = run_query(VERB_TEST, QUERY);

    println!("{:#?}", res.nodes);
    let nodes = res.nodes.as_vec();

    assert!(nodes.contains(&SymbolInstanceId::new(210)), "id_table must be derived");
    assert!(nodes.contains(&SymbolInstanceId::new(220)), "info_a");
    assert!(nodes.contains(&SymbolInstanceId::new(240)), "config_a");
    assert!(nodes.contains(&SymbolInstanceId::new(260)), "channels_a");

    assert!(!nodes.contains(&SymbolInstanceId::new(230)), "info_b should be pruned");
    assert!(!nodes.contains(&SymbolInstanceId::new(250)), "config_b should be pruned");
    assert!(!nodes.contains(&SymbolInstanceId::new(270)), "channels_b should be pruned");
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
fn direct_only_has_file_shows_only_direct_children() {
    // file has { func } — only direct function children (not nested anon funcs)
    const QUERY: &str = r#"file("/main.go") has { func }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("direct has func: {:?}", nodes);
    // foo, bar, baz are direct children; anon150, anon350 are inside foo
    assert!(nodes.contains(&SymbolInstanceId::new(510)), "file");
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz");
    assert!(!nodes.contains(&SymbolInstanceId::new(25)), "anon150 should be filtered");
    assert!(!nodes.contains(&SymbolInstanceId::new(26)), "anon350 should be filtered");
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
fn direct_only_refs_hides_nested_refs() {
    // foo { } — default REFS, direct-only: ref at [160,170) is inside anon150,
    // so bar is NOT a direct ref from foo
    const QUERY: &str = r#""foo" { }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("direct refs from foo: {:?}", nodes);
    // Only foo itself — the ref to bar is inside anon150, not direct from foo
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(!nodes.contains(&SymbolInstanceId::new(30)), "bar should be filtered (inside anon150)");
}

#[test]
fn unnest_refs_shows_all_refs() {
    // foo { unnest } — unnest REFS: all refs including nested
    const QUERY: &str = r#""foo" { unnest }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest refs from foo: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar (via anon150 ref)");
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
fn direct_only_nested_scope_no_children_of_hidden() {
    // "foo" refs { refs { } } — REFS-only at both levels.
    // Level 1: bar is hidden (ref at [160,170) originates from inside anon150).
    // Level 2: baz must NOT appear — bar was never in the intermediate selection,
    // so bar→baz cannot be discovered.
    const QUERY: &str = r#""foo" refs { refs { } }"#;
    let res = run_query(TEST_INPUT_NESTED_FUNC, QUERY);

    let nodes = res.nodes.as_vec();
    println!("nested scope refs-only from foo: {:?}", nodes);
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo");
    assert!(!nodes.contains(&SymbolInstanceId::new(30)), "bar hidden at level 1");
    assert!(!nodes.contains(&SymbolInstanceId::new(40)), "baz must not leak from hidden bar");
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
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar (via unnested ref)");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz (direct ref from bar)");
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
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "x should be in results (child of M in e)");
    assert!(!nodes.contains(&SymbolInstanceId::new(201)), "M inside g should NOT be in results");
    assert!(!nodes.contains(&SymbolInstanceId::new(211)), "y should NOT be in results (child of M in g)");
}

#[test]
fn scoped_children_g_m() {
    // Symmetric: "g" { "M" {} } should only find y, not x.
    const QUERY: &str = r#""g" { "M" {} }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(97)), "g should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(201)), "M inside g should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(211)), "y should be in results (child of M in g)");
    assert!(!nodes.contains(&SymbolInstanceId::new(200)), "M inside e should NOT be in results");
    assert!(!nodes.contains(&SymbolInstanceId::new(210)), "x should NOT be in results (child of M in e)");
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
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "x (child of M in e)");
    assert!(!nodes.contains(&SymbolInstanceId::new(201)), "M inside g should NOT appear");
    assert!(!nodes.contains(&SymbolInstanceId::new(211)), "y should NOT appear (child of M in g)");
}

#[test]
fn label_scoped_children_g_m() {
    // Symmetric: label the scoped M-children inside g.
    const QUERY: &str = r#"@res "g" { "M" { @found } }; #found"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(97)), "g");
    assert!(nodes.contains(&SymbolInstanceId::new(201)), "M inside g");
    assert!(nodes.contains(&SymbolInstanceId::new(211)), "y (child of M in g)");
    assert!(!nodes.contains(&SymbolInstanceId::new(200)), "M inside e should NOT appear");
    assert!(!nodes.contains(&SymbolInstanceId::new(210)), "x should NOT appear (child of M in e)");
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
    assert!(!nodes.contains(&SymbolInstanceId::new(91)), "a should NOT appear");
    assert!(!nodes.contains(&SymbolInstanceId::new(92)), "b should NOT appear");
    assert!(!nodes.contains(&SymbolInstanceId::new(97)), "g should NOT appear");
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
    assert!(nodes.contains(&SymbolInstanceId::new(10)), "testmodule via unnest HAS");
    assert!(nodes.contains(&SymbolInstanceId::new(510)), "file via unnest HAS");
    assert!(nodes.contains(&SymbolInstanceId::new(20)), "foo via unnest HAS");
    assert!(nodes.contains(&SymbolInstanceId::new(30)), "bar via unnest HAS");
    assert!(nodes.contains(&SymbolInstanceId::new(40)), "baz via unnest HAS");
}

#[test]
fn scoped_children_unnest_m() {
    // "e" { "M" { unnest } } — with unnest, should still be scoped to M-in-e.
    const QUERY: &str = r#""e" { "M" { unnest } }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "x (child of M in e)");
    assert!(!nodes.contains(&SymbolInstanceId::new(201)), "M inside g should NOT appear");
    assert!(!nodes.contains(&SymbolInstanceId::new(211)), "y should NOT appear");
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
    assert!(!nodes.contains(&SymbolInstanceId::new(201)), "M inside g should NOT appear");
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
    assert!(nodes.contains(&SymbolInstanceId::new(96)), "f (ref child of e)");
    // M is not reachable via refs-only
    assert!(!nodes.contains(&SymbolInstanceId::new(200)), "M should NOT appear via refs");
}

#[test]
fn scoped_has_children_only() {
    // "e" has { "M" } — HAS-only derivation. M is a macro expansion inside e,
    // so M-in-e should appear.
    const QUERY: &str = r#""e" has { "M" }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e via HAS");
    assert!(!nodes.contains(&SymbolInstanceId::new(201)), "M inside g should NOT appear");
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
    // "e" label("parent") { "M" {} }; "g" { use("parent", forced="true") }
    // The forced use should inject e's children into g's scope, constraining
    // to only M-children visible from e.
    const QUERY: &str = r#""e" label("parent") { "M" {} }; "g" { use("parent", forced="true") }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(95)), "e");
    assert!(nodes.contains(&SymbolInstanceId::new(97)), "g");
    // e's scoped M children (x) should appear
    assert!(nodes.contains(&SymbolInstanceId::new(200)), "M inside e");
    assert!(nodes.contains(&SymbolInstanceId::new(210)), "x (child of M in e)");
}

// ============================================================================
// Multi-instance symbol constraint tests — verify that REFS constraints use
// symbol-level matching so that a symbol with declaration + definition
// instances retains both when constrained by children/parents.
//
// Symbol f (id=6) has two instances: 86 (bar.c) and 96 (main.c).
// Only instance 86 (bar.c) references h.  With instance-level matching
// the child constraint would remove instance 96; symbol-level matching
// keeps both.
// ============================================================================

#[test]
fn multi_instance_nested_retains_both() {
    // d -> {e, f} and f(bar.c) -> h.
    // "d" { "f" { "h" } } must retain both f instances (86 and 96).
    const QUERY: &str = r#""d" { "f" { "h" } }"#;
    let res = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(94)), "d should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(86)), "f (bar.c) should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(96)), "f (main.c) should be retained by symbol-level matching");
    assert!(nodes.contains(&SymbolInstanceId::new(88)), "h should be in results");
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
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="50000", end="50100", instance_type="1"); "#,
        r#""foo""#,
    );
    let res = run_query_err(VERB_TEST, QUERY);
    assert!(res.is_err(), "standalone ephemeral_instance should fail");
}

#[test]
fn ephemeral_instance_is_queryable() {
    // Create an ephemeral instance for an existing symbol and verify
    // it appears when querying that symbol.
    // Symbol 1 = "foo", object 1 = main.c
    // Existing instance 91 is at [910,919). Add a second at [50000,50100).
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="50000", end="50100", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    // Must contain the original persistent instance
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "persistent foo instance should be in results");
    // Must also contain the ephemeral instance (negative ID)
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
    assert!(has_eph, "ephemeral instance should appear with negative ID");
}

#[test]
fn ephemeral_ref_creates_edge() {
    // Create an ephemeral ref from foo's object to tar's symbol,
    // then query foo → tar. Without the ephemeral ref, foo does not
    // reference tar (only foo.bar and foobar via existing refs).
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_ref(to_symbol="4", from_object="1", "#,
        r#"start="915", end="916") "#,
        r#"}; "#,
        r#""foo" { "tar" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(94)), "tar should be reachable via ephemeral ref");
}

#[test]
fn ephemeral_layers_are_isolated_between_queries() {
    // Run two separate queries. An ephemeral instance created in one query
    // must not leak into the other.
    const QUERY1: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="51000", end="51100", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    const QUERY2: &str = r#""foo""#;

    let res1 = run_query(VERB_TEST, QUERY1);
    let res2 = run_query(VERB_TEST, QUERY2);

    let nodes1 = res1.nodes.as_vec();
    let nodes2 = res2.nodes.as_vec();

    let eph_count_1 = nodes1.iter().filter(|id| { let v: i64 = (**id).into(); v < 0 }).count();
    let eph_count_2 = nodes2.iter().filter(|id| { let v: i64 = (**id).into(); v < 0 }).count();

    assert!(eph_count_1 > 0, "query with ephemeral_instance should have negative IDs");
    assert_eq!(eph_count_2, 0, "query without ephemeral verbs should have no negative IDs");
}

#[test]
fn multiple_ephemeral_instances_same_symbol() {
    // Create two ephemeral instances for the same symbol. Both should appear.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="52000", end="52100", instance_type="1"); "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="52200", end="52300", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    let eph_count = nodes.iter().filter(|id| { let v: i64 = (**id).into(); v < 0 }).count();
    assert!(eph_count >= 2, "both ephemeral instances should appear, got {}", eph_count);
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
        r#"ephemeral_ref(to_symbol="5", from_object="1", "#,
        r#"start="913", end="914") "#,
        r#"}; "#,
        r#""foo" { "sort.IsSorted" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(95)),
        "sort.IsSorted should be reachable via ephemeral ref");
}

#[test]
fn eph_instance_in_containment() {
    // A2: Ephemeral instance participates in HAS (offset containment) queries.
    // Create eph instance of tar (symbol 4) at [911,912), which is inside
    // foo's range [910,919). Query "foo" has { "tar" } — the eph instance
    // should appear as a containment child of foo.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="4", object_id="1", "#,
        r#"start="911", end="912", instance_type="1") "#,
        r#"}; "#,
        r#""foo" has { "tar" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "foo should be in results");
    // The ephemeral instance of tar at [911,912) should appear as child
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
    assert!(has_eph,
        "ephemeral tar instance inside foo's range should appear in HAS query");
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
        r#"ephemeral_ref(to_symbol="5", from_object="1", "#,
        r#"start="914", end="915") "#,
        r#"}; "#,
        r#""foo" { "sort.IsSorted" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let edges = format_edges(res.edges);
    assert!(edges.contains(&"91-95".to_string()),
        "edge from foo(91) to sort.IsSorted(95) should exist via ephemeral ref, got {:?}", edges);
}

#[test]
fn eph_instance_produces_edge() {
    // B2: Ephemeral instance of foo.bar (symbol 2) makes both persistent (92)
    // and ephemeral instances appear as children of foo.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="2", object_id="1", "#,
        r#"start="53100", end="53200", instance_type="1") "#,
        r#"}; "#,
        r#""foo" { "foo.bar" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "foo should be in results");
    assert!(nodes.contains(&SymbolInstanceId::new(92)), "persistent foo.bar instance");
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
    assert!(has_eph,
        "ephemeral foo.bar instance should also appear with negative ID");
}

// --- C. Phase 1 chaining — multiple ephemeral producers ---

#[test]
fn layer_block_symbol_then_instance() {
    // C1: Symbol + instance in one layer block. The instance should be
    // visible in the final query.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_symbol(name="eph_chain", "#,
        r#"project_id="1", symbol_type="1"); "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="53300", end="53400", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "persistent foo");
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
    assert!(has_eph,
        "ephemeral instance from second producer should appear");
}

#[test]
fn layer_block_instance_and_ref_enables_traversal() {
    // C2: Instance + ref in one layer block. Create eph instance of
    // sort.IsSorted, and eph ref from foo's range [916,917) to
    // sort.IsSorted. Query should traverse foo → sort.IsSorted
    // and find both the persistent instance (95) and the ephemeral one.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="5", object_id="1", "#,
        r#"start="53500", end="53600", instance_type="1"); "#,
        r#"ephemeral_ref(to_symbol="5", from_object="1", "#,
        r#"start="916", end="917") "#,
        r#"}; "#,
        r#""foo" { "sort.IsSorted" }"#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "foo");
    assert!(nodes.contains(&SymbolInstanceId::new(95)),
        "persistent sort.IsSorted instance");
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
    assert!(has_eph,
        "ephemeral sort.IsSorted instance should also appear");
}

// --- D. Content-addressed caching ---

#[test]
fn eph_instance_same_params_is_idempotent() {
    // D1: Running the same layer block query twice should succeed both
    // times. The second run hits the cache (find_eph_layer returns the existing
    // layer) and reuses the existing data without re-inserting.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="53700", end="53800", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res1 = run_query(VERB_TEST, QUERY);
    let res2 = run_query(VERB_TEST, QUERY);

    let nodes1 = res1.nodes.as_vec();
    let nodes2 = res2.nodes.as_vec();
    assert!(nodes1.contains(&SymbolInstanceId::new(91)));
    assert!(nodes2.contains(&SymbolInstanceId::new(91)));

    let eph1 = nodes1.iter().filter(|id| { let v: i64 = (**id).into(); v < 0 }).count();
    let eph2 = nodes2.iter().filter(|id| { let v: i64 = (**id).into(); v < 0 }).count();
    assert!(eph1 > 0, "first run should have ephemeral instance");
    assert!(eph2 > 0, "second run should reuse cached ephemeral instance");
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
    let all_positive = nodes.iter().all(|id| { let v: i64 = (*id).into(); v > 0 });
    assert!(all_positive,
        "all IDs should be positive (no ephemeral leakage), got {:?}", nodes);
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
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="910", end="919", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)),
        "persistent foo instance should still be present");
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
    assert!(has_eph,
        "ephemeral foo instance at same offset should coexist");
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
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="54000", end="54100", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );

    // Run 1: create ephemeral layers.
    let res1 = run_query(VERB_TEST, QUERY);
    let eph_ids_1: Vec<_> = res1.nodes.as_vec().iter()
        .filter(|id| { let v: i64 = (**id).into(); v < 0 })
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
            let layer_ids = index.get_eph_layer_for_instance(inst_id.into()).await.unwrap();
            for layer_id in layer_ids {
                let ids = index.get_eph_instance_ids_for_layer(layer_id).await.unwrap();
                assert!(!ids.is_empty(),
                    "layer {} should have instances before deletion", layer_id);
                index.delete_eph_layer(layer_id).await.unwrap();
            }
        }
    });

    // Run 2: same query should create fresh layers (not cache hits).
    let res2 = run_query(VERB_TEST, QUERY);
    let eph_ids_2: Vec<_> = res2.nodes.as_vec().iter()
        .filter(|id| { let v: i64 = (**id).into(); v < 0 })
        .copied()
        .collect();
    assert!(!eph_ids_2.is_empty(), "should still produce ephemeral instances");
    // Fresh layers get new negative IDs from the sequence, so they differ.
    assert_ne!(eph_ids_1, eph_ids_2,
        "after deletion, re-running should produce different ephemeral IDs");
}

// --- Layer block tests ---

#[test]
fn layer_block_groups_ops_into_single_layer() {
    // Create an ephemeral symbol + ephemeral instance inside a layer block.
    // Both ops go into a single layer. The instance (for symbol 1 = foo)
    // should be queryable.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_symbol(name="layer_test_sym", project_id="1", symbol_type="1"); "#,
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="55000", end="55100", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(91)), "persistent foo should be in results");
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
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
        r#"ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="55100", end="55200", instance_type="1") "#,
        r#"}; "#,
        r#""foo""#,
    );

    let (res1, acts1) = run_query_traced(VERB_TEST, QUERY);
    let eph_ids_1: Vec<_> = res1.nodes.as_vec().iter()
        .filter(|id| { let v: i64 = (**id).into(); v < 0 })
        .copied()
        .collect();

    let (res2, acts2) = run_query_traced(VERB_TEST, QUERY);
    let eph_ids_2: Vec<_> = res2.nodes.as_vec().iter()
        .filter(|id| { let v: i64 = (**id).into(); v < 0 })
        .copied()
        .collect();

    assert_eq!(eph_ids_1, eph_ids_2,
        "same layer block should produce same ephemeral IDs (cache hit)");

    assert_eq!(acts1.len(), 1, "one layer-bearing statement, got {:?}", acts1);
    assert_eq!(acts2.len(), 1, "one layer-bearing statement, got {:?}", acts2);
    assert!(acts1[0].created, "first run must create the layer, got {:?}", acts1[0]);
    assert!(!acts2[0].created, "second run must hit the cache, got {:?}", acts2[0]);
    assert_eq!(acts1[0].layer_id, acts2[0].layer_id,
        "cache hit must reuse the same layer row");
}

#[test]
fn layer_block_includes_parent_context_in_hash() {
    // Two sequential layer blocks. The second layer block's hash includes
    // the parent context (the first layer block's layer ID).
    // Verify both layers are created and the query works.
    const QUERY: &str = concat!(
        r#"layer { "#,
        r#"ephemeral_ref(to_symbol="5", from_object="1", "#,
        r#"start="55300", end="55310") "#,
        r#"}; "#,
        r#"layer { "#,
        r#"ephemeral_instance(symbol_id="4", object_id="1", "#,
        r#"start="55200", end="55300", instance_type="1") "#,
        r#"}; "#,
        r#""tar""#,
    );
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert!(nodes.contains(&SymbolInstanceId::new(94)), "persistent tar should be in results");
    let has_eph = nodes.iter().any(|id| { let v: i64 = (*id).into(); v < 0 });
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
    // Both match the suffix "main.c". loc("main.c", "1") should create
    // an ephemeral symbol+instance for each matching file.
    const QUERY: &str = r#"loc("main.c", "1")"#;
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    // All instances should be ephemeral (negative IDs).
    assert!(nodes.len() >= 2, "loc should match at least 2 files, got {}", nodes.len());
    assert!(
        nodes.iter().all(|id| { let v: i64 = (*id).into(); v < 0 }),
        "all loc instances should be ephemeral (negative IDs), got {:?}", nodes
    );
}

#[test]
fn loc_single_file_with_project_filter() {
    // When a project filter is specified, only the matching project's file
    // should be returned.
    const QUERY: &str = r#"loc("main.c", "1", project="test_project_2")"#;
    let res = run_query(VERB_TEST, QUERY);

    let nodes = res.nodes.as_vec();
    assert_eq!(nodes.len(), 1, "loc with project filter should match exactly 1 file, got {:?}", nodes);
    assert!(
        { let v: i64 = nodes[0].into(); v < 0 },
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
                .create_eph_layer(None, &hash, index::db_diesel::EphLayerKind::Layer).await.unwrap();
            assert!(txn.created(), "first create should return created=true");
            txn.rollback().await.unwrap();
        }

        // Attempt 2: same hash — must get created=true (layer was rolled back).
        {
            let txn = index
                .create_eph_layer(None, &hash, index::db_diesel::EphLayerKind::Layer).await.unwrap();
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

        // Call find_symbol with no ephemeral layers visible.
        let result = index
            .find_symbol(&filter, ScopeContext::Skip, ScopeContext::Skip, &EphContext::new())
            .await
            .expect("find_symbol should succeed (canary must be filtered out)")
            .into_inner();

        // The canary row must never appear in results.
        assert!(
            result.nodes.is_empty(),
            "canary symbol must not appear in results with eph_ids=[], got {} nodes",
            result.nodes.len()
        );

        // Even with canary layer in eph_ids, the canary should be findable.
        let result = index
            .find_symbol(&filter, ScopeContext::Skip, ScopeContext::Skip, &EphContext::from_slice(&[CANARY_LAYER_ID]))
            .await
            .expect("find_symbol with canary in eph_ids should succeed")
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
            eph_layer: Some(CANARY_LAYER_ID),
        },
        symbol_instance: SymbolInstance {
            id: -999999,
            symbol: -999999,
            object_id: -999999,
            offset_range: (Bound::Included(0), Bound::Excluded(1)),
            instance_type: 1,
            eph_layer: Some(CANARY_LAYER_ID),
        },
        object: Object {
            id: -999999,
            project_id: -999999,
            module_path: "".into(),
            filesystem_path: "/__canary__".into(),
            filetype: "canary".into(),
            content_hash: "".into(),
        },
        project: Project {
            id: -999999,
            project_name: "__canary__".into(),
            root_path: "/__canary__".into(),
            upload_status: "complete".into(),
        },
        query_statements: vec![],
    });

    // With empty eph_ids, the canary is a leak.
    assert!(
        selection.has_eph_leak(&EphContext::new()),
        "canary row must be detected as a leak with empty eph_ids"
    );

    // With some other layer, canary is still a leak.
    assert!(
        selection.has_eph_leak(&EphContext::from_slice(&[-1])),
        "canary row must be detected as a leak when its ID is not in eph_ids"
    );

    // Only when canary layer is explicitly included, it's not a leak.
    assert!(
        !selection.has_eph_leak(&EphContext::from_slice(&[CANARY_LAYER_ID])),
        "canary row must not be a leak when its ID is in eph_ids"
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
            eph_layer: Some(CANARY_LAYER_ID),
        }
    }
    fn canary_instance() -> SymbolInstance {
        SymbolInstance {
            id: -999999,
            symbol: -999999,
            object_id: -999999,
            offset_range: (Bound::Included(0), Bound::Excluded(1)),
            instance_type: 1,
            eph_layer: Some(CANARY_LAYER_ID),
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
        }
    }
    fn canary_ref() -> SymbolRef {
        SymbolRef {
            id: -999999,
            to_symbol: -999999,
            from_object: -999999,
            from_offset_range: (Bound::Included(0), Bound::Excluded(1)),
            eph_layer: Some(CANARY_LAYER_ID),
        }
    }

    let empty_eph = EphContext::new();

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
    let res = run_query_err(VERB_TEST, r#"loc("nonexistent_file.c", "1")"#);
    let msg = res.err().expect("should be an error").to_string();
    assert!(msg.contains("no file matching"), "expected 'no file matching' error, got: {}", msg);
}

#[test]
fn loc_bad_line_number_errors() {
    let res = run_query_err(VERB_TEST, r#"loc("main.c", "999")"#);
    let msg = res.err().expect("should be an error").to_string();
    assert!(msg.contains("out of range"), "expected 'out of range' error, got: {}", msg);
}

#[test]
fn loc_missing_args_errors() {
    let res = run_query_err(VERB_TEST, r#"loc("main.c")"#);
    let msg = res.err().expect("should be an error").to_string();
    assert!(msg.contains("requires two positional"), "expected 'requires two positional' error, got: {}", msg);
}

#[test]
fn negative_id_with_null_eph_layer_rejected_by_check() {
    // Regression: an orphan row shaped like `id < 0, eph_layer = NULL`
    // would re-surface as a stale loc:* node (this is what produced the
    // duplicate `-18` instance pre-CHECK).  The migration's CHECK
    // constraints `id > 0 OR eph_layer IS NOT NULL` on symbols /
    // symbol_instances / symbol_refs must reject any such insert.
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

        let err = diesel::sql_query(
            "INSERT INTO index.symbol_instances \
             (id, symbol, object_id, offset_range, instance_type, eph_layer) \
             VALUES (-424242, 1, 1, int4range(0, 1), 1, NULL)",
        )
        .execute(&mut conn)
        .await
        .err()
        .expect("CHECK constraint must reject id<0 with eph_layer=NULL");
        let msg = format!("{:?}", err);
        assert!(
            msg.contains("eph_id_sign_check") || msg.contains("check constraint"),
            "expected CHECK-constraint violation, got: {}",
            msg
        );
    });
}

#[test]
fn wrap_loc_yields_single_instance_and_parent_has_edge() {
    // Regression for the {loc(...)} wrapper pattern.  The user's
    // failing query had an inner `loc("qaic_drv.c", "100")` inside
    // `{...}`; a leftover persistent loc-row caused two nodes plus a
    // missing has-edge from the containing function.  On a clean DB
    // the wrapper must yield exactly one ephemeral loc node and a
    // has-edge from the containing function (fixture: wrap_loc_target,
    // body covers line 1 of /main.c).
    const QUERY: &str = r#"{loc("main.c", "1", project="test_project")}"#;
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
    assert!(loc_inst_id < 0, "loc instance must be ephemeral, got id {}", loc_inst_id);

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
    //   layer { ephemeral_instance(symbol_id="@target", object_id="1",
    //                              start="50000", end="50100",
    //                              instance_type="1") }
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
        r#"layer { ephemeral_instance(symbol_id="@target", object_id="1", "#,
        r#"start="50000", end="50100", instance_type="1") }"#,
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

#[test]
fn ephemeral_instance_label_hash_matches_equivalent_literal() {
    // Validates the cache-key semantic: an ephemeral_instance with
    // symbol_id="@target" where @target resolves to symbol id 1 must
    // produce the same layer hash as symbol_id="1" — same resolved
    // symbol IDs in the hash inputs means the same cache entry.
    //
    // Hashes are private, but cache sharing has a direct
    // observable: a shared layer means a shared row in
    // `symbol_instances` with a shared negative `id`.  We assert that
    // the negative-ID set produced by the literal form intersects the
    // negative-ID set produced by the label form — only possible if
    // both runs hit the same eph_layers row.  (Using identical
    // start/end ensures the rows are byte-identical; running the
    // label form after the literal means the literal's already-
    // committed row is what the label run finds via ON CONFLICT.)
    //
    // Robust against concurrent shared-DB pollution: we compare the
    // *negative IDs we produced* across the two runs, not absolute
    // table counts.
    const LITERAL_FIRST: &str = concat!(
        r#"layer { ephemeral_instance(symbol_id="1", object_id="1", "#,
        r#"start="60500", end="60600", instance_type="1") }"#,
    );
    const LABEL_VARIANT: &str = concat!(
        r#"@target "foo"; "#,
        r#"layer { ephemeral_instance(symbol_id="@target", object_id="1", "#,
        r#"start="60500", end="60600", instance_type="1") }"#,
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
         equivalent literal form (same layer hash means same eph_layers \
         row means same instance id).  literal_eph={:?}, label_eph={:?}",
        literal_eph, label_eph,
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
        nodes.iter().all(|id| { let v: i64 = (*id).into(); v < 0 }),
        "all search instances should be ephemeral (negative IDs), got {:?}",
        nodes,
    );
}

#[test]
fn search_skeleton_no_match_returns_empty() {
    // Nonexistent needle ≥ 3 chars: helper returns an empty match set, the
    // verb should not bail.  Step 6 returns Ok(Some(LayerSpec)) regardless,
    // so the eph_layer is created (empty); the caller's selection is then
    // empty.  Critically: the query should succeed (no err), and produce a
    // result with zero matched instances.
    const QUERY: &str = r#"search("nonexistent_xyzzy")"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let eph_only: Vec<_> = res.nodes.as_vec().into_iter()
        .filter(|id| { let v: i64 = (*id).into(); v < 0 })
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
    // whole_word="true", `search("foo")` must NOT match the leading
    // "foobar" or "foo_bar" but MUST match the freestanding "foo" and
    // "foo.bar" instances.  The substring default (whole_word=false)
    // would match all four occurrences.
    let substring = run_query(TEST_INPUT_SEARCH, r#"search("foo")"#);
    let whole_word = run_query(TEST_INPUT_SEARCH, r#"search("foo", whole_word="true")"#);

    let n_substr = substring.nodes.as_vec().len();
    let n_whole = whole_word.nodes.as_vec().len();
    assert!(
        n_substr > n_whole,
        "whole_word=\"true\" must match strictly fewer ranges than substring; \
         got substring={}, whole_word={}",
        n_substr, n_whole,
    );
}

#[test]
fn search_case_smart_default_lowercase_is_insensitive() {
    // Fixture object 4 has "Foo FOO foo".  An all-lowercase query under
    // the default smart-case resolves to insensitive, so all three
    // tokens match across the fixture (plus matches in other files).
    let res = run_query(TEST_INPUT_SEARCH, r#"search("foo", whole_word="true")"#);
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
    let res = run_query(TEST_INPUT_SEARCH, r#"search("Foo", whole_word="true")"#);
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
    let res = run_query(TEST_INPUT_SEARCH, r#"search("Foo", case="insensitive", whole_word="true")"#);
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
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", whole_word="yes")"#);
    assert!(res.is_err(), "search with whole_word=yes should error");
}

#[test]
fn search_bad_limit_value_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", limit="not_a_number")"#);
    assert!(res.is_err(), "search with non-integer limit should error");
}

#[test]
fn search_zero_limit_rejects() {
    let res = run_query_err(TEST_INPUT_SEARCH, r#"search("foo", limit="0")"#);
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
    const QUERY: &str = r#"search("foo", whole_word="true", limit="3")"#;
    let (res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert_eq!(acts.len(), 1, "one layer-bearing statement, got {:?}", acts);
    assert!(acts[0].created, "unique limit must produce a cache miss, got {:?}", acts[0]);
    assert!(acts[0].truncated, "limit=3 must truncate, got {:?}", acts[0]);

    assert_eq!(
        res.nodes.as_vec().len(), 3,
        "limit=3 should return exactly 3 matches, got {}",
        res.nodes.as_vec().len(),
    );

    let truncation_warns: Vec<_> = res.warnings.iter()
        .filter(|w| format!("{}", w).contains("truncated"))
        .collect();
    assert_eq!(
        truncation_warns.len(), 1,
        "cache-miss should surface exactly one truncation warning, got {:?}",
        res.warnings,
    );
}

#[test]
fn search_truncation_warning_surfaces_on_cache_hit() {
    // Two back-to-back identical queries.  The first call's populate
    // writes truncated=true to eph_layers; the second call hits the cache
    // (no populate) and reads truncated=true back, reconstructing the
    // warning from the verb's own span via make_truncation_warning.
    //
    // Verifies the "warning always visible" property -- silent loss on
    // cache hit was an early footgun the design pins down.  limit=2 is
    // unique to this test so the miss->hit sequence is guaranteed and
    // asserted, not assumed.
    const QUERY: &str = r#"search("foo", whole_word="true", limit="2")"#;
    let (_first, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert!(acts1[0].created, "first call must be a cache miss, got {:?}", acts1);
    assert!(!acts2[0].created, "second call must be a cache hit, got {:?}", acts2);
    assert_eq!(acts1[0].layer_id, acts2[0].layer_id, "hit must reuse the same layer row");
    assert!(acts2[0].truncated,
        "cache hit must propagate the persisted truncated flag, got {:?}", acts2[0]);

    let truncation_warns: Vec<_> = second.warnings.iter()
        .filter(|w| format!("{}", w).contains("truncated"))
        .collect();
    assert_eq!(
        truncation_warns.len(), 1,
        "cache-hit should also surface the truncation warning, got {:?}",
        second.warnings,
    );
}

#[test]
fn search_no_truncation_warning_when_under_cap() {
    // Plenty of headroom: explicit large limit so no truncation occurs.
    // The result set should not contain a truncation warning.
    const QUERY: &str = r#"search("foo", limit="500")"#;
    let res = run_query(TEST_INPUT_SEARCH, QUERY);

    let truncation_warns: Vec<_> = res.warnings.iter()
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
    // `_` is in is_word_char's ASCII range so `search("foo", whole_word="true")`
    // must NOT match "foo_bar".  Object 3's "foobar foo foo_bar foo.bar" lets
    // us cross-check: the whole-word search picks up "foo" (freestanding) and
    // "foo" inside "foo.bar" (`.` separates), but neither "foobar" (no left
    // boundary) nor "foo_bar" (underscore is a word char).
    let res = run_query(TEST_INPUT_SEARCH, r#"project("search_proj_1") search("foo", whole_word="true", case="sensitive")"#);
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
    let substring = run_query(TEST_INPUT_SEARCH, r#"project("search_proj_1") search("foo")"#);
    let whole = run_query(TEST_INPUT_SEARCH, r#"project("search_proj_1") search("foo", whole_word="true")"#);
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
    let p1 = run_query(TEST_INPUT_SEARCH, r#"project("search_proj_1") search("foo")"#);
    let p2 = run_query(TEST_INPUT_SEARCH, r#"project("search_proj_2") search("foo")"#);
    let no_filter = run_query(TEST_INPUT_SEARCH, r#"search("foo")"#);

    let n_p1 = p1.nodes.as_vec().len();
    let n_p2 = p2.nodes.as_vec().len();
    let n_none = no_filter.nodes.as_vec().len();

    assert!(n_p1 > 0 && n_p2 > 0, "each project should yield matches");
    assert!(
        n_p1 < n_none && n_p2 < n_none,
        "project filter should narrow vs no filter; p1={}, p2={}, no_filter={}",
        n_p1, n_p2, n_none,
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
        p1_nodes, p2_nodes,
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
    // same eph_layer row.  The activation trace asserts the cache paths
    // directly: first call creates+populates, second reuses the same layer
    // id without repopulating.  limit=399 is unique to this test (limit is
    // part of the layer hash) so the miss->hit sequence is guaranteed even
    // with other tests sharing the fixture DB.
    const QUERY: &str = r#"project("search_proj_1") search("foo", limit="399")"#;
    let (first, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert!(acts1[0].created, "first call must create the layer, got {:?}", acts1);
    assert!(!acts2[0].created, "second call must hit the cache, got {:?}", acts2);
    assert_eq!(acts1[0].layer_id, acts2[0].layer_id, "hit must reuse the same layer row");

    assert_eq!(
        first.nodes.as_vec(),
        second.nodes.as_vec(),
        "identical search() under the same filter should yield identical nodes",
    );
}

#[test]
fn search_different_filter_different_cache() {
    // The composite filter is mixed into the eph_layer hash via
    // CompositeFilter::hash_into.  project("p1") and project("p2")
    // therefore produce DIFFERENT hashes and different cache entries,
    // each scoped to its own project's matches.  Asserted two ways: the
    // activation trace shows two distinct freshly-created layer rows
    // (limit=400 is unique to this test, so both runs are cache misses),
    // and the node sets are disjoint (a shared layer would return
    // identical nodes for both).
    let (p1, acts1) = run_query_traced(TEST_INPUT_SEARCH, r#"project("search_proj_1") search("foo", limit="400")"#);
    let (p2, acts2) = run_query_traced(TEST_INPUT_SEARCH, r#"project("search_proj_2") search("foo", limit="400")"#);

    assert!(acts1[0].created && acts2[0].created,
        "both filter variants must create their own layer, got {:?} / {:?}", acts1, acts2);
    assert_ne!(acts1[0].layer_id, acts2[0].layer_id,
        "different composite filters must map to different layer rows");

    let p1_nodes: std::collections::HashSet<_> = p1.nodes.as_vec().into_iter().collect();
    let p2_nodes: std::collections::HashSet<_> = p2.nodes.as_vec().into_iter().collect();
    assert!(
        p1_nodes.is_disjoint(&p2_nodes),
        "project-scoped search layers must produce disjoint node sets; \
         shared nodes would indicate the cache key is not filter-aware. \
         p1={:?}, p2={:?}",
        p1_nodes, p2_nodes,
    );
}

// ============================================================================
// search() verb tests: explicit cache-state observability
// ============================================================================
//
// These tests assert cache behaviour DIRECTLY -- via the layer-activation
// trace (created vs cache hit) and the eph_layers row state (populated /
// truncated flags, per-layer row counts) -- rather than inferring it from
// eph instance IDs.
//
// Isolation convention: all tests share one fixture DB per fixture file and
// run in parallel, so cache entries leak across tests.  `limit` is part of
// the search layer hash; every cache-sensitive test therefore uses a limit
// value unique across this file (grep for `limit="` before picking a new
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
    // A cold search() must report exactly one activation with created=true,
    // and leave behind an eph_layers row with populated=true (2-phase
    // commit completed) and truncated=false, whose instance rows match the
    // returned nodes 1:1.
    const QUERY: &str = r#"search("foo", limit="397")"#;
    let (res, acts) = run_query_traced(TEST_INPUT_SEARCH, QUERY);

    assert_eq!(acts.len(), 1, "one layer-bearing statement, got {:?}", acts);
    let act = acts[0];
    assert!(act.created, "unique limit must produce a cache miss, got {:?}", act);
    assert!(!act.truncated, "limit=397 leaves ample headroom, got {:?}", act);

    let (meta, (symbols, instances)) = eph_layer_state(TEST_INPUT_SEARCH, act.layer_id);
    assert_eq!(meta.kind, "search", "layer kind must be 'search', got {:?}", meta);
    assert!(meta.populated, "populate must have been committed, got {:?}", meta);
    assert!(!meta.truncated, "truncated flag must be false on the row, got {:?}", meta);

    // One instance per byte-range match; the statement's selection is
    // exactly the layer's instances.
    assert_eq!(
        instances,
        res.nodes.as_vec().len() as i64,
        "layer instance rows must match returned nodes 1:1",
    );
    // One eph symbol per matching project: substring "foo" matches objects
    // in both fixture projects (proj 1: basic/multi/boundary; proj 2:
    // mixedcase/docless).
    assert_eq!(symbols, 2, "expected one 'search:foo' symbol per matching project");
}

#[test]
fn search_repeat_call_hits_cache_without_repopulating() {
    // Second identical call must reuse the SAME layer row (created=false)
    // and must not insert any new rows into it -- the per-layer symbol and
    // instance counts are identical before and after the repeat call.
    const QUERY: &str = r#"search("foo", limit="398")"#;

    let (first, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(acts1[0].created, "first call must create the layer, got {:?}", acts1);
    let layer_id = acts1[0].layer_id;
    let (_, counts_after_first) = eph_layer_state(TEST_INPUT_SEARCH, layer_id);

    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(!acts2[0].created, "second call must hit the cache, got {:?}", acts2);
    assert_eq!(acts2[0].layer_id, layer_id, "hit must reuse the same layer row");

    let (meta, counts_after_second) = eph_layer_state(TEST_INPUT_SEARCH, layer_id);
    assert!(meta.populated, "layer must remain populated, got {:?}", meta);
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
    // Truncation state lives on the eph_layers row itself.  First call
    // truncates (whole-word "foo" has ~10 matches, limit=1) and writes
    // truncated=true in the same transaction as the populate; the repeat
    // call reads it back on the cache-hit path and still surfaces the
    // warning.
    const QUERY: &str = r#"search("foo", whole_word="true", limit="1")"#;

    let (_, acts1) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(acts1[0].created, "first call must be a cache miss, got {:?}", acts1);
    assert!(acts1[0].truncated, "limit=1 must truncate, got {:?}", acts1);

    let (meta, _) = eph_layer_state(TEST_INPUT_SEARCH, acts1[0].layer_id);
    assert!(meta.populated, "truncated layer is still fully committed, got {:?}", meta);
    assert!(meta.truncated, "truncated=true must be persisted on the row, got {:?}", meta);

    let (second, acts2) = run_query_traced(TEST_INPUT_SEARCH, QUERY);
    assert!(!acts2[0].created, "second call must be a cache hit, got {:?}", acts2);
    assert_eq!(acts2[0].layer_id, acts1[0].layer_id, "hit must reuse the same layer row");
    assert!(acts2[0].truncated,
        "cache hit must propagate the persisted truncated flag, got {:?}", acts2);
    assert!(
        second.warnings.iter().any(|w| format!("{}", w).contains("truncated")),
        "cache hit must still surface the truncation warning, got {:?}",
        second.warnings,
    );
}
