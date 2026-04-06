use crate::cfg::ControlFlowGraph;
use crate::execution_context::ExecutionContext;
use crate::parser::parse;
use crate::test_util::{create_index_with_timeout, get_shared_db_url, TEST_INPUT_A};
use tokio::runtime::Runtime;
use tokio::task;

#[test]
fn timeout_error_propagates_with_statement_location() {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async {
        let url = get_shared_db_url(TEST_INPUT_A);
        let index = create_index_with_timeout(url, 1).await;
        let cfg = ControlFlowGraph::from_symbols(index);

        let query = r#"func("main")"#;
        let ast = parse(query).unwrap();
        let mut ctx = ExecutionContext::new();
        let result = ast.execute(&mut ctx, &cfg).await;

        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("Expected error from timed-out query"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("statement timeout"),
            "Error should contain 'statement timeout', got: {}",
            msg,
        );
        // The error should point to the statement's span, not (0,0)
        match err.line_col {
            pest::error::LineColLocation::Span((l, c), _) => {
                assert_eq!(l, 1, "Expected line 1");
                assert_eq!(c, 1, "Expected column 1");
            }
            pest::error::LineColLocation::Pos((l, c)) => {
                assert_eq!(l, 1, "Expected line 1");
                assert_eq!(c, 1, "Expected column 1");
            }
        }
    });
}

#[test]
fn multistatement_timeout_identifies_statement() {
    let mut rt = Runtime::new().unwrap();
    let local = task::LocalSet::new();
    local.block_on(&mut rt, async {
        let url = get_shared_db_url(TEST_INPUT_A);
        let index = create_index_with_timeout(url, 1).await;
        let cfg = ControlFlowGraph::from_symbols(index);

        let query = "func(\"main\")\nfunc(\"other\")";
        let ast = parse(query).unwrap();
        let mut ctx = ExecutionContext::new();
        let result = ast.execute(&mut ctx, &cfg).await;

        let err = match result {
            Err(e) => e,
            Ok(_) => panic!("Expected error from timed-out query"),
        };
        let msg = err.to_string();
        assert!(
            msg.contains("statement timeout"),
            "Error should contain 'statement timeout', got: {}",
            msg,
        );
        // Should point to the first statement (line 1)
        match err.line_col {
            pest::error::LineColLocation::Span((l, _), _) => {
                assert_eq!(l, 1, "Expected timeout error on line 1 (first statement)");
            }
            pest::error::LineColLocation::Pos((l, _)) => {
                assert_eq!(l, 1, "Expected timeout error on line 1 (first statement)");
            }
        }
    });
}
