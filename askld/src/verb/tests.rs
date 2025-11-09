use index::{db_diesel::Index, symbols::DeclarationId};

use crate::{
    cfg::ControlFlowGraph, execution_context::ExecutionContext, test_util::run_query, verb::*,
};

use std::collections::HashMap;

#[tokio::test]
async fn test_select_matching_name() {
    let index_diesel = Index::new_in_memory().await.unwrap();
    index_diesel.load_test_input("verb_test.sql").await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(index_diesel);

    let test_cases = vec![
        ("foo", vec![91, 92]),
        ("bar", vec![92]),
        ("foo.bar", vec![92]),
        ("FOO.bar", vec![]),
        ("FOO", vec![]),
    ];

    let mut ctx = ExecutionContext::new(); // Assuming there's a default constructor

    for (name, expected_ids) in test_cases {
        let named_args = HashMap::from([("name".to_string(), name.to_string())]);
        let selector = NameSelector::new(&vec![], &named_args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select_from_all(&mut ctx, &cfg, vec![])
            .await
            .unwrap();

        let mut got_declarations: Vec<DeclarationId> = result
            .unwrap()
            .nodes
            .into_iter()
            .map(|s| DeclarationId::new(s.declaration.id))
            .collect();
        got_declarations.sort();

        let expected_declarations: Vec<DeclarationId> = expected_ids
            .into_iter()
            .map(|i| DeclarationId::new(i))
            .collect();

        assert_eq!(
            got_declarations, expected_declarations,
            "Failed for name: {}",
            name
        );
    }
}

#[test]
fn test_ignore_package_filter() {
    let query = r#"
@preamble
@ignore(package="foo");
"foo";
"foo.bar";
"foobar";
"tar";
"#;

    let (res_nodes, _res_edges) = run_query("verb_test.sql", query);

    assert_eq!(
        res_nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(93),
            DeclarationId::new(94),
        ]
    );
}
