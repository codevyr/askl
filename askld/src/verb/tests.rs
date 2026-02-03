use index::{db_diesel::Index, symbols::DeclarationId};
use testcontainers::{clients, core::WaitFor, GenericImage};

use crate::test_support::wait_for_postgres;

use crate::{
    cfg::ControlFlowGraph, execution_context::ExecutionContext, span::Span, test_util::run_query,
    verb::*,
};

use std::collections::HashMap;

#[tokio::test(flavor = "current_thread")]
async fn test_select_matching_name() {
    let docker = clients::Cli::default();
    let image = GenericImage::new("postgres", "15-alpine")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "askl")
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ));
    let node = docker.run(image);
    let port = node.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/askl", port);

    wait_for_postgres(&url).await.unwrap();

    let index_diesel = Index::connect(&url).await.unwrap();
    index_diesel.load_test_input("verb_test.sql").await.unwrap();
    let cfg = ControlFlowGraph::from_symbols(index_diesel);

    let test_cases = vec![
        ("sort.Sort", vec![96]),
        ("sort.IsSorted", vec![95]),
        ("foo", vec![91, 92]),
        ("bar", vec![92]),
        ("foo.bar", vec![92]),
        ("FOO.bar", vec![]),
        ("FOO", vec![]),
    ];

    let mut ctx = ExecutionContext::new(); // Assuming there's a default constructor

    for (name, expected_ids) in test_cases {
        let fake_span = Span::synthetic(name);
        let named_args = HashMap::from([("name".to_string(), name.to_string())]);
        let selector = NameSelector::new(fake_span, &vec![], &named_args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select_from_all(&mut ctx, &cfg, vec![])
            .await
            .unwrap();

        let mut got_declarations: Vec<DeclarationId> = result
            .unwrap()
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

#[test]
fn test_ignore_package_filter() {
    let query = r#"
@preamble
@ignore(package="foo");
"foo";
"foo.bar";
"foobar";
"tar";
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            DeclarationId::new(91),
            DeclarationId::new(93),
            DeclarationId::new(94),
        ]
    );
}
