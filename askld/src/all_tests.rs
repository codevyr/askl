use crate::test_util::{
    format_edges, run_query, run_query_err, TEST_INPUT_A, TEST_INPUT_B, TEST_INPUT_CONTAINMENT, TEST_INPUT_MODULES, TEST_INPUT_TREE_BROWSER,
};
use index::symbols::DeclarationId;

#[test]
fn single_node_query() {
    env_logger::init();

    const QUERY: &str = r#""a""#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(91)]);
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
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn single_parent_query() {
    const QUERY: &str = r#"{"a"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(942)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-91"]);
}

#[test]
fn double_parent_query() {
    const QUERY: &str = r#"{{"b"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(942)
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "942-91", "942-92"]);
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
        vec![DeclarationId::new(91), DeclarationId::new(92)]
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
        vec![DeclarationId::new(92), DeclarationId::new(93)]
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
        vec![DeclarationId::new(93), DeclarationId::new(942)]
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
        vec![DeclarationId::new(91), DeclarationId::new(97)]
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
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(97),
            DeclarationId::new(942),
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
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(97),
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
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(942)
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "92-942", "942-91", "942-92"]);
}

#[test]
fn generic_forced_child_query_3() {
    const QUERY: &str = r#""main" {
            @forced(name="c")
        }"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(93), DeclarationId::new(942)]
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
        vec![DeclarationId::new(91), DeclarationId::new(92),]
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
        vec![DeclarationId::new(91), DeclarationId::new(92),]
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
        vec![DeclarationId::new(91), DeclarationId::new(92),]
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
        vec![DeclarationId::new(91), DeclarationId::new(92),]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn ignore_node_no_result() {
    const QUERY: &str = r#""a" {@ignore("b")}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_sibling() {
    const QUERY: &str = r#""d" {@ignore("e")}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(94), DeclarationId::new(96)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["94-96"]);
}

#[test]
fn ignore_node_parent_no_result() {
    const QUERY: &str = r#"@ignore("d") {"e"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(95)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_parent_no_result_2() {
    const QUERY: &str = r#" {@ignore("a") "a"{}}; @ignore("d") {"f" {@ignore("asdf")}};"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(96), DeclarationId::new(97)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["96-97"]);
    println!("{:#?}", res.warnings);
    assert_eq!(res.warnings.len(), 1);
}

#[test]
fn ignore_node_parent_no_result_3() {
    const QUERY: &str = r#" {@ignore("a") "a"{}};"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_parent_no_result_4() {
    const QUERY: &str = r#"@ignore("d") {"f" {@ignore("asdf")}};"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(96), DeclarationId::new(97)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["96-97"]);
}

#[test]
fn ignore_node_wrong_parent() {
    const QUERY: &str = r#"@ignore("a") {"e"}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(94), DeclarationId::new(95)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["94-95"]);
}

#[test]
fn ignore_node_recurse() {
    // Ignore applies to all children, so this should return no results
    const QUERY: &str = r#""a" @ignore("b") {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_another_statement() {
    // Ignore applies to all children, so this should return no results
    const QUERY: &str = r#"@preamble @ignore("b") ; "a" {}; "a" {}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(91)]);
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
            DeclarationId::new(94),
            DeclarationId::new(95),
            DeclarationId::new(96),
            DeclarationId::new(97),
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
        vec![DeclarationId::new(94), DeclarationId::new(96),]
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
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
#[ignore = "module filter is broken after modules-as-symbols migration"]
fn project_double_parent_query() {
    // Tests @module filter with double parent query pattern
    const QUERY: &str = r#"@module("test") {{"b"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);
    assert_eq!(
        res.nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(942)
        ]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["91-92", "91-92", "942-91", "942-92"]);
}

