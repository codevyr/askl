use dotenv::dotenv;
use std::{env, path::Path};

use index::{
    db::{Module, Symbol},
    symbols::{
        self, SymbolInstanceId, FileId, ModuleId, Occurrence, ProjectId, SymbolId, SymbolScope,
        SymbolType,
    },
};

use anyhow::Result;
use clang::clang::{run_clang_ast, CompileCommand, GlobalVisitorState};
use index::db::{SymbolInstance, Index, Reference};
use std::collections::HashMap;

async fn index_files(files: Vec<&str>, module: &Module) -> GlobalVisitorState {
    dotenv().ok();
    env_logger::init();

    let index = Index::new_in_memory().await.unwrap();

    let module_res = index
        .create_or_get_module(&module.module_name)
        .await
        .unwrap();

    assert_eq!(
        module_res, module.id,
        "Expected newly created module to have id {}, found {}",
        module.id, module_res
    );
    let mut state = GlobalVisitorState::new(index, module.id);

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

        let node = run_clang_ast(&clang, command).await.unwrap();

        state.extract_symbol_map_root(node).await.unwrap();
    }

    state
}

/// We do not need all fields for comparison
fn mask_instance(symbol: &SymbolInstance) -> SymbolInstance {
    SymbolInstance {
        id: SymbolInstanceId::invalid(),
        symbol: symbol.symbol,
        // name: symbol.name.clone(),
        file_id: symbol.file_id,
        symbol_type: symbol.symbol_type,
        offset_range: (0, 0),
    }
}

fn mask_ref(reference: &Reference) -> Reference {
    Reference {
        from_symbol_instance: reference.from_symbol_instance,
        to_symbol: reference.to_symbol,
        from_file: reference.from_file,
        from_offset_start: 0,
        from_offset_end: 0,
    }
}

fn new_ref(from_symbol_instance: i32, to_symbol: i32) -> Reference {
    Reference {
        from_symbol_instance: SymbolInstanceId::new(from_symbol_instance),
        to_symbol: SymbolId(to_symbol),
        from_file: FileId::new(1),
        from_offset_start: 0,
        from_offset_end: 0,
    }
}

#[ignore]
#[tokio::test]
async fn create_state() {
    let current_dir = env::current_dir().unwrap();
    let filesystem_path = current_dir
        .as_path()
        .join("tests/clang-indexer-code/test1.c");
    let module = Module::new(ModuleId::new(1), "test", ProjectId::new(1));
    let state = index_files(vec![filesystem_path.to_str().unwrap()], &module).await;

    let files = state.get_index().all_files().await.unwrap();
    log::debug!("{:?}", files);
    let file = &files[0];

    assert_eq!(file.id, FileId::new(1));
    assert_eq!(file.module, module.id);
    assert_eq!(file.module_path, "test1.c");
    assert_eq!(Path::new(&file.filesystem_path), filesystem_path);
    assert_eq!(file.filetype, "cxx");

    let files = state.get_index().all_files().await.unwrap();
    for file in files.iter() {
        println!("File: {:?}", file);
    }
    assert_eq!(files.len(), 1);

    let symbols = state.get_index().all_symbols().await.unwrap();
    for symbol in symbols.iter() {
        println!("Symbols: {:?}", symbol);
    }

    let expected_symbols = [
        Symbol::new(1.into(), "foo", module.id, SymbolScope::Global),
        Symbol::new(2.into(), "bar", module.id, SymbolScope::Local),
        Symbol::new(3.into(), "zar", module.id, SymbolScope::Local),
        Symbol::new(4.into(), "main", module.id, SymbolScope::Global),
        Symbol::new(5.into(), "tar", module.id, SymbolScope::Global),
    ];
    for (i, s) in symbols.iter().enumerate() {
        assert_eq!(*s, expected_symbols[i]);
    }
    assert_eq!(symbols.len(), expected_symbols.len());

    let instances = state.get_index().all_symbol_instances().await.unwrap();
    for instance in instances.iter() {
        println!("SymbolInstance: {:?}", instance);
    }

    let did = SymbolInstanceId::invalid();
    let fid = FileId::new(1);
    let expected_instances = [
        SymbolInstance::new_nolines(did, SymbolId::new(1), fid, SymbolType::Function), // foo
        SymbolInstance::new_nolines(did, SymbolId::new(1), fid, SymbolType::Function), // foo
        SymbolInstance::new_nolines(did, SymbolId::new(2), fid, SymbolType::Function), // bar
        SymbolInstance::new_nolines(did, SymbolId::new(3), fid, SymbolType::Function), // zar
        SymbolInstance::new_nolines(did, SymbolId::new(4), fid, SymbolType::Function), // main
        SymbolInstance::new_nolines(did, SymbolId::new(5), fid, SymbolType::Function), // tar
        SymbolInstance::new_nolines(did, SymbolId::new(5), fid, SymbolType::Function), // tar
        SymbolInstance::new_nolines(did, SymbolId::new(5), fid, SymbolType::Function), // tar
        SymbolInstance::new_nolines(did, SymbolId::new(3), fid, SymbolType::Function), // zar
    ];
    for (i, o) in instances.iter().enumerate() {
        assert_eq!(mask_instance(o), expected_instances[i]);
    }
    assert_eq!(instances.len(), expected_instances.len());

    let refs = state.get_index().all_refs().await.unwrap();
    for reference in refs.iter() {
        println!("Reference: {:?}", reference);
    }
    let expected_refs = [
        new_ref(5, 3), // main to zar
        new_ref(5, 1), // main to foo
        new_ref(5, 5), // main to tar
        new_ref(5, 2), // main to bar
    ];
    for (i, s) in refs.iter().enumerate() {
        assert_eq!(mask_ref(&s), expected_refs[i]);
    }
    assert_eq!(refs.len(), expected_refs.len());

    let index: Index = state.into();
    let _symbols = from_index(&index).await.unwrap();
}

pub async fn from_index(index: &Index) -> Result<()> {
    let symbols = index.all_symbols().await?;
    let mut symbols_map = HashMap::new();
    for symbol in symbols {
        symbols_map.insert(
            symbol.id,
            symbols::Symbol::new(SymbolId::from(symbol.id), symbol.name.clone()),
        );
    }

    let instances = index.all_symbol_instances().await?;
    let mut instance_map = HashMap::new();
    for instance in instances {
        instance_map.insert(instance.id, instance);
    }

    let files = index.all_files().await?;
    let mut files_map = HashMap::new();
    for file in files {
        files_map.insert(file.id, file);
    }

    let modules = index.all_modules().await?;
    let mut modules_map = HashMap::new();
    for module in modules {
        modules_map.insert(module.id, module);
    }

    let references = index.all_refs().await?;
    for reference in references {
        let from_instance = instance_map.get(&reference.from_symbol_instance).unwrap();
        let from_symbol = symbols_map.get_mut(&from_instance.symbol).unwrap();
        let occurrence = Occurrence {
            file: from_instance.file_id,
            offset_range: (
                reference.from_offset_start as i32,
                reference.from_offset_end as i32,
            ),
        };
        from_symbol.add_child(reference.to_symbol, occurrence.clone());

        let to_symbol = symbols_map.get_mut(&reference.to_symbol).unwrap();
        to_symbol.add_parent(from_instance.id, occurrence);
    }

    Ok(())
}
