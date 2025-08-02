use crate::test_util::{format_edges, run_query, run_query_err, TEST_INPUT_A, TEST_INPUT_B};
use index::symbols::DeclarationId;

#[test]
fn label_use_syntax_check() {
    const QUERY: &str = r#""b" "a" {@label("foo")}; @use("foo")"#;
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
fn label_use_forced() {
    const QUERY: &str = r#""main" @label("foo") {}; "b" {@use("foo", forced="true")}"#;
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
    assert_eq!(edges, vec!["91-92", "91-92", "92-942", "942-91", "942-92"]);
}
