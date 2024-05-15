use std::env;

use askl::{
    index::{File, Index, Reference, Symbol},
    indexer::clang::{run_clang_ast, CompileCommand, GlobalVisitorState},
    symbols::{FileId, SymbolId, SymbolType},
};

async fn index_files(files: Vec<&str>) -> GlobalVisitorState {
    let index = Index::new_in_memory().await.unwrap();
    let mut state = GlobalVisitorState::new(index);

    let symbols = state.get_index().all_symbols().await.unwrap();
    assert!(symbols.is_empty());

    let clang = "clang";
    let mut test_directory = env::current_dir().unwrap();
    test_directory.push("tests/clang-indexer-code");
    for test_file in files {
        let command = CompileCommand {
            arguments: Some(vec![
                clang.to_string(),
                "-Wno-implicit-function-declaration".to_string(),
                test_file.to_string(),
            ]),
            command: None,
            directory: test_directory.to_str().unwrap().to_string(),
            file: test_file.to_string(),
            output: Some("/dev/null".to_string()),
        };

        log::debug!("{:?}", command);

        let (ast_file, node) = run_clang_ast(&clang, command).await.unwrap();

        log::debug!("{}", ast_file);

        state.extract_symbol_map_root(node).await.unwrap();
    }

    state
}

/// We do not need all fields for comparison
fn mask_symbol(symbol: &Symbol) -> Symbol {
    Symbol {
        id: symbol.id,
        name: symbol.name.clone(),
        file_id: symbol.file_id,
        symbol_type: symbol.symbol_type,
        line_start: 1,
        col_start: 1,
        line_end: 1,
        col_end: 1,
    }
}

fn new_symbol(sym_id: i32, name: &str, file_id: i32, symbol_type: SymbolType) -> Symbol {
    Symbol::new_nolines(SymbolId(sym_id), name, FileId::new(file_id), symbol_type)
}

fn mask_ref(reference: &Reference) -> Reference {
    Reference {
        from_symbol: reference.from_symbol,
        to_symbol: reference.to_symbol,
        from_line: 1,
        from_col_start: 1,
        from_col_end: 1,
    }
}

fn new_ref(from_symbol: i32, to_symbol: i32) -> Reference {
    Reference {
        from_symbol: SymbolId(from_symbol),
        to_symbol: SymbolId(to_symbol),
        from_line: 1,
        from_col_start: 1,
        from_col_end: 1,
    }
}

#[tokio::test]
async fn create_state() {
    let state = index_files(vec!["test1.c"]).await;

    let files = state.get_index().all_files().await.unwrap();
    log::debug!("{:?}", files);
    assert_eq!(
        files[0],
        File::new(FileId::new(1), "test1.c", "test", "cxx")
    );

    let symbols = state.get_index().all_symbols().await.unwrap();
    log::debug!("{:#?}", symbols);

    let expected_symbols = [
        new_symbol(1, "foo", 1, SymbolType::Declaration),
        new_symbol(2, "foo", 1, SymbolType::Definition),
        new_symbol(3, "bar", 1, SymbolType::Definition),
        new_symbol(4, "main", 1, SymbolType::Definition),
        new_symbol(5, "tar", 1, SymbolType::Declaration),
        new_symbol(6, "tar", 1, SymbolType::Declaration),
        new_symbol(7, "tar", 1, SymbolType::Definition),
    ];
    for (i, s) in symbols.iter().enumerate() {
        assert_eq!(mask_symbol(&s), expected_symbols[i]);
    }
    assert_eq!(symbols.len(), expected_symbols.len());

    let refs = state.get_index().all_refs().await.unwrap();
    log::debug!("{:#?}", refs);
    let expected_refs = [new_ref(4, 3)];
    assert_eq!(refs.len(), expected_refs.len());
    for (i, s) in refs.into_iter().enumerate() {
        assert_eq!(mask_ref(&s), expected_refs[i]);
    }

    state.resolve_global_symbols().await.unwrap();

    let refs = state.get_index().all_refs().await.unwrap();
    log::debug!("{:#?}", refs);
    let expected_refs = [new_ref(4, 3), new_ref(4, 1), new_ref(4, 2)];
    assert_eq!(refs.len(), expected_refs.len());
    for (i, s) in refs.into_iter().enumerate() {
        assert_eq!(mask_ref(&s), expected_refs[i]);
    }
}
