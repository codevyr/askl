use std::env;

use index::{
    db::Symbol,
    symbols::{FileId, SymbolId, SymbolScope, SymbolType},
};

use index::clang::{run_clang_ast, CompileCommand, GlobalVisitorState};
use index::db::{File, Index, Occurrence, Reference};

async fn index_files(files: Vec<&str>) -> GlobalVisitorState {
    let index = Index::new_in_memory().await.unwrap();
    let mut state = GlobalVisitorState::new(index);

    let symbols = state.get_index().all_symbols().await.unwrap();
    assert!(symbols.is_empty());

    let clang = "clang";
    let clang = "/nix/store/2n2ranlijkkab8xqb1y0bha8mhl6j2gk-clang-wrapper-17.0.6/bin/clang";
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

        state
            .extract_symbol_map_root(test_file, node)
            .await
            .unwrap();
    }

    state
}

/// We do not need all fields for comparison
fn mask_occurrence(symbol: &Occurrence) -> Occurrence {
    Occurrence {
        symbol: symbol.symbol,
        // name: symbol.name.clone(),
        file_id: symbol.file_id,
        symbol_type: symbol.symbol_type,
        line_start: 1,
        col_start: 1,
        line_end: 1,
        col_end: 1,
    }
}

fn new_symbol(
    sym_id: i32,
    name: &str,
    module_id: Option<i32>,
    symbol_scope: SymbolScope,
) -> Symbol {
    let module_id = if let Some(module_id) = module_id {
        Some(FileId::new(module_id))
    } else {
        None
    };
    Symbol::new(SymbolId(sym_id), name, module_id, symbol_scope)
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
    for symbol in symbols.iter() {
        println!("Symbols: {:?}", symbol);
    }

    let expected_symbols = [
        new_symbol(1, "foo", None, SymbolScope::Global),
        new_symbol(2, "bar", Some(1), SymbolScope::Local),
        new_symbol(3, "zar", Some(1), SymbolScope::Local),
        new_symbol(4, "main", None, SymbolScope::Global),
        new_symbol(5, "tar", None, SymbolScope::Global),
    ];
    for (i, s) in symbols.iter().enumerate() {
        assert_eq!(*s, expected_symbols[i]);
    }
    assert_eq!(symbols.len(), expected_symbols.len());

    let occurrences = state.get_index().all_occurrences().await.unwrap();
    for occurrence in occurrences.iter() {
        println!("Occurrence: {:?}", occurrence);
    }

    let expected_occurrences = [
        Occurrence::new_nolines(SymbolId::new(1), FileId::new(1), SymbolType::Declaration), // foo
        Occurrence::new_nolines(SymbolId::new(1), FileId::new(1), SymbolType::Definition),  // foo
        Occurrence::new_nolines(SymbolId::new(2), FileId::new(1), SymbolType::Definition),  // bar
        Occurrence::new_nolines(SymbolId::new(3), FileId::new(1), SymbolType::Declaration), // zar
        Occurrence::new_nolines(SymbolId::new(4), FileId::new(1), SymbolType::Definition),  // main
        Occurrence::new_nolines(SymbolId::new(5), FileId::new(1), SymbolType::Declaration), // tar
        Occurrence::new_nolines(SymbolId::new(5), FileId::new(1), SymbolType::Declaration), // tar
        Occurrence::new_nolines(SymbolId::new(5), FileId::new(1), SymbolType::Definition), // tar
        Occurrence::new_nolines(SymbolId::new(3), FileId::new(1), SymbolType::Definition), // zar
    ];
    for (i, o) in occurrences.iter().enumerate() {
        assert_eq!(mask_occurrence(o), expected_occurrences[i]);
    }
    assert_eq!(occurrences.len(), expected_occurrences.len());

    let refs = state.get_index().all_refs().await.unwrap();
    log::debug!("{:#?}", refs);
    let expected_refs = [
        new_ref(5, 4), // main to zar
        new_ref(5, 9), // main to zar
        new_ref(5, 3), // main to bar
    ];
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
