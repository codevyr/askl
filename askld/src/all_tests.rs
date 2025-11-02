use crate::test_util::{
    format_edges, run_query, run_query_err, TEST_INPUT_A, TEST_INPUT_B, TEST_INPUT_MODULES,
};
use index::symbols::DeclarationId;

#[test]
fn single_node_query() {
    env_logger::init();

    const QUERY: &str = r#""a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(res_nodes.as_vec(), vec![DeclarationId::new(91)]);
    assert_eq!(res_edges.0.len(), 0);
}

#[test]
fn single_child_query() {
    const QUERY: &str = r#""a"{}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn single_parent_query() {
    const QUERY: &str = r#"{"a"}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(942)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["942-91"]);
}

#[test]
fn double_parent_query() {
    const QUERY: &str = r#"{{"b"}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(942)
        ]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92", "942-91", "942-92"]);
}

#[test]
fn missing_child_query() {
    // "a" does not have grandchildren, so this should return no results
    const QUERY: &str = r#""a"{{}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn no_selectors() {
    const QUERY: &str = r#"{{}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn forced_query() {
    // Forcing a node without any selectors should return no results
    const QUERY: &str = r#"!"a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    assert_eq!(res_edges.0.len(), 0);
}

#[test]
fn forced_child_query_1() {
    const QUERY: &str = r#""b"{!"a"}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92", "92-91"]);
}

#[test]
fn forced_child_query_2() {
    const QUERY: &str = r#""b"{!"c"}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(92), DeclarationId::new(93)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["92-93"]);
}

#[test]
fn forced_child_query_3() {
    const QUERY: &str = r#""main" {
            !"c"
        }"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(93), DeclarationId::new(942)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["942-93"]);
}

#[test]
fn forced_child_query_4() {
    const QUERY: &str = r#""a"{!"g"}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(97)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-97"]);
}

#[test]
fn forced_child_query_5() {
    const QUERY: &str = r#""main"{{!"g"}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(97),
            DeclarationId::new(942),
        ]
    );
    let edges = format_edges(res_edges);
    assert_eq!(
        edges,
        vec!["91-92", "91-92", "91-97", "92-97", "942-91", "942-92"]
    );
}

#[test]
fn forced_child_query_6() {
    const QUERY: &str = r#""a" "b"{{!"g"}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(97),
        ]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92", "92-97"]);
}

#[test]
fn generic_forced_child_query_3() {
    const QUERY: &str = r#""main" {
            @forced(name="c")
        }"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(93), DeclarationId::new(942)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["942-93"]);
}

#[test]
fn two_selectors() {
    const QUERY: &str = r#""b" "a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92),]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn two_selectors_children() {
    const QUERY: &str = r#""b" "a" {}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92),]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn statement_after_scope() {
    const QUERY: &str = r#""a" {}; "a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92),]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn statement_after_scope_newline() {
    const QUERY: &str = r#""a" {}
        "a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92),]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn ignore_node_no_result() {
    const QUERY: &str = r#""a" {@ignore("b")}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_sibling() {
    const QUERY: &str = r#""d" {@ignore("e")}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(94), DeclarationId::new(96)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["94-96"]);
}

#[test]
fn ignore_node_parent_no_result() {
    const QUERY: &str = r#"@ignore("d") {"e"}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_node_wrong_parent() {
    const QUERY: &str = r#"@ignore("a") {"e"}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(94), DeclarationId::new(95)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["94-95"]);
}

#[test]
fn ignore_node_recurse() {
    // Ignore applies to all children, so this should return no results
    const QUERY: &str = r#""a" @ignore("b") {}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn ignore_another_statement() {
    // Ignore applies to all children, so this should return no results
    const QUERY: &str = r#"@preamble @ignore("b") ; "a" {}; "a" {}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
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
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(94),
            DeclarationId::new(95),
            DeclarationId::new(96),
            DeclarationId::new(97),
        ]
    );
    let edges = format_edges(res_edges);

    // This test requires dependency tracking to pass, so let it fail for now
    assert_eq!(edges, vec!["94-95", "94-96", "96-97"]);
}

#[test]
fn statement_semicolon() {
    const QUERY: &str = r#""d" {"f";}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(94), DeclarationId::new(96),]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["94-96"]);
}

#[test]
fn two_statements() {
    // We connect all nodes, unless they are explicitly isolated into different scopes
    const QUERY: &str = r#""a"; "b""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92"]);
}

#[test]
fn project_double_parent_query() {
    const QUERY: &str = r#"@module("test") {{"b"}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(92),
            DeclarationId::new(942)
        ]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-92", "91-92", "942-91", "942-92"]);
}

