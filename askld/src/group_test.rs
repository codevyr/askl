/// Tests for the `@scope` query modifier in the Askld query language. This
/// module contains tests that verify the behavior of isolated scopes, nested
/// scopes, and the handling of isolated nodes within scopes.
///
/// The tests are disabled for the time being, as I intend to refactor the
// `@scope` into a more general `@group` modifier. In particular, it allows to
// avoid confusion between the `@scope` modifier and the `scope` object in the
// AST. But also I intend to change how groups behave.
use crate::test_util::{format_edges, run_query, TEST_INPUT_A};
use index::symbols::DeclarationId;

#[test]
fn single_isolated_scope() {
    const QUERY: &str = r#"@scope{{"e"}}"#;
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
fn double_isolated_scope() {
    const QUERY: &str = r#"@scope{@scope{{"e"}}}"#;
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
fn isolated_scopes() {
    // We connect all nodes, unless they are explicitly isolated into different scopes
    const QUERY: &str = r#"@scope(isolated="true"); "a"; "b""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn implicitly_isolated_scopes() {
    // We connect all nodes, unless they are explicitly isolated into different scopes
    const QUERY: &str = r#"@scope{"a"}; "b""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);
    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}

#[test]
fn preamble_isolated_scope_with_nodes() {
    const QUERY: &str = r#"@preamble @scope(isolated="true"); "a"; "b""#;
    let (res_nodes, res_edges) = run_query(TEST_INPUT_A, QUERY);

    println!("{:#?}", res_nodes);
    println!("{:#?}", res_edges);

    assert_eq!(
        res_nodes.as_vec(),
        vec![DeclarationId::new(91), DeclarationId::new(92)]
    );
    let edges = format_edges(res_edges);
    assert_eq!(edges, Vec::<String>::new());
}
