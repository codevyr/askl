use std::env;

use askl::{index::Index, indexer::clang::{VisitorState, CompileCommand, run_clang_ast}};

#[tokio::test]
async fn create_state() {
    let index = Index::new_or_connect("sqlite::memory:").await.unwrap();
    let mut state = VisitorState::new(index);

    let clang = "clang";
    let mut test_directory = env::current_dir().unwrap();
    test_directory.push("tests/clang-indexer-code");
    let test_file = "test1.c".to_string();
    let command = CompileCommand{
        arguments: Some(vec![clang.to_string(), test_file.clone()]),
        command: None,
        directory: test_directory.to_str().unwrap().to_string(),
        file: test_file,
        output: Some("/dev/null".to_string())
    };

    println!("{:?}", command);

    let (ast_file, node) = run_clang_ast(&clang, command).await.unwrap();

    println!("{}", ast_file);
    
    state.extract_symbol_map_root(node).await.unwrap();
    state.handle_unresolved_symbols().await.unwrap();
}