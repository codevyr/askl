use crate::parser::parse;

#[test]
fn parse_query() {
    const QUERY: &str = r#""a""#;
    let ast = parse(QUERY).unwrap();

    let statements: Vec<_> = ast.scope().statements().collect();
    assert_eq!(statements.len(), 1);
    let statement = &statements[0];

    let _verb = statement.command();
    let scope = statement.scope();

    let statements: Vec<_> = scope.statements().collect();
    assert_eq!(statements.len(), 0);

    println!("{:?}", ast);
    // assert_eq!(
    //     format!("{:?}", ast),
    //     r#"GlobalStatement { command: Command { verbs: [UnitVerb] }, scope: DefaultScope(RefCell { value: [DefaultStatement { command: Command { verbs: [UnitVerb, ChildrenVerb, NameSelector { name: "a" }] }, scope: EmptyScope }] }) }"#
    // );
}

#[test]
fn parse_parent_query() {
    const QUERY: &str = r#"{"a"}"#;
    let ast = parse(QUERY).unwrap();
    println!("{:?}", ast);
    // assert_eq!(
    //     format!("{:?}", ast),
    //     r#"GlobalStatement { command: Command { verbs: [UnitVerb] }, scope: DefaultScope(RefCell { value: [DefaultStatement { command: Command { verbs: [UnitVerb, ChildrenVerb] }, scope: DefaultScope(RefCell { value: [DefaultStatement { command: Command { verbs: [UnitVerb, ChildrenVerb, NameSelector { name: "a" }] }, scope: EmptyScope }] }) }] }) }"#
    // );
}

#[test]
fn parse_child_query() {
    const QUERY: &str = r#""a"{}"#;
    let ast = parse(QUERY).unwrap();
    println!("{:?}", ast);
    // assert_eq!(
    //     format!("{:?}", ast),
    //     r#"GlobalStatement { command: Command { verbs: [UnitVerb] }, scope: DefaultScope(RefCell { value: [DefaultStatement { command: Command { verbs: [UnitVerb, ChildrenVerb, NameSelector { name: "a" }] }, scope: DefaultScope(RefCell { value: [DefaultStatement { command: Command { verbs: [UnitVerb, ChildrenVerb] }, scope: EmptyScope }] }) }] }) }"#
    // );
}