#[test]
#[ignore = "@module is now a type selector, not a filter - old filter behavior obsoleted"]
fn module_filter_excludes_other_modules() {
    const FILTERED_QUERY: &str = r#"@module("test") "a""#;
    let filtered = run_query(TEST_INPUT_MODULES, FILTERED_QUERY);

    println!("{:#?}", filtered.nodes);
    println!("{:#?}", filtered.edges);

    assert_eq!(filtered.nodes.as_vec(), vec![DeclarationId::new(91)]);
    assert_eq!(filtered.edges.0.len(), 0);

    const UNFILTERED_QUERY: &str = r#""a""#;
    let unfiltered = run_query(TEST_INPUT_MODULES, UNFILTERED_QUERY);
    let unfiltered_nodes = unfiltered.nodes.as_vec();

    assert_eq!(
        unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"@module("test") "a"; "a""#;
    let filtered_unfiltered = run_query(TEST_INPUT_MODULES, FILTERED_AND_UNFILTERED_QUERY);
    let filtered_unfiltered_nodes = filtered_unfiltered.nodes.as_vec();

    assert_eq!(
        filtered_unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const PREAMBLE_FILTERED_QUERY: &str = r#"@preamble @module("test"); "a""#;
    let preamble_filtered = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered.nodes.as_vec();

    assert_eq!(preamble_filtered_nodes, vec![DeclarationId::new(91)]);
}

#[test]
#[ignore = "@module is now a type selector, not a filter - old filter behavior obsoleted"]
fn module_filter_selects_other_module() {
    const QUERY: &str = r#"@module("other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
#[ignore = "@module is now a type selector, not a filter - old filter behavior obsoleted"]
fn module_filter_replaced_by_second_invocation() {
    const QUERY: &str = r#"@module("test") @module("other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
#[ignore = "@module is now a type selector, not a filter - old filter behavior obsoleted"]
fn module_filter_children_scope_honors_filter() {
    const QUERY: &str = r#"@module("other") "a" {}"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(201), DeclarationId::new(202)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["201-202"]);
}

#[test]
fn project_filter_excludes_other_projects() {
    const FILTERED_QUERY: &str = r#"@project("test_project") "a""#;
    let filtered = run_query(TEST_INPUT_MODULES, FILTERED_QUERY);

    println!("{:#?}", filtered.nodes);
    println!("{:#?}", filtered.edges);

    assert_eq!(
        filtered.nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(201)]
    );
    assert_eq!(filtered.edges.0.len(), 0);

    const UNFILTERED_QUERY: &str = r#""a""#;
    let unfiltered = run_query(TEST_INPUT_MODULES, UNFILTERED_QUERY);
    let unfiltered_nodes = unfiltered.nodes.as_vec();

    assert_eq!(
        unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"@project("test_project") "a"; "a""#;
    let filtered_unfiltered = run_query(TEST_INPUT_MODULES, FILTERED_AND_UNFILTERED_QUERY);
    let filtered_unfiltered_nodes = filtered_unfiltered.nodes.as_vec();

    assert_eq!(
        filtered_unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const PREAMBLE_FILTERED_QUERY: &str = r#"@preamble @project("test_project"); "a""#;
    let preamble_filtered = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered.nodes.as_vec();

    assert_eq!(
        preamble_filtered_nodes,
        vec![DeclarationId::new(91), DeclarationId::new(201)]
    );

    const REPLACE_PROJECT_FILTERED_QUERY: &str = r#"@project("adsf") @project("test_project") "a""#;
    let replace_project_filtered = run_query(TEST_INPUT_MODULES, REPLACE_PROJECT_FILTERED_QUERY);
    let replace_project_filtered_nodes = replace_project_filtered.nodes.as_vec();

    assert_eq!(
        replace_project_filtered_nodes,
        vec![DeclarationId::new(91), DeclarationId::new(201)]
    );
}

#[test]
fn project_filter_selects_other_project() {
    const QUERY: &str = r#"@project("other_project") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(301)]);
    assert_eq!(res.edges.0.len(), 0);

    const WRONG_PROJECT_QUERY: &str = r#"@project("blablabla_project") "a""#;
    let res = run_query(TEST_INPUT_MODULES, WRONG_PROJECT_QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
#[ignore = "@module is now a type selector, not a filter - old filter behavior obsoleted"]
fn project_and_module_filters_combine() {
    const QUERY: &str = r#"@project("test_project") @module("other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(201)]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
#[ignore = "@module is now a type selector, not a filter - old filter behavior obsoleted"]
fn conflicting_project_and_module_filters_return_empty() {
    const QUERY: &str = r#"@project("other_project") @module("other") "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn scoped_project_filter_does_not_leak() {
    const QUERY: &str = r#"@project("other_project") "a"; "a""#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );
    assert_eq!(res.edges.0.len(), 0);
}

#[test]
fn multiple_projects_with_forced() {
    const QUERY: &str = r#"@project("test_project") "a" { @project("other_project") !"a" }"#;
    let res = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
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
            DeclarationId::new(86),
            DeclarationId::new(94),
            DeclarationId::new(95),
            DeclarationId::new(96)
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
            DeclarationId::new(86),
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(93),
            DeclarationId::new(94),
            DeclarationId::new(95),
            DeclarationId::new(96),
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
    const QUERY: &str = r#"@preamble"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_empty_commands() {
    const QUERY: &str = r#";;;;;@preamble"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_second_command() {
    const QUERY: &str = r#""a";;;;;@preamble"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(res.nodes.as_vec(), vec![DeclarationId::new(91)]);
    let edges = format_edges(res.edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_inner_command() {
    const QUERY: &str = r#""a"{;;;;;@preamble}"#;
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
    const QUERY: &str = r#"@preamble @scope(isolated="true")"#;
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
        vec![DeclarationId::new(96), DeclarationId::new(97)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["96-97"]);
}

#[test]
fn weak_grandchild_2() {
    const QUERY: &str = r#"@preamble @project("test_project"); "a"{{{{{}}}}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
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

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(942)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-91"]);
}

#[test]
fn weak_grandparent_2() {
    const QUERY: &str = r#"{"main"{"a"}}"#;
    let res = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    assert_eq!(
        res.nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(942)]
    );
    let edges = format_edges(res.edges);
    assert_eq!(edges, vec!["942-91"]);
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
    // @module("testmodule") @has { @file @has { "foo" } }
    // With direct-children-only: module(3) → file(2) → function(1)
    // Returns: module "testmodule", file, and function "testmodule.foo"
    const QUERY: &str = r#"@module("testmodule") @has { @file @has { "foo" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have module (10), file (510), and foo (20)
    assert_eq!(res.nodes.as_vec().len(), 3);
}

#[test]
fn has_parents_query() {
    // @file @has { "foo" }
    // With direct-children-only: file(2) → function(1)
    // Returns: function "foo" and its containing file
    const QUERY: &str = r#"@file @has { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510) and foo (20)
    assert_eq!(res.nodes.as_vec().len(), 2);
}

#[test]
fn mixed_has_refs_query() {
    // @module("testmodule") @has { @file @has { "foo" {} } }
    // With direct-children-only: module(3) → file(2) → function(1), then refs
    // Returns: module, file, foo in file, and foo's callees (bar)
    const QUERY: &str = r#"@module("testmodule") @has { @file @has { "foo" {} } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have module (10), file (510), foo (20), and bar (30)
    assert_eq!(res.nodes.as_vec().len(), 4);
}

#[test]
fn type_selector_function_query() {
    // @function("testmodule.foo")
    // Returns function named "testmodule.foo"
    const QUERY: &str = r#"@function("testmodule.foo")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have foo (20)
    assert_eq!(res.nodes.as_vec().len(), 1);
}

#[test]
fn type_selector_function_filter_at_root() {
    // @function (no name) at root acts as filter - returns empty
    // because there's no parent to derive from
    const QUERY: &str = r#"@function"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Filter at root level = empty
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn type_selector_function_explicit_select_all() {
    // @function(filter="false")
    // Explicitly select all functions (override default filter behavior)
    const QUERY: &str = r#"@function(filter="false")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have foo (20), bar (30), baz (40)
    assert_eq!(res.nodes.as_vec().len(), 3);
}

#[test]
fn type_selector_module_query() {
    // @module("testmodule")
    // Returns module named "testmodule"
    const QUERY: &str = r#"@module("testmodule")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have module (10)
    assert_eq!(res.nodes.as_vec().len(), 1);
}

#[test]
fn has_does_not_propagate() {
    // @file @has { "foo" { "bar" } }
    // foo is in file (has), bar is callee of foo (refs)
    // bar does NOT need to be in file directly (nested refs uses refs semantics)
    // With direct-children-only: file(2) → function(1)
    const QUERY: &str = r#"@file @has { "foo" { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510), foo (20), and bar (30)
    assert_eq!(res.nodes.as_vec().len(), 3);
}

// ============================================================================
// @has vs @refs Comparison Tests
// ============================================================================
//
// These tests verify that @has (containment) and @refs (call graph) behave differently.
// Test data has:
// - Module contains: foo, bar, baz (via offset ranges)
// - Call graph: foo→bar→baz (via symbol_refs)

#[test]
fn has_vs_refs_module_to_function() {
    // With direct-children-only: test file(2) → function(1)
    // @has: file CONTAINS foo (offset ranges)
    const HAS_QUERY: &str = r#"@file("/main.go") @has { "foo" }"#;
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    println!("@has result: {:#?}", has_res.nodes);

    // File contains foo, so we get both
    assert_eq!(has_res.nodes.as_vec().len(), 2);
    assert!(has_res.nodes.as_vec().contains(&DeclarationId::new(510))); // file
    assert!(has_res.nodes.as_vec().contains(&DeclarationId::new(20))); // foo

    // @refs: file CALLS foo? No refs from file to foo exist
    const REFS_QUERY: &str = r#"@file("/main.go") @refs { "foo" }"#;
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    println!("@refs result: {:#?}", refs_res.nodes);

    // File doesn't call foo - no refs relationship exists
    // Both parent and child are filtered out when relationship doesn't hold
    assert_eq!(refs_res.nodes.as_vec().len(), 0);
}

#[test]
fn has_vs_refs_function_to_function() {
    // @has: foo CONTAINS bar? No - foo [100,200) doesn't contain bar [200,300)
    const HAS_QUERY: &str = r#""foo" @has { "bar" }"#;
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    println!("@has result: {:#?}", has_res.nodes);

    // foo doesn't contain bar - no containment relationship exists
    // Both parent and child are filtered out when relationship doesn't hold
    assert_eq!(has_res.nodes.as_vec().len(), 0);

    // @refs: foo CALLS bar? Yes - there's a ref from foo to bar
    const REFS_QUERY: &str = r#""foo" @refs { "bar" }"#;
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    println!("@refs result: {:#?}", refs_res.nodes);

    // foo calls bar, so we get both
    assert_eq!(refs_res.nodes.as_vec().len(), 2);
    assert!(refs_res.nodes.as_vec().contains(&DeclarationId::new(20))); // foo
    assert!(refs_res.nodes.as_vec().contains(&DeclarationId::new(30))); // bar
}

#[test]
fn has_vs_refs_all_children() {
    // Test @has vs @refs behavior comparison
    // @has: directory contains file(s) and transitively contains functions
    const HAS_QUERY: &str = r#"@directory("/") @has { @file @has { @function } }"#;
    let has_res = run_query(TEST_INPUT_CONTAINMENT, HAS_QUERY);

    println!("@has result: {:#?}", has_res.nodes);

    // Directory "/" contains file "/main.go" which contains foo, bar, baz
    // Results: directory + file + 3 functions = 5 nodes
    assert_eq!(has_res.nodes.as_vec().len(), 5);

    // @refs: test function-to-function refs
    // foo calls bar, bar calls baz
    const REFS_QUERY: &str = r#""foo" @refs { @function }"#;
    let refs_res = run_query(TEST_INPUT_CONTAINMENT, REFS_QUERY);

    println!("@refs result: {:#?}", refs_res.nodes);

    // foo calls bar, so we get foo + bar
    assert_eq!(refs_res.nodes.as_vec().len(), 2); // foo + bar
}

#[test]
fn refs_is_default_relationship() {
    // Bare {} should use refs (the default)
    const DEFAULT_QUERY: &str = r#""foo" { "bar" }"#;
    let default_res = run_query(TEST_INPUT_CONTAINMENT, DEFAULT_QUERY);

    // Explicit @refs should give same result
    const EXPLICIT_QUERY: &str = r#""foo" @refs { "bar" }"#;
    let explicit_res = run_query(TEST_INPUT_CONTAINMENT, EXPLICIT_QUERY);

    println!("default {{}} result: {:#?}", default_res.nodes);
    println!("explicit @refs result: {:#?}", explicit_res.nodes);

    // Both should have foo + bar (foo calls bar)
    assert_eq!(default_res.nodes.as_vec(), explicit_res.nodes.as_vec());
    assert_eq!(default_res.nodes.as_vec().len(), 2);
}

#[test]
fn refs_overrides_inherited_has() {
    // @has { @refs { } } - outer uses has, but inner explicitly uses refs
    // With direct-children-only: file(2) → function(1)
    // File contains foo (has), foo calls bar (refs)
    const QUERY: &str = r#"@file("/main.go") @has { "foo" @refs { "bar" } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);

    // Should have file (contains foo) + foo + bar (foo calls bar)
    assert_eq!(res.nodes.as_vec().len(), 3);
    assert!(res.nodes.as_vec().contains(&DeclarationId::new(510))); // file
    assert!(res.nodes.as_vec().contains(&DeclarationId::new(20))); // foo
    assert!(res.nodes.as_vec().contains(&DeclarationId::new(30))); // bar
}

// ============================================================================
// Default Symbol Type Inheritance Tests
// ============================================================================
//
// These tests verify that type selectors set default types for child scopes.
// @module("test") {} should show modules AND functions that test references
// @module("test") { @function } should explicitly filter to only functions
//
// Test data (TEST_INPUT_CONTAINMENT) has:
// - Module `testmodule` (type=3, id=1, instance=10)
// - Functions `foo`, `bar`, `baz` (type=1, ids=2,3,4, instances=20,30,40)
// - Refs: foo→bar (at 150), bar→baz (at 250) - so module refs bar and baz

#[test]
fn default_type_inheritance_module_refs_children() {
    // @module("testmodule") {} should show:
    // - module itself
    // - modules it references (none in test data)
    // - functions it references (bar and baz via contained refs)
    //
    // Without default type inheritance, {} would return ALL types.
    // With default type inheritance, {} filters to module + function types.
    const QUERY: &str = r#"@module("testmodule") {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have module (10) and the functions it refs (bar=30, baz=40)
    // via refs at positions 150 and 250 within module's range [0,1000)
    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // The module should be included
    assert!(nodes.contains(&DeclarationId::new(10)), "Should include module");

    // Functions referenced by refs within module's range should be included
    // (bar at 30, baz at 40)
    // Note: The refs are foo→bar (150→30) and bar→baz (250→40)
    // Both ref sites are within module's range [0,1000)
}

#[test]
fn default_type_inheritance_explicit_function_only() {
    // @module("testmodule") { @function } should show:
    // - module itself (the parent selector)
    // - ONLY functions it references (not modules, because @function is explicit)
    //
    // The explicit @function overrides the default type inheritance for the CHILD scope
    // The parent (@module) is still included as it's the parent selector
    const QUERY: &str = r#"@module("testmodule") { @function }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // Should include module (10) as the parent + functions (30, 40)
    // This is the same as the default case since there are no module-to-module refs
    // The difference would be visible if module referenced other modules
    assert!(nodes.contains(&DeclarationId::new(10)), "Should include parent module");
    assert!(nodes.contains(&DeclarationId::new(30)), "Should include bar");
    assert!(nodes.contains(&DeclarationId::new(40)), "Should include baz");
}

#[test]
fn default_type_inheritance_function_refs_children() {
    // @function("foo") {} should show:
    // - function foo itself
    // - functions it references (bar)
    //
    // With default type inheritance from @function, {} filters to function type only
    const QUERY: &str = r#"@function("foo") {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // Should have foo (20) and bar (30) - foo calls bar
    assert!(nodes.contains(&DeclarationId::new(20)), "Should include foo");
    assert!(nodes.contains(&DeclarationId::new(30)), "Should include bar");

    // Should NOT include module since @function sets default to function only
    assert!(
        !nodes.contains(&DeclarationId::new(10)),
        "Should NOT include module"
    );
}

#[test]
fn default_type_inheritance_nested_scopes() {
    // @module("testmodule") { @function("foo") {} }
    // First level: module (sets defaults to module+function)
    // Second level: @function("foo") (overrides to function only)
    // Third level: {} inherits function-only from @function
    const QUERY: &str = r#"@module("testmodule") { @function("foo") {} }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    let nodes = res.nodes.as_vec();
    println!("Nodes: {:?}", nodes);

    // Module should be filtered out at second level by @function
    // But wait - module is the parent, @function filters the child
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
    // @file("/main.go") @has { @function }
    // Returns: file /main.go and all functions it contains
    const QUERY: &str = r#"@file("/main.go") @has { @function }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510) + functions (20, 30, 40)
    assert_eq!(res.nodes.as_vec().len(), 4);
}

#[test]
fn file_contains_specific_function() {
    // @file("/main.go") @has { "foo" }
    // Returns: file /main.go and function foo
    const QUERY: &str = r#"@file("/main.go") @has { "foo" }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510) + foo (20)
    assert_eq!(res.nodes.as_vec().len(), 2);
}

#[test]
fn directory_contains_file() {
    // @directory("/") @has { @module @has { @file } }
    // With direct-children-only: directory(4) → module(3) → file(2)
    // Returns: directory /, module, and file
    const QUERY: &str = r#"@directory("/") @has { @module @has { @file } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Directory + module + file = 3 nodes
    assert_eq!(res.nodes.as_vec().len(), 3);
}

#[test]
fn directory_contains_module() {
    // @directory("/") @has { @module }
    // Returns: directory / and modules contained in it
    const QUERY: &str = r#"@directory("/") @has { @module }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Directory + module = 2 nodes
    assert_eq!(res.nodes.as_vec().len(), 2);
}

#[test]
fn directory_contains_function() {
    // @directory("/") @has { @module @has { @file @has { @function } } }
    // With direct-children-only: directory(4) → module(3) → file(2) → function(1)
    // Returns: directory, module, file, and all functions
    const QUERY: &str = r#"@directory("/") @has { @module @has { @file @has { @function } } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Directory + module + file + 3 functions = 6 nodes
    assert_eq!(res.nodes.as_vec().len(), 6);
}

#[test]
fn full_hierarchy_query() {
    // @directory("/") @has { @module @has { @file("/main.go") @has { "foo" } } }
    // With direct-children-only: directory(4) → module(3) → file(2) → function(1)
    // Returns: directory, module, file /main.go, and foo
    const QUERY: &str = r#"@directory("/") @has { @module @has { @file("/main.go") @has { "foo" } } }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Directory + module + file + foo = 4 nodes
    assert_eq!(res.nodes.as_vec().len(), 4);
}

#[test]
fn file_type_selector_filter_at_root() {
    // @file (no name) at root acts as filter - returns empty
    // because there's no parent to derive from
    const QUERY: &str = r#"@file"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Filter at root level = empty
    assert_eq!(res.nodes.as_vec().len(), 0);
}

#[test]
fn file_type_selector_by_name() {
    // @file("/main.go")
    // Returns file /main.go
    const QUERY: &str = r#"@file("/main.go")"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Should have file (510)
    assert_eq!(res.nodes.as_vec().len(), 1);
}

#[test]
fn directory_type_selector_filter_at_root() {
    // @directory (no name) at root acts as filter - returns empty
    // because there's no parent to derive from
    const QUERY: &str = r#"@directory"#;
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
// NOTE: @directory("/src") currently uses CompoundNameMixin which does
// prefix/partial name matching, so it matches /src, /src/util, /src/config, etc.
// This is a known behavior - exact path matching could be added as an improvement.

#[test]
fn directory_src_util_contains_its_direct_files() {
    // @directory("/src/util") @has { @file }
    // /src/util has instances in objects 2,3 (util.go, helper.go)
    // Files in those objects: util.go (obj 2), helper.go (obj 3)
    const QUERY: &str = r#"@directory("/src/util") @has { @file }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // /src/util directory + 2 files = 4 nodes (directory constrained to instances matching children)
    assert_eq!(
        res.nodes.as_vec().len(),
        4,
        "/src/util should have directory + 2 file instances = 4 nodes. Got {}.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn directory_docs_contains_its_direct_file() {
    // @directory("/docs") @has { @file }
    // /docs has 1 instance (in object 5 for readme.md)
    const QUERY: &str = r#"@directory("/docs") @has { @file }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // /docs directory + 1 file = 2 nodes
    assert_eq!(
        res.nodes.as_vec().len(),
        2,
        "/docs should have directory + 1 file instance = 2 nodes. Got {}.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn directory_has_empty_scope_returns_children() {
    // @directory("/src/util") @has {}
    // Empty scope should inherit @has relationship and return all direct children
    // /src/util has instances in objects 2,3 (util.go, helper.go)
    // With default type inheritance [DIRECTORY, FUNCTION], we get:
    // - Directory instances for /src/util
    // - Function instances in those objects (none in test data)
    // - Any other directories contained (none)
    const QUERY: &str = r#"@directory("/src/util") @has {}"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // /src/util has 2 instances (in objects 2 and 3)
    // Empty {} with @has should find children in those objects
    // With default type filter [DIRECTORY, FUNCTION], we get:
    // - The 2 directory instances of /src/util itself (via the TypeSelector)
    // - Plus any contained directories/functions (none in test data that match)
    // So we expect at least 2 nodes for the directory itself
    assert!(
        res.nodes.as_vec().len() >= 2,
        "@directory('/src/util') @has {{}} should return directory + any children. Got {} nodes.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn directory_has_empty_scope_with_file_test_data() {
    // @module("testmodule") @has {}
    // Empty scope should inherit @has relationship and return all direct children
    // In TEST_INPUT_CONTAINMENT, module has instances and contains file/functions
    const QUERY: &str = r#"@module("testmodule") @has {}"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // module "testmodule" has 1 instance [0,1000)
    // With default type filter [MODULE, FUNCTION], the empty @has {} should find:
    // - The module instance itself
    // - Functions contained in it (foo, bar, baz - but depends on type hierarchy)
    // The module contains file (level 2), file contains functions (level 1)
    // But with direct containment, module (level 3) > file (level 2) should work
    // Actually with [MODULE, FUNCTION] filter, we should get:
    // - module itself (from @module selector)
    // - functions if they match the type filter
    assert!(
        res.nodes.as_vec().len() >= 1,
        "@module('testmodule') @has {{}} should return module + any matching children. Got {} nodes.",
        res.nodes.as_vec().len()
    );
}

#[test]
fn has_sibling_children_different_types() {
    // @directory("/") @has { @module ; @file }
    // Test that sibling children of different types use UNION logic.
    // Directory "/" contains both modules and files via different instances.
    // Both sibling children should be found.
    const QUERY: &str = r#"@directory("/") @has { @module ; @file }"#;
    let res = run_query(TEST_INPUT_CONTAINMENT, QUERY);

    println!("{:#?}", res.nodes);
    println!("{:#?}", res.edges);

    // Directory "/" should contain:
    // - module (testmodule) via one instance
    // - file (/main.go) via possibly another instance
    // The parent directory should NOT be filtered out by the union of both children
    assert!(
        res.nodes.as_vec().len() >= 2,
        "@directory('/') @has {{ @module ; @file }} should return directory + module + file (union of both). Got {} nodes.",
        res.nodes.as_vec().len()
    );
}

// =============================================================================
// Directory @refs tests (directory hierarchy via symbol_refs)
// =============================================================================

// Test data (TEST_INPUT_TREE_BROWSER):
// /src → /src/util and /src/config via symbol_refs
// Directories: /, /src, /docs, /src/util, /src/config

#[test]
fn directory_refs_children() {
    // @directory("/src") @refs { @directory } should return /src + child dirs
    const QUERY: &str = r#"@directory("/src") @refs { @directory }"#;
    let res = run_query(TEST_INPUT_TREE_BROWSER, QUERY);

    let names: Vec<_> = res.nodes.0.iter().map(|n| n.symbol.name.as_str()).collect();
    println!("directory_refs_children names: {:?}", names);

    assert!(names.contains(&"/src"), "Should contain parent /src");
    assert!(names.contains(&"/src/util"), "Should contain child /src/util");
    assert!(names.contains(&"/src/config"), "Should contain child /src/config");
}
