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
    let ast =
        parse("preamble { ignore(package=\"foo\"); ignore(package=\"bar\") }\n\"baz\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 2);
}

#[test]
fn preamble_rejects_a_selector() {
    // Issue #19: the name used to be swallowed — redirected into the global
    // context, where it selects nothing — and the query ran as though it had
    // never been written.
    let err = parse("preamble project(\"p\") \"main\"\n\"bar\"").unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("preamble cannot select"),
        "expected a preamble-selector error, got: {msg}"
    );
}

#[test]
fn preamble_scope_rejects_a_selector() {
    // Same rule inside the scope form: `derive` carries the redirection down.
    let err = parse("preamble {\nignore(package=\"foo\")\nsearch(\"xyz\")\n}").unwrap_err();
    let msg = format!("{}", err);
    assert!(
        msg.contains("preamble cannot select"),
        "expected a preamble-selector error, got: {msg}"
    );
}

#[test]
fn preamble_keeps_constraints_and_directives() {
    // What a preamble is FOR must keep parsing: filters — including the NAME
    // filters, which scope every statement instead of selecting on their own
    // — a bare type selector, and the modifier verbs, which are selectors
    // that never originate a row.
    for query in [
        "preamble project(\"p\") ignore(\"builtin\")\n\"bar\"",
        "preamble filter(\"type\", \"func\")\n\"bar\"",
        "preamble filter(\"compound_name\", \"test\", inherit=true)\n\"bar\"",
        "preamble filter(\"exact_name\", \"foo\")\n\"bar\"",
        "preamble func\n\"bar\"",
    ] {
        assert!(parse(query).is_ok(), "must still parse: {query}");
    }
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

/// Anchor classification: parse a one-statement query and report whether its
/// command is anchored (eligible to drive execution).
#[cfg(test)]
fn first_statement_anchored(query: &str) -> bool {
    let ast = parse(query).unwrap();
    let statements: Vec<_> = ast.scope().statements().collect();
    assert!(!statements.is_empty(), "query must parse to a statement");
    statements[0].command().is_anchored()
}

#[test]
fn anchor_classification_table() {
    // Anchored: name predicates in any form.
    assert!(first_statement_anchored(r#""foo""#));
    assert!(first_statement_anchored(r#"g"foo*""#));
    assert!(first_statement_anchored(r#"!"foo""#));
    assert!(first_statement_anchored(r#"func("foo")"#));
    assert!(first_statement_anchored(
        r#"select filter("exact_name", "foo")"#
    ));
    assert!(first_statement_anchored(
        r#"filter("compound_name", "a::b")"#
    ));
    // Anchored: `select`, the bindness verb, carries the All anchor.
    assert!(first_statement_anchored("select"));
    assert!(first_statement_anchored("func select"));
    // Anchored: content and location predicates.
    assert!(first_statement_anchored(r#"search("needle")"#));
    assert!(first_statement_anchored(r#"loc("main.c", 1)"#));
    // Pure constraints: type predicates, project, bare select.
    assert!(!first_statement_anchored("func"));
    assert!(!first_statement_anchored(r#"project("linux")"#));
    assert!(!first_statement_anchored(r#"func project("linux")"#));
    assert!(!first_statement_anchored(r#"filter("type", "func")"#));
    // An anchor anywhere in the verb bag anchors the whole statement.
    assert!(first_statement_anchored(r#"func "foo""#));
    assert!(first_statement_anchored(r#"project("linux") "foo""#));
}

// === Primitive literals: integers and booleans ===

#[test]
fn bare_literals_are_not_selectors() {
    // Literals live in argument position only.  A bare `42` or `true` in
    // statement position is a mistake, not a name — `"42"` is how you name a
    // symbol called 42 — so `plain_filter` takes a string, never a `value`.
    for query in ["42", "-7", "true", "false", "!42", "!true"] {
        assert!(
            parse(query).is_err(),
            "a bare literal must not parse as a selector: {query}"
        );
    }
}

#[test]
fn boolean_keywords_do_not_split_longer_words() {
    // `true`/`false` are keywords only where the word ends, mirroring the
    // !XID_CONTINUE guard on or/and/not.  `truthy` is not `true` followed by
    // garbage; it is not a value at all.
    for query in [
        r#"func("a", inherit=truthy)"#,
        r#"func("a", inherit=falsey)"#,
    ] {
        assert!(parse(query).is_err(), "must not parse: {query}");
    }
}

#[test]
fn integer_literals_must_fit_in_64_bits() {
    // Both signs go through the same `integer` rule, so both overflow the
    // same way — and the message says which literal, not "expected ...".
    for query in [
        r#"search("abc", limit=99999999999999999999)"#,
        r#"search("abc", limit=-99999999999999999999)"#,
    ] {
        let err = parse(query).unwrap_err();
        let msg = format!("{err}");
        assert!(
            msg.contains("does not fit in a 64-bit integer"),
            "expected an out-of-range error for {query}, got: {msg}"
        );
    }
}

#[test]
fn a_bare_word_in_value_position_names_every_value_type() {
    // pest lists the rules that could have matched, and a SILENT rule never
    // appears there.  With `quoted_string` silent, `case=smart` was told it
    // could write a glob string, an integer or a boolean — everything except
    // the plain string it meant.
    let err = parse(r#"search("x", case=smart)"#).unwrap_err();
    let msg = format!("{err}");
    for want in ["a quoted string", "an integer", "a boolean"] {
        assert!(msg.contains(want), "expected {want:?} in: {msg}");
    }
}
