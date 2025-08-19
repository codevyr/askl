use crate::symbols::{partial_name_match, Symbol, SymbolId};

#[test]
fn test_partial_name_matcher() {
    let test_cases = vec![
        // (symbol_name, search_term, should_match)
        ("foo", "foo", true),
        ("bar.foo", "foo", true),
        ("bar.foo", "bar.foo", true),
        ("zar/bar.foo", "bar.foo", true),
        ("foo.bar", "bar.foo", false),
        ("barfoo", "foo", false),
        (
            "(*k8s.io/kubelet/pkg/apis/deviceplugin/v1beta1.devicePluginClient).Allocate",
            "devicePluginClient.Allocate",
            true,
        ),
    ];

    for (symbol_name, search_term, expected_match) in test_cases {
        let id = SymbolId::new(1);
        let sym = Symbol::new(id, symbol_name.to_string());

        let matcher = partial_name_match(search_term);
        let matched_symbol = matcher((&id, &sym));

        assert_eq!(
            matched_symbol.is_some(),
            expected_match,
            "Symbol '{}' with search term '{}' should{} match",
            symbol_name,
            search_term,
            if expected_match { "" } else { " not" }
        );
    }
}

#[tokio::test]
async fn test_find_symbol_by_name() -> anyhow::Result<()> {
    use crate::db_diesel::Index;

    let index = Index::new_in_memory().await?;

    // Test with empty database first
    let empty_selection = index.find_symbol_by_name(&["nonexistent"]).await?;
    assert!(empty_selection.nodes.is_empty());
    assert!(empty_selection.parents.is_empty());
    assert!(empty_selection.children.is_empty());

    // Load test data
    index.load_test_input(Index::TEST_INPUT_A).await?;

    // Test searching for symbols - use "a" which we know exists
    let selection = index.find_symbol_by_name(&["a"]).await?;
    assert!(
        !selection.nodes.is_empty(),
        "Should find symbols with 'a' in the name"
    );

    // Test searching for symbols - use "main" which we know exists
    let selection = index.find_symbol_by_name(&["main"]).await?;
    assert!(
        !selection.nodes.is_empty(),
        "Should find symbols with 'main' in the name"
    );
    assert_eq!(
        selection.children.len(),
        2,
        "Should have two children for 'main'"
    );
    assert_eq!(
        selection.parents.len(),
        0,
        "Should have no parents for 'main'"
    );

    // Verify that DeclarationFull is properly populated
    for s in &selection.nodes {
        assert!(!s.symbol.name.is_empty(), "Symbol name should not be empty");
        // The file field should be properly populated
        assert!(
            !s.file.filesystem_path.is_empty(),
            "File path should not be empty"
        );
        assert!(
            !s.module.module_name.is_empty(),
            "Module name should not be empty"
        );
    }

    // Test compound name search
    let compound_selection = index.find_symbol_by_name(&["mai", "n"]).await?;
    assert!(
        compound_selection.nodes.is_empty(),
        "Should find no symbols with compound name search"
    );

    Ok(())
}
