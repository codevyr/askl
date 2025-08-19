use index::{
    db,
    db_diesel::Index,
    symbols::{DeclarationId, SymbolMap},
};

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, verb::*};

use std::collections::HashMap;

#[tokio::test]
async fn test_select_matching_name() {
    let index_diesel = Index::new_in_memory().await.unwrap();
    let index = db::Index::new_in_memory().await.unwrap();
    index.load_test_input("verb_test.sql").await.unwrap();
    index_diesel.load_test_input("verb_test.sql").await.unwrap();
    let symbols = SymbolMap::from_index(&index).await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(symbols, index_diesel);

    let test_cases = vec![
        ("foo", vec![91, 92]),
        ("bar", vec![92]),
        ("foo.bar", vec![92]),
        ("FOO.bar", vec![]),
    ];

    let mut ctx = ExecutionContext::new(); // Assuming there's a default constructor

    for (name, expected_ids) in test_cases {
        let named_args = HashMap::from([("name".to_string(), name.to_string())]);
        let selector = NameSelector::new(&vec![], &named_args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select_from_all(&mut ctx, &cfg)
            .await
            .unwrap();

        let mut got_declarations: Vec<DeclarationId> = result
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