#[test]
fn module_filter_excludes_other_modules() {
    const FILTERED_QUERY: &str = r#"@module("test") "a""#;
    let (filtered_nodes, filtered_edges) = run_query(TEST_INPUT_MODULES, FILTERED_QUERY);

    println!("{:#?}", filtered_nodes);
    println!("{:#?}", filtered_edges);

    assert_eq!(filtered_nodes.as_vec(), vec![DeclarationId::new(91)]);
    assert_eq!(filtered_edges.0.len(), 0);

    const UNFILTERED_QUERY: &str = r#""a""#;
    let (unfiltered_nodes, _) = run_query(TEST_INPUT_MODULES, UNFILTERED_QUERY);
    let unfiltered_nodes = unfiltered_nodes.as_vec();

    assert_eq!(
        unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"@module("test") "a"; "a""#;
    let (filtered_unfiltered_nodes, _) =
        run_query(TEST_INPUT_MODULES, FILTERED_AND_UNFILTERED_QUERY);
    let filtered_unfiltered_nodes = filtered_unfiltered_nodes.as_vec();

    assert_eq!(
        filtered_unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const PREAMBLE_FILTERED_QUERY: &str = r#"@preamble @module("test"); "a""#;
    let (preamble_filtered_nodes, _) = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered_nodes.as_vec();

    assert_eq!(preamble_filtered_nodes, vec![DeclarationId::new(91)]);
}

#[test]
fn module_filter_selects_other_module() {
    const QUERY: &str = r#"@module("other") "a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![DeclarationId::new(201)]);
    assert_eq!(res_edges.0.len(), 0);
}

#[test]
fn project_filter_excludes_other_projects() {
    const FILTERED_QUERY: &str = r#"@project("test_project") "a""#;
    let (filtered_nodes, filtered_edges) = run_query(TEST_INPUT_MODULES, FILTERED_QUERY);

    println!("{:#?}", filtered_nodes);
    println!("{:#?}", filtered_edges);

    assert_eq!(
        filtered_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(201)]
    );
    assert_eq!(filtered_edges.0.len(), 0);

    const UNFILTERED_QUERY: &str = r#""a""#;
    let (unfiltered_nodes, _) = run_query(TEST_INPUT_MODULES, UNFILTERED_QUERY);
    let unfiltered_nodes = unfiltered_nodes.as_vec();

    assert_eq!(
        unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const FILTERED_AND_UNFILTERED_QUERY: &str = r#"@project("test_project") "a"; "a""#;
    let (filtered_unfiltered_nodes, _) =
        run_query(TEST_INPUT_MODULES, FILTERED_AND_UNFILTERED_QUERY);
    let filtered_unfiltered_nodes = filtered_unfiltered_nodes.as_vec();

    assert_eq!(
        filtered_unfiltered_nodes,
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );

    const PREAMBLE_FILTERED_QUERY: &str = r#"@preamble @project("test_project"); "a""#;
    let (preamble_filtered_nodes, _) = run_query(TEST_INPUT_MODULES, PREAMBLE_FILTERED_QUERY);
    let preamble_filtered_nodes = preamble_filtered_nodes.as_vec();

    assert_eq!(
        preamble_filtered_nodes,
        vec![DeclarationId::new(91), DeclarationId::new(201)]
    );

    const REPLACE_PROJECT_FILTERED_QUERY: &str = r#"@project("adsf") @project("test_project") "a""#;
    let (replace_project_filtered_nodes, _) =
        run_query(TEST_INPUT_MODULES, REPLACE_PROJECT_FILTERED_QUERY);
    let replace_project_filtered_nodes = replace_project_filtered_nodes.as_vec();

    assert_eq!(
        replace_project_filtered_nodes,
        vec![DeclarationId::new(91), DeclarationId::new(201)]
    );
}

#[test]
fn project_filter_selects_other_project() {
    const QUERY: &str = r#"@project("other_project") "a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![DeclarationId::new(301)]);
    assert_eq!(res_edges.0.len(), 0);

    const WRONG_PROJECT_QUERY: &str = r#"@project("blablabla_project") "a""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_MODULES, WRONG_PROJECT_QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    assert_eq!(res_edges.0.len(), 0);
}

#[test]
fn multiple_projects_with_forced() {
    const QUERY: &str = r#"@project("test_project") "a" { @project("other_project") !"a" }"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_MODULES, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(201),
            DeclarationId::new(301)
        ]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["91-301", "201-301"]);
}

#[test]
fn implicit_edge() {
    const QUERY: &str = r#""d" {}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(86),
            DeclarationId::new(94),
            DeclarationId::new(95),
            DeclarationId::new(96)
        ]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, vec!["94-86", "94-95", "94-96", "95-86", "95-96"]);
}

#[test]
fn multiple_selectors() {
    const QUERY: &str = r#""a" "c" { {"d" {}}}"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_B, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
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
    let edges = format_edges(res_edges);
    assert_eq!(
        edges,
        vec!["91-92", "92-94", "93-92", "94-86", "94-95", "94-96", "95-86", "95-96"]
    );
}

#[test]
fn preamble() {
    const QUERY: &str = r#"@preamble"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_empty_commands() {
    const QUERY: &str = r#";;;;;@preamble"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_second_command() {
    const QUERY: &str = r#""a";;;;;@preamble"#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![DeclarationId::new(91)]);
    let edges = format_edges(res_edges);
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
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(res_nodes.as_vec(), vec![]);
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}
