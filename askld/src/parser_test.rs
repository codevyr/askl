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
    const QUERY: &str = r#"ignore(package="k8s.io/klog");; "a""#;
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
    let ast = parse("func(\n\"name\"\n)").unwrap();
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

// === preamble scope syntax parsing tests ===

#[test]
fn preamble_scope_parses() {
    let ast = parse("preamble {\nignore(package=\"foo\")\n}\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_inline_parses() {
    let ast = parse("preamble ignore(package=\"foo\")\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_scope_multiple_verbs() {
    let ast = parse("preamble {\nignore(package=\"foo\")\nignore(package=\"bar\")\n}").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn preamble_scope_single_line() {
    let ast = parse("preamble { ignore(package=\"foo\") }\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_alone_is_noop() {
    let ast = parse("preamble\n\"bar\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_scope_with_semicolons() {
    let ast = parse("preamble { ignore(package=\"foo\"); ignore(package=\"bar\") }\n\"baz\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

// === Multi-line argument list tests ===

#[test]
fn multiline_positional_args() {
    let ast = parse("func(\n\"a\",\n\"b\"\n)").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn multiline_named_args() {
    let ast = parse("ignore(\npackage=\"foo\"\n)").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn label_shortcut_parses() {
    let ast = parse("@foo").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn inherit_label_shortcut_parses() {
    let ast = parse("@@foo").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn use_shortcut_parses() {
    let ast = parse("#foo").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn label_shortcut_with_scope() {
    let ast = parse(r#"@foo "a" { "b" }"#).unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn bare_verb_with_args() {
    let ast = parse(r#"func("main")"#).unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn bare_verb_no_args() {
    let ast = parse("preamble").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn line_comment() {
    let ast = parse("\"a\" // this is a comment\n\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn line_comment_at_end() {
    let ast = parse("\"a\" // trailing comment").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

// === Underscore as UnitVerb tests ===

#[test]
fn underscore_is_unit_verb() {
    let ast = parse(r#""foo" { _ {} }"#).unwrap();
    let stmts: Vec<_> = ast.scope().statements().collect();
    assert_eq!(stmts.len(), 1);
    assert_eq!(stmts[0].scope().statements().count(), 1);
}

#[test]
fn underscore_alone() {
    let ast = parse("_").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn underscore_with_verbs() {
    let ast = parse(r#"_ "bar""#).unwrap();
    assert_eq!(ast.scope().statements().count(), 1);
}

#[test]
fn underscore_equivalent_to_bare_scope() {
    // Both `_ {}` and `{}` parse as a single top-level statement with an empty scope
    let with_underscore = parse("_ {}").unwrap();
    let bare_scope = parse("{}").unwrap();
    let us: Vec<_> = with_underscore.scope().statements().collect();
    let bs: Vec<_> = bare_scope.scope().statements().collect();
    assert_eq!(us.len(), 1);
    assert_eq!(bs.len(), 1);
}

#[test]
fn extra_semicolons_do_not_create_statements() {
    let ast = parse(r#""foo" { "bar" ; ; ; ; ; }"#).unwrap();
    let stmts: Vec<_> = ast.scope().statements().collect();
    assert_eq!(stmts.len(), 1); // just "foo" + scope
    assert_eq!(stmts[0].scope().statements().count(), 1); // just "bar"
}
