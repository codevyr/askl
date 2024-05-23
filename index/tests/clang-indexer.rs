use dotenv::dotenv;
use std::env;

use index::{
    db::Symbol,
    symbols::{DeclarationId, FileId, SymbolId, SymbolScope, SymbolType},
};

use index::clang::{run_clang_ast, CompileCommand, GlobalVisitorState};
use index::db::{Declaration, File, Index, Reference};

async fn index_files(files: Vec<&str>) -> GlobalVisitorState {
    dotenv().ok();
    env_logger::init();

    let index = Index::new_in_memory().await.unwrap();
    let mut state = GlobalVisitorState::new(index);

    let symbols = state.get_index().all_symbols().await.unwrap();
    assert!(symbols.is_empty());

    let clang = std::env::var("CLANG_PATH")
        .ok()
        .or(Some("clang".to_string()))
        .unwrap();
    let test_directory_rel = std::env::var("TEST_DIR")
        .ok()
        .or(Some("tests/clang-indexer-code".to_string()))
        .unwrap();
    let mut test_directory = env::current_dir().unwrap();
    println!("{}", test_directory.display());
    test_directory.push(test_directory_rel);
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
fn mask_declaration(symbol: &Declaration) -> Declaration {
    Declaration {
        id: DeclarationId::invalid(),
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

    let declarations = state.get_index().all_declarations().await.unwrap();
    for declaration in declarations.iter() {
        println!("Declaration: {:?}", declaration);
    }

    let did = DeclarationId::invalid();
    let fid = FileId::new(1);
    let expected_declarations = [
        Declaration::new_nolines(did, SymbolId::new(1), fid, SymbolType::Declaration), // foo
        Declaration::new_nolines(did, SymbolId::new(1), fid, SymbolType::Definition),  // foo
        Declaration::new_nolines(did, SymbolId::new(2), fid, SymbolType::Definition),  // bar
        Declaration::new_nolines(did, SymbolId::new(3), fid, SymbolType::Declaration), // zar
        Declaration::new_nolines(did, SymbolId::new(4), fid, SymbolType::Definition),  // main
        Declaration::new_nolines(did, SymbolId::new(5), fid, SymbolType::Declaration), // tar
        Declaration::new_nolines(did, SymbolId::new(5), fid, SymbolType::Declaration), // tar
        Declaration::new_nolines(did, SymbolId::new(5), fid, SymbolType::Definition),  // tar
        Declaration::new_nolines(did, SymbolId::new(3), fid, SymbolType::Definition),  // zar
    ];
    for (i, o) in declarations.iter().enumerate() {
        assert_eq!(mask_declaration(o), expected_declarations[i]);
    }
    assert_eq!(declarations.len(), expected_declarations.len());

    let refs = state.get_index().all_refs().await.unwrap();
    for reference in refs.iter() {
        println!("Reference: {:?}", reference);
    }
    let expected_refs = [
        new_ref(4, 3), // main to zar
        new_ref(4, 1), // main to foo
        new_ref(4, 5), // main to tar
        new_ref(4, 2), // main to bar
    ];
    for (i, s) in refs.iter().enumerate() {
        assert_eq!(mask_ref(&s), expected_refs[i]);
    }
    assert_eq!(refs.len(), expected_refs.len());
}
