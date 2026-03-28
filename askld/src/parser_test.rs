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

#[test]
fn parse_unit_verb() {
    const QUERY: &str = r#"@ignore(package="k8s.io/klog");; "a""#;
    let ast = parse(QUERY).unwrap();
    println!("{:?}", ast);
}

// === Newline-as-separator tests ===

#[test]
fn newline_separates_statements() {
    let ast = parse("\"a\"\n\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn same_line_is_single_statement() {
    let ast = parse("\"a\" \"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn scope_on_same_line_attaches() {
    let ast = parse("\"a\" {\n\"b\"\n}").unwrap();
    let stmts: Vec<_> = ast.scope().statements().collect();
    assert_eq!(stmts.len(), 1);
    assert_eq!(stmts[0].scope().statements().count(), 1);
}

#[test]
fn newline_before_scope_splits() {
    let ast = parse("\"a\"\n{\"b\"}").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn newlines_in_parens() {
    let ast = parse("@func(\n\"name\"\n)").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn windows_line_endings() {
    let ast = parse("\"a\"\r\n\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn carriage_return_only() {
    let ast = parse("\"a\"\r\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn multiple_empty_lines() {
    let ast = parse("\"a\"\n\n\n\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn semicolons_still_work() {
    let ast = parse("\"a\";\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn mixed_separators() {
    let ast = parse("\"a\";\n\"b\"\n\"c\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 3);
}

#[test]
fn leading_trailing_newlines() {
    let ast = parse("\n\n\"a\"\n\n").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn multiline_comment_does_not_separate() {
    let ast = parse("\"a\" /* comment\nstill comment */ \"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

// === @preamble scope syntax parsing tests ===

#[test]
fn preamble_scope_parses() {
    let ast = parse("@preamble {\n@ignore(package=\"foo\")\n}\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_inline_parses() {
    let ast = parse("@preamble @ignore(package=\"foo\")\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_scope_multiple_verbs() {
    let ast = parse("@preamble {\n@ignore(package=\"foo\")\n@ignore(package=\"bar\")\n}").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn preamble_scope_single_line() {
    let ast = parse("@preamble { @ignore(package=\"foo\") }\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_alone_is_noop() {
    let ast = parse("@preamble\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_scope_with_semicolons() {
    let ast = parse("@preamble { @ignore(package=\"foo\"); @ignore(package=\"bar\") }\n\"baz\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

// === Multi-line argument list tests ===

#[test]
fn multiline_positional_args() {
    let ast = parse("@func(\n\"a\",\n\"b\"\n)").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn multiline_named_args() {
    let ast = parse("@ignore(\npackage=\"foo\"\n)").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}
