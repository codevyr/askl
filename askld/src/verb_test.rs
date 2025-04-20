use index::{
    db::Index,
    symbols::{DeclarationId, DeclarationRefs, SymbolMap},
};

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, verb::*};

use std::collections::{HashMap, HashSet};

#[tokio::test]
async fn test_select_matching_name() {
    let index = Index::new_in_memory().await.unwrap();
    index.load_test_input("verb_test.sql").await.unwrap();
    let symbols = SymbolMap::from_index(&index).await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(symbols, index);

    let declarations = cfg.index.all_declarations().await.unwrap();
    let mut declaration_refs: DeclarationRefs = HashMap::new();
    declarations.into_iter().for_each(|d| {
        declaration_refs.insert(d.id, HashSet::new());
    });

    let test_cases = vec![
        ("foo", vec![91, 92]),
        ("bar", vec![92]),
        // ("foo.bar", vec![92]),
    ];

    let mut ctx = ExecutionContext::new(); // Assuming there's a default constructor

    for (name, expected_ids) in test_cases {
        let named_args = HashMap::from([("name".to_string(), name.to_string())]);
        let selector = NameSelector::new(&vec![], &named_args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select(&mut ctx, &cfg, declaration_refs.clone())
            .unwrap();

        let mut got_declarations: Vec<DeclarationId> = result.into_keys().collect();
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
