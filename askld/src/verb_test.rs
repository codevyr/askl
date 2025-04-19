use clang_ast::SourceRange;
use index::{
    db::Index,
    symbols::{self, DeclarationId, DeclarationRefs, FileId, Occurrence, SymbolMap},
};

use crate::{cfg::ControlFlowGraph, execution_context::ExecutionContext, verb::*};

use std::{
    collections::{HashMap, HashSet},
    sync::Arc,
};

#[tokio::test]
async fn test_select_matching_name() {
    let location = clang_ast::SourceLocation {
        spelling_loc: None,
        expansion_loc: Some(clang_ast::BareSourceLocation {
            line: 1,
            col: 2,
            offset: 0,
            file: Arc::from(String::from("main")),
            presumed_file: None,
            presumed_line: None,
            tok_len: 1,
            included_from: None,
            is_macro_arg_expansion: false,
        }),
    };
    let source_range = SourceRange {
        begin: location.clone(),
        end: location,
    };
    let index = Index::new_in_memory().await.unwrap();
    index.load_test_input("verb_test.sql").await.unwrap();
    let symbols = SymbolMap::from_index(&index).await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(symbols, index);

    let declarations = cfg.index.all_declarations().await.unwrap();
    let mut declaration_refs: DeclarationRefs = HashMap::new();
    declarations.into_iter().for_each(|d| {
        declaration_refs.insert(d.id, HashSet::new());
    });

    let named_args = HashMap::from([("name".to_string(), "foo".to_string())]);
    let selector = NameSelector::new(&vec![], &named_args).unwrap();
    let mut ctx = ExecutionContext::new(); // Assuming there's a default constructor

    let result = selector
        .as_selector()
        .unwrap()
        .select(&mut ctx, &cfg, declaration_refs)
        .unwrap();

    let mut got_declarations: Vec<DeclarationId> = result.into_keys().collect();
    got_declarations.sort();

    let expected_declarations: Vec<DeclarationId> = vec![91, 92, 93]
        .into_iter()
        .map(|i| DeclarationId::new(i))
        .collect();

    assert_eq!(got_declarations, expected_declarations)
}
