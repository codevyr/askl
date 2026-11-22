//! Boolean filter expressions: parsing, admission errors, and execution.

use crate::hierarchy::Hierarchy;
use crate::parser::parse;
use crate::test_util::{run_query, TEST_INPUT_A, VERB_TEST};
use index::symbols::SymbolInstanceId;

fn parse_err(query: &str) -> String {
    match parse(query) {
        Ok(_) => panic!("expected `{}` to be rejected", query),
        Err(e) => format!("{}", e),
    }
}

// ---------------------------------------------------------------------------
// Parsing and admission
// ---------------------------------------------------------------------------

#[test]
fn compound_expressions_parse() {
    for q in [
        r#""a" or "b""#,
        r#"func or method"#,
        r#"not func "a""#,
        r#"(func or method) "a""#,
        r#"func and not g"test_*""#,
        r#"project("x") or project("y") { "foo" }"#,
    ] {
        parse(q).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", q, e));
    }
}

#[test]
fn keywords_do_not_split_identifiers() {
    // `order`, `android`, `notation` must stay ordinary (unknown) verb
    // idents rather than parse as `or`/`and`/`not` + fragment.
    for q in ["order", "android", "notation"] {
        let err = parse_err(q);
        assert!(
            err.contains("unknown verb"),
            "`{}` should be an unknown verb, got: {}",
            q,
            err
        );
    }
}

#[test]
fn non_predicate_verbs_are_rejected_in_expressions() {
    // (A relationship WORD is an atom of the other kind of expression: see
    // "Relationship expressions" below.  `unnest` is not one.)
    let err = parse_err(r#"not unnest "a""#);
    assert!(err.contains("relationship verb"), "{}", err);

    let err = parse_err(r#"@l or "a""#);
    assert!(err.contains("label verb"), "{}", err);
}

#[test]
fn non_concrete_atoms_are_rejected_in_expressions() {
    let err = parse_err(r#"search("xyz") or "a""#);
    assert!(err.contains("not a single predicate query"), "{}", err);

    let err = parse_err(r#"any or "a""#);
    assert!(err.contains("constrains nothing"), "{}", err);

    // An `ignore` whose package names no package excludes nothing, so it is
    // not a concrete predicate: admitting it used to panic the query in
    // `PredicateExpr::compile`'s totality `unreachable!()`.
    let err = parse_err(r#"not ignore(package="") "a""#);
    assert!(err.contains("excludes nothing"), "{}", err);

    let err = parse_err(r#"select or "a""#);
    assert!(err.contains("constrains nothing"), "{}", err);

    let err = parse_err(r#"!"a" or "b""#);
    assert!(err.contains("forced"), "{}", err);
}

#[test]
fn double_negation_is_rejected() {
    // Both spellings: the positivity classification would silently treat a
    // double-negated (i.e. positive) predicate as an exclusion.
    for q in [r#"not not "a" "b""#, r#"not (not "a") "b""#] {
        let err = parse_err(q);
        assert!(err.contains("double negation"), "`{}`: {}", q, err);
    }
}

#[test]
fn mixed_anchor_filter_or_is_rejected() {
    let err = parse_err(r#""a" or func"#);
    assert!(err.contains("mixes anchors"), "{}", err);
}

#[test]
fn cross_dimension_filter_expressions_are_rejected() {
    let err = parse_err(r#"project("x") or func"#);
    assert!(err.contains("cross-dimension"), "{}", err);

    let err = parse_err(r#"project("x") and func "a""#);
    assert!(err.contains("cross-dimension"), "{}", err);
}

// ---------------------------------------------------------------------------
// The slot cascade over compounds
// ---------------------------------------------------------------------------

#[test]
fn filter_compound_inherits_as_a_unit() {
    let ast = parse(r#"project("x") or project("y") { "foo" }"#).unwrap();
    let outer_all: Vec<_> = ast.scope().statements().collect();
    let outer = &outer_all[0];
    assert_eq!(outer.command().filters().count(), 1);

    let inner_all: Vec<_> = outer.scope().statements().collect();
    let child = &inner_all[0];
    let filters: Vec<String> = child
        .command()
        .filters()
        .map(|f| format!("{}", f))
        .collect();
    assert_eq!(
        filters.len(),
        1,
        "the or-group must flow down as one slot value: {:?}",
        filters
    );
    assert!(filters[0].contains("project"), "{:?}", filters);
}

#[test]
fn child_type_write_replaces_inherited_compound() {
    let ast = parse(r#"func or method { data "x" }"#).unwrap();
    let outer_all: Vec<_> = ast.scope().statements().collect();
    let inner_all: Vec<_> = outer_all[0].scope().statements().collect();
    let filters: Vec<String> = inner_all[0]
        .command()
        .filters()
        .map(|f| format!("{}", f))
        .collect();
    assert_eq!(
        filters,
        vec!["TypeSelector(data)".to_string()],
        "a child type write must evict the whole inherited type-slot group"
    );
}

// ---------------------------------------------------------------------------
// Execution
// ---------------------------------------------------------------------------

fn union(mut a: Vec<SymbolInstanceId>, b: Vec<SymbolInstanceId>) -> Vec<SymbolInstanceId> {
    a.extend(b);
    a.sort();
    a.dedup();
    a
}

fn sorted(mut v: Vec<SymbolInstanceId>) -> Vec<SymbolInstanceId> {
    v.sort();
    v
}

#[test]
fn or_of_anchors_selects_the_union() {
    let a = run_query(TEST_INPUT_A, r#""a""#).nodes.as_vec();
    let b = run_query(TEST_INPUT_A, r#""b""#).nodes.as_vec();
    let both = run_query(TEST_INPUT_A, r#""a" or "b""#).nodes.as_vec();
    assert_eq!(sorted(both), union(a, b));
}

#[test]
fn grouping_parens_are_transparent() {
    let flat = run_query(TEST_INPUT_A, r#""a" or "b""#).nodes.as_vec();
    let grouped = run_query(TEST_INPUT_A, r#"("a" or "b")"#).nodes.as_vec();
    assert_eq!(sorted(grouped), sorted(flat));
}

#[test]
fn and_binds_tighter_than_or() {
    // "a" or ("b" and "c"): the right conjunct is an always-empty name
    // conjunction, so the result is exactly "a".  Under the wrong grouping
    // (("a" or "b") and "c") the result would be empty.
    let a = run_query(TEST_INPUT_A, r#""a""#).nodes.as_vec();
    let res = run_query(TEST_INPUT_A, r#""a" or "b" and "c""#)
        .nodes
        .as_vec();
    assert_eq!(sorted(res), sorted(a));
}

#[test]
fn not_excludes_within_a_branch() {
    let b = run_query(TEST_INPUT_A, r#""b""#).nodes.as_vec();
    let res = run_query(TEST_INPUT_A, r#"("a" or "b") and not "a""#)
        .nodes
        .as_vec();
    assert_eq!(sorted(res), sorted(b));
}

// ---------------------------------------------------------------------------
// Regressions (review findings)
// ---------------------------------------------------------------------------

/// A standalone `ignore` with an unusable package stays the no-op it has
/// always been — only expressions, whose compiler needs totality, refuse it.
#[test]
fn ignore_with_no_usable_package_still_parses_standalone() {
    parse(r#"ignore(package="") "a""#).unwrap();
}

/// Exclusions inherit unconditionally, so mixing one into a filter group
/// must not stop the GROUP from inheriting: the child scope keeps both the
/// type constraint and the exclusion.  The group inherits when its type
/// word does, which a type word does only on request.
#[test]
fn filter_group_with_an_exclusion_still_inherits() {
    let ast = parse(r#"func and not g"test_*" inherit(type) { "x" }"#).unwrap();
    let outer: Vec<_> = ast.scope().statements().collect();
    let inner: Vec<_> = outer[0].scope().statements().collect();
    let child: Vec<String> = inner[0]
        .command()
        .filters()
        .map(|f| format!("{}", f))
        .collect();
    assert_eq!(
        child.len(),
        1,
        "the group must inherit as a unit, got {:?}",
        child
    );
    assert!(child[0].contains("func"), "{:?}", child);
    assert!(child[0].contains("not"), "{:?}", child);
}

/// Slots are held by constraints: a later type filter conjoins with an
/// anchor group instead of evicting it (which silently dropped the only
/// anchor and left the statement enumerating everything).
#[test]
fn a_later_filter_does_not_evict_an_anchor_group() {
    let ast = parse(r#"func("a") or method("a") data"#).unwrap();
    let stmts: Vec<_> = ast.scope().statements().collect();
    assert!(
        stmts[0].command().is_anchored(),
        "the anchor group must survive a later type write"
    );
    let filters: Vec<String> = stmts[0]
        .command()
        .filters()
        .map(|f| format!("{}", f))
        .collect();
    assert_eq!(filters, vec!["TypeSelector(data)".to_string()]);
}

/// A trailing operator continues the statement onto the next line; a line
/// STARTING with an operator is still a statement break, and says so.
#[test]
fn operators_continue_across_lines_only_when_trailing() {
    let ast = parse("\"a\" or\n\"b\"").unwrap();
    assert_eq!(ast.scope().statements().count(), 1);

    let err = parse_err("\"a\"\nor \"b\"");
    assert!(
        err.contains("trailing operator"),
        "expected the line-continuation hint, got: {}",
        err
    );
    // The hint stands on its own: a line-initial operator is not a verb that
    // failed to build, so it must not wear that frame.
    assert!(
        !err.contains("Failed to create a generic verb"),
        "the hint must not be wrapped in a construction failure: {}",
        err
    );
}

/// An or-group of plain names keeps the "did you mean?" token that the
/// equivalent juxtaposition offers.
#[test]
fn anchor_group_forwards_a_suggestion_token() {
    let ast = parse(r#""vfs_rea" or "vfs_write""#).unwrap();
    let stmts: Vec<_> = ast.scope().statements().collect();
    let token = stmts[0]
        .command()
        .selectors()
        .find_map(|s| s.matched_token());
    assert_eq!(token, Some("vfs_rea".to_string()));
}

/// Layer blocks build their ops during construction, so expressions are
/// refused there outright rather than leaving half-built ops behind.
#[test]
fn expressions_are_rejected_inside_layer_blocks() {
    let err = parse_err(r#"layer { ephemeral_symbol(name="a") or ephemeral_symbol(name="b") }"#);
    assert!(err.contains("not allowed inside layer blocks"), "{}", err);
}

// ---------------------------------------------------------------------------
// `not` versus `ignore`, and the positive package filter
// ---------------------------------------------------------------------------

/// `not "x"` negates the SAME predicate the positive selector `"x"` uses —
/// a leaf-name match.  `ignore("x")` is broader: it excludes anything whose
/// compound path carries the label `x` at any position.  On plain leaf names
/// the two coincide, which is why they read as aliases.
#[test]
fn not_name_and_ignore_agree_on_leaf_names() {
    for (with_ignore, with_not) in [
        (r#""a" {ignore("b")}"#, r#""a" {not "b"}"#),
        (r#""d" {ignore("e")}"#, r#""d" {not "e"}"#),
    ] {
        let old = run_query(TEST_INPUT_A, with_ignore);
        let new = run_query(TEST_INPUT_A, with_not);
        assert_eq!(
            sorted(old.nodes.as_vec()),
            sorted(new.nodes.as_vec()),
            "`{}` and `{}` must select the same nodes",
            with_ignore,
            with_not
        );
    }
}

/// ...but they are NOT interchangeable in general, and the docs must not
/// call them aliases: `foo.bar` carries the label `foo` inside its path, so
/// `ignore("foo")` drops it while `not "foo"` (leaf match) keeps it.
#[test]
fn not_name_is_narrower_than_ignore_on_compound_names() {
    let ignored = run_query(VERB_TEST, r#""foo.bar" ignore("foo")"#);
    assert_eq!(
        ignored.nodes.as_vec(),
        vec![],
        "ignore() excludes on any path label"
    );

    let negated = run_query(VERB_TEST, r#""foo.bar" not "foo""#);
    assert_eq!(
        negated.nodes.as_vec(),
        vec![SymbolInstanceId::new(92)],
        "not \"foo\" excludes only symbols whose LEAF is foo"
    );
}

#[test]
fn package_filter_parses_and_holds_its_slot() {
    parse(r#"package("k8s.io/klog") "a""#).unwrap();
    parse(r#"not (g"kl*" and package("k8s.io/klog")) "a""#).unwrap();

    // package() writes its own dimension: a later write replaces it.
    let ast = parse(r#"package("k8s.io/x") package("k8s.io/y") "a""#).unwrap();
    let stmts: Vec<_> = ast.scope().statements().collect();
    let filters: Vec<String> = stmts[0]
        .command()
        .filters()
        .map(|f| format!("{}", f))
        .collect();
    assert_eq!(filters, vec!["PackageFilter(package=k8s.io/y)".to_string()]);
}

// ---------------------------------------------------------------------------
// Relationship expressions
// ---------------------------------------------------------------------------

/// The relationship each top-level statement asks of the statements in its
/// braces, as (edge, the words written).
fn asked(query: &str) -> Vec<(index::edge::Edge, Option<String>)> {
    let ast = parse(query).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", query, e));
    ast.scope()
        .statements()
        .map(|statement| {
            let child = statement
                .children()
                .next()
                .unwrap_or_else(|| panic!("`{}` has no scope", query));
            (
                child.get_relationship_type(),
                child.relationship().written(),
            )
        })
        .collect()
}

#[test]
fn relationship_words_join_with_or() {
    use index::edge::{Edge, RefTest};
    use index::roles::{Relation, Roles};

    let calls = Edge::references(RefTest::stating(Roles::CALL).unwrap());
    let writes = Edge::references(RefTest::stating(Roles::WRITE).unwrap());
    let takes = Edge::references(RefTest::stating(Roles::TAKE).unwrap());
    let holds = Edge::references(RefTest::relation(Relation::Holds));
    let declares = Edge::references(RefTest::relation(Relation::Declares));
    let refs = Edge::references(RefTest::ORDINARY);

    for (q, edge, written) in [
        // One word is the expression of one word.
        (r#"calls { "x" }"#, calls, "calls"),
        (r#"(calls) { "x" }"#, calls, "calls"),
        // A PAIR MATCHES IF ANY OF ITS ROWS DOES: the union of the tests.
        (r#"calls or has { "x" }"#, calls | Edge::HAS, "calls or has"),
        (
            r#""vfs_read" calls or holds { func }"#,
            calls | holds,
            "calls or holds",
        ),
        (
            r#"calls or writes or takes { "x" }"#,
            calls | writes | takes,
            "calls or writes or takes",
        ),
        (
            r#"refs or declares { "x" }"#,
            refs | declares,
            "refs or declares",
        ),
        // Parentheses group and change nothing: there is one operator.
        (
            r#"(calls or writes) or holds { "x" }"#,
            calls | writes | holds,
            "calls or writes or holds",
        ),
        (
            r#"has or (refs or (declares)) { "x" }"#,
            Edge::HAS | refs | declares,
            "has or refs or declares",
        ),
        // A trailing operator continues the expression on the next line.
        (
            "\"a\" calls or\n  holds { \"x\" }",
            calls | holds,
            "calls or holds",
        ),
        // `reads` is `takes`; each is quoted as it was written, once.
        (r#"reads { "x" }"#, takes, "reads"),
        (r#"takes or reads { "x" }"#, takes, "takes or reads"),
        (r#"calls or calls { "x" }"#, calls, "calls"),
        // A role word narrows `refs`, so `refs` beside it admits them all.
        (r#"calls or refs { "x" }"#, refs, "calls or refs"),
    ] {
        assert_eq!(asked(q), vec![(edge, Some(written.to_string()))], "{q}");
    }

    // The four relationships are everything: the bare scope's edge.
    let all = asked(r#""a" has or refs or declares or holds { "x" }"#);
    assert_eq!(all[0].0, Edge::ALL);
    assert_eq!(asked(r#""a" { "x" }"#), vec![(Edge::ALL, None)]);

    // LAW 3: `func or data calls or holds { "x" }` is two expressions, a type
    // group for the statement and a relationship group for the edge.
    let ast = parse(r#"func or data calls or holds { "x" }"#).unwrap();
    let statement = ast.scope().statements().next().unwrap();
    assert!(statement.command().has_suppress_default_type_filter());
    let child = statement.children().next().unwrap();
    assert_eq!(child.get_relationship_type(), calls | holds);
    assert_eq!(
        child.relationship().written().as_deref(),
        Some("calls or holds")
    );
}

#[test]
fn and_between_relationships_is_not_supported_yet() {
    for (q, quoted) in [
        (r#"calls and writes { "n" }"#, "calls and writes"),
        (r#""a" has and refs { func }"#, "has and refs"),
        // `and` binds tighter than `or`: the conjunction is what is quoted.
        (r#"has or calls and writes { "n" }"#, "calls and writes"),
        (
            r#"(calls or takes) and writes { "n" }"#,
            "(calls or takes) and writes",
        ),
        // With a `not` inside, the `and` is met first.
        (r#"refs and not calls { "x" }"#, "refs and not calls"),
    ] {
        let err = parse_err(q);
        let expected = format!(
            "`and` between relationships is not supported yet: `{quoted}` would have to be \
             decided for the pair as a whole. Ask each in its own query."
        );
        assert!(err.contains(&expected), "{q}: {err}");
    }
}

#[test]
fn not_on_a_relationship_is_not_supported_yet() {
    for (q, quoted, instead) in [
        (
            r#"not calls { "x" }"#,
            "not calls",
            "`writes or takes`, `refs`, `declares`",
        ),
        (
            r#"not writes { "x" }"#,
            "not writes",
            "`calls or takes`, `refs`, `declares`",
        ),
        (
            r#"not reads { "x" }"#,
            "not reads",
            "`calls or writes`, `refs`, `declares`",
        ),
        (
            r#"not (calls or writes) { "x" }"#,
            "not (calls or writes)",
            "`takes`, `refs`, `declares`",
        ),
        (
            r#"not has { "x" }"#,
            "not has",
            "`refs`, `declares`, `holds`",
        ),
        (
            r#"not refs { "x" }"#,
            "not refs",
            "`has`, `declares`, `holds`",
        ),
        (
            r#"has or not holds { "x" }"#,
            "not holds",
            "`has`, `refs`, `declares`",
        ),
        // Written before a selector, with no braces: still one expression.
        (r#"not has "a""#, "not has", "`refs`, `declares`, `holds`"),
    ] {
        let err = parse_err(q);
        let expected = format!("`{quoted}` is not supported yet: name what you want: {instead}.");
        assert!(err.contains(&expected), "{q}: {err}");
    }
}

#[test]
fn a_relationship_and_a_predicate_are_written_side_by_side() {
    // One expression is over predicates or over relationship words.
    for (q, expected) in [
        (
            r#"func or calls { "x" }"#,
            "`func` is a type, `calls` a relationship: write them side by side: \
             `func calls { … }`",
        ),
        (
            r#"calls or func { "x" }"#,
            "`func` is a type, `calls` a relationship: write them side by side: \
             `func calls { … }`",
        ),
        (
            r#"func and has { "x" }"#,
            "`func` is a type, `has` a relationship: write them side by side: \
             `func has { … }`",
        ),
        (
            r#"func or data or calls or holds { "x" }"#,
            "`func` is a type, `calls` a relationship: write them side by side: \
             `func or data calls or holds { … }`",
        ),
        (
            r#""foo" or calls { "x" }"#,
            "`\"foo\"` is a selector, `calls` a relationship: write them side by side: \
             `\"foo\" calls { … }`",
        ),
        (
            r#"project("p") or holds { "x" }"#,
            "`project(\"p\")` is a filter, `holds` a relationship: write them side by side: \
             `project(\"p\") holds { … }`",
        ),
    ] {
        let err = parse_err(q);
        assert!(err.contains(expected), "{q}: {err}");
    }

    // What is not a predicate either says so itself.
    let err = parse_err(r#"unnest or calls { "x" }"#);
    assert!(err.contains("`unnest` is a relationship verb"), "{err}");
    let err = parse_err(r#"@l or calls { "x" }"#);
    assert!(err.contains("label verb"), "{err}");
    // A relationship word takes no arguments, in an expression as outside.
    let err = parse_err(r#"calls("x") or has { "x" }"#);
    assert!(err.contains("takes no positional arguments"), "{err}");
}

#[test]
fn a_relationship_word_stands_anywhere_in_its_statement() {
    // A term acts on the statement it is written in; where it stands among
    // the other terms means nothing.  (What each order ANSWERS is the same:
    // `a_relationship_word_acts_on_its_statement_wherever_it_stands`.)
    for q in [
        r#"func calls { "x" }"#,
        r#"calls func { "x" }"#,
        r#""a" has "b" { "x" }"#,
        r#"has "a" "b" { "x" }"#,
        r#"calls or holds func or data { "x" }"#,
        r#"project("p") calls or holds func or data { "x" }"#,
        r#"has @l "a" { "x" }"#,
        r#"@l "a" has { "x" }"#,
    ] {
        parse(q).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", q, e));
    }
}

/// What each statement of the first top-level tree says of the edge into
/// its braces, outermost first: the words, or `-` for a bare scope.  A
/// statement's braces are read off its first child.
fn edges_down(q: &str) -> Vec<String> {
    let ast = parse(q).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", q, e));
    let mut edges = Vec::new();
    let mut statement = ast.scope().statements().next();
    while let Some(s) = statement {
        let child = s.scope().statements().next();
        if let Some(child) = &child {
            edges.push(
                child
                    .relationship()
                    .written()
                    .unwrap_or_else(|| "-".to_string()),
            );
        }
        statement = child;
    }
    edges
}

/// The filters each statement of the first top-level tree runs under,
/// outermost first.
fn filters_down(q: &str) -> Vec<Vec<String>> {
    let ast = parse(q).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", q, e));
    let mut all = Vec::new();
    let mut statement = ast.scope().statements().next();
    while let Some(s) = statement {
        all.push(s.command().filters().map(|f| format!("{}", f)).collect());
        statement = s.scope().statements().next();
    }
    all
}

#[test]
fn a_relationship_word_governs_the_statement_it_is_written_in() {
    // A WORD GOVERNS THE STATEMENT IT IS WRITTEN IN.  The scopes below wrote
    // none, so each is the bare scope.
    for word in [
        "has", "refs", "declares", "holds", "calls", "writes", "takes", "reads",
    ] {
        assert_eq!(
            edges_down(&format!(r#""a" {word} {{ "b" {{ "c" {{ "d" }} }} }}"#)),
            vec![word, "-", "-"],
            "{word}"
        );
    }
    assert_eq!(
        edges_down(r#""a" calls or holds { "b" { "c" } }"#),
        vec!["calls or holds", "-"]
    );
}

#[test]
fn inherit_carries_what_its_statement_writes() {
    // `inherit(relationship)` CARRIES THE STATEMENT'S RELATIONSHIP INTO
    // EVERY SCOPE BELOW, wherever it stands among the terms...
    for q in [
        r#""a" calls inherit(relationship) { "b" { "c" { "d" } } }"#,
        r#""a" inherit(relationship) calls { "b" { "c" { "d" } } }"#,
        r#"inherit(relationship) "a" calls { "b" { "c" { "d" } } }"#,
        r#"calls inherit( relationship ) "a" { "b" { "c" { "d" } } }"#,
    ] {
        assert_eq!(edges_down(q), vec!["calls", "calls", "calls"], "{q}");
    }
    assert_eq!(
        edges_down(r#""a" inherit(relationship) calls or holds { "b" { "c" } }"#),
        vec!["calls or holds", "calls or holds"]
    );
    // ...UNTIL A STATEMENT WRITES ITS OWN, which governs that statement
    // alone unless it says `inherit` too.
    assert_eq!(
        edges_down(r#""a" calls inherit(relationship) { "b" { "c" holds { "d" { "e" } } } }"#),
        vec!["calls", "calls", "holds", "-"]
    );
    assert_eq!(
        edges_down(
            r#""a" calls inherit(relationship) { "b" holds inherit(relationship) { "c" { "d" } } }"#
        ),
        vec!["calls", "holds", "holds"]
    );
    // Written again under a carried relationship, with no word of its own,
    // it carries what was carried.
    assert_eq!(
        edges_down(
            r#""a" has inherit(relationship) { "b" inherit(relationship) { "c" { "d" } } }"#
        ),
        vec!["has", "has", "has"]
    );
    // Nothing to carry is nothing carried, and no error: the scopes below
    // are bare either way.
    assert_eq!(
        edges_down(r#""a" inherit(relationship) { "b" { "c" } }"#),
        vec!["-", "-"]
    );
    assert_eq!(
        edges_down(r#""a" has { "b" inherit(relationship) { "c" { "d" } } }"#),
        vec!["has", "-", "-"]
    );
    // It copies words into the braces that exist; it adds none.
    assert_eq!(
        edges_down(r#""a" calls inherit(relationship)"#),
        Vec::<String>::new()
    );
}

#[test]
fn inherit_type_carries_the_statements_type_filter() {
    // `inherit(type)` CARRIES THE STATEMENT'S TYPE FILTER: the statements
    // below run under it, whichever side of the type word it is written on.
    // (Each row: the two spellings, and the filters of every statement from
    // the first down.)
    let data = "TypeSelector(data)";
    let func = "TypeSelector(function)";
    let either = "Compound((func or field))";
    let but = "Compound((func and not _name_select))";
    let generic = "GenericFilter(type=[1])";
    for (verb, other_side, expected) in [
        (
            r#"data inherit(type) { { "x" } }"#,
            r#"inherit(type) data { { "x" } }"#,
            vec![vec![data], vec![data], vec![data]],
        ),
        (
            r#"func inherit(type) "foo" { data { "x" } }"#,
            r#"inherit(type) "foo" func { data { "x" } }"#,
            vec![vec![func], vec![data], vec![]],
        ),
        // A named type selector is a branch of its own statement and a
        // filter below it.
        (
            r#"func("a") inherit(type) { { "x" } }"#,
            r#"inherit(type) func("a") { { "x" } }"#,
            vec![vec![], vec![func], vec![func]],
        ),
        (
            r#"func or method inherit(type) { { "x" } }"#,
            r#"inherit(type) func or method { { "x" } }"#,
            vec![vec![either], vec![either], vec![either]],
        ),
        (
            r#"func and not g"test_*" inherit(type) { { "x" } }"#,
            r#"inherit(type) func and not g"test_*" { { "x" } }"#,
            vec![vec![but], vec![but], vec![but]],
        ),
        // The generic filter keeps its own argument, and the verb carries it
        // as that argument does.
        (
            r#"filter("type", "func") inherit(type) { { "x" } }"#,
            r#"inherit(type) filter("type", "func") { { "x" } }"#,
            vec![vec![generic], vec![generic], vec![generic]],
        ),
        (
            r#"filter("type", "func", inherit=true) { { "x" } }"#,
            r#"filter("type", "func", inherit=false) inherit(type) { { "x" } }"#,
            vec![vec![generic], vec![generic], vec![generic]],
        ),
        // A statement under a carried type that writes its own replaces it,
        // and its own stays in its statement.
        (
            r#"func inherit(type) "foo" { data { { "x" } } }"#,
            r#"inherit(type) func "foo" { data { { "x" } } }"#,
            vec![vec![func], vec![data], vec![], vec![]],
        ),
    ] {
        let expected: Vec<Vec<String>> = expected
            .into_iter()
            .map(|filters| filters.into_iter().map(str::to_string).collect())
            .collect();
        assert_eq!(filters_down(verb), expected, "{verb}");
        assert_eq!(filters_down(other_side), expected, "{other_side}");
    }
    // Without the verb a type word stays in its statement.
    assert_eq!(
        filters_down(r#"data { { "x" } }"#)[1..],
        [Vec::<String>::new(), Vec::new()]
    );
    // It carries the type and nothing else the statement wrote...
    assert_eq!(
        filters_down(r#"data filter("compound_name", "a") inherit(type) { { "x" } }"#)[1..],
        filters_down(r#"data inherit(type) { { "x" } }"#)[1..]
    );
    // ...and a statement that wrote no type has none to carry.
    assert_eq!(
        filters_down(r#""foo" inherit(type) { { "x" } }"#),
        filters_down(r#""foo" { { "x" } }"#)
    );
    // Each kind carries its own; both are two verbs.
    let both = r#""a" has inherit(relationship) data inherit(type) { { "x" } }"#;
    assert_eq!(edges_down(both), vec!["has", "has"]);
    assert_eq!(
        filters_down(both)[1..],
        [vec![data.to_string()], vec![data.to_string()]]
    );
    assert_eq!(
        edges_down(r#""a" has data inherit(type) { { "x" } }"#),
        vec!["has", "-"]
    );
    assert_eq!(
        filters_down(r#""a" has inherit(relationship) data { { "x" } }"#)[1..],
        [Vec::<String>::new(), Vec::new()]
    );
}

#[test]
fn inherit_says_what_it_takes() {
    let kinds = "write `inherit(relationship)` or `inherit(type)`";
    let not_a_kind = "`inherit` carries a statement's relationship or its type, not";
    let no_part = "carries what its statement writes and is no part of an expression: \
                   write it beside the expression";
    for (q, expected) in [
        (
            r#""a" has inherit { "b" }"#,
            format!("`inherit` needs what to carry: {kinds}"),
        ),
        (
            r#""a" has inherit() { "b" }"#,
            format!("`inherit` needs what to carry: {kinds}"),
        ),
        (
            r#""a" has inherit(expression) { "b" }"#,
            format!("{not_a_kind} `expression`: {kinds}"),
        ),
        (
            r#""a" has inherit(true) { "b" }"#,
            format!("{not_a_kind} `true`: {kinds}"),
        ),
        (
            r#""a" has inherit("relationship") { "b" }"#,
            "the kind is a word, not a string: write `inherit(relationship)`".to_string(),
        ),
        (
            r#"data inherit("type") { "b" }"#,
            "the kind is a word, not a string: write `inherit(type)`".to_string(),
        ),
        (
            r#"data inherit("types") { "b" }"#,
            format!("{not_a_kind} `\"types\"`: {kinds}"),
        ),
        (
            r#""a" has data inherit(relationship, type) { "b" }"#,
            "`inherit` carries one kind: write `inherit(relationship) inherit(type)` to \
             carry both"
                .to_string(),
        ),
        // It is no part of an expression of either kind.
        (
            r#"func or inherit(type) { "b" }"#,
            format!("`inherit(type)` {no_part}"),
        ),
        (
            r#""a" calls or inherit(relationship) { "b" }"#,
            format!("`inherit(relationship)` {no_part}"),
        ),
        (
            r#""a" not inherit(relationship) has { "b" }"#,
            format!("`inherit(relationship)` {no_part}"),
        ),
    ] {
        let err = parse_err(q);
        assert!(err.contains(&expected), "{q}: {err}");
    }
    // The words it is spelled with stay what they were everywhere else.
    for q in [
        r#"filter("type", "func", inherit=true) { "x" }"#,
        r#"type("relationship") { "x" }"#,
        r#""inherit" { "x" }"#,
        r#"@inherit "a"; #inherit { "x" }"#,
    ] {
        parse(q).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", q, e));
    }
    let err = parse_err(r#"inherited(type) { "x" }"#);
    assert!(!err.contains("needs what to carry"), "{err}");
}

#[test]
fn two_relationship_words_are_joined_with_or() {
    for (q, expected) in [
        (
            r#"field("slot") has holds { }"#,
            "join relationship words with `or`: `has or holds`",
        ),
        (
            r#""caller" calls writes { }"#,
            "join relationship words with `or`: `calls or writes`",
        ),
        (
            r#""caller" calls or has (holds) { }"#,
            "join relationship words with `or`: `calls or has or (holds)`",
        ),
        // Apart, they are still two.
        (
            r#"has "a" refs { }"#,
            "join relationship words with `or`: `has or refs`",
        ),
    ] {
        let err = parse_err(q);
        assert!(err.contains(expected), "{q}: {err}");
    }
}

// ---------------------------------------------------------------------------
// What is carried, and what is not
// ---------------------------------------------------------------------------

#[test]
fn a_relationship_reaches_below_only_when_carried() {
    // A WORD GOVERNS THE STATEMENT IT IS WRITTEN IN; `inherit(relationship)`
    // CARRIES IT.  The same wherever the words stand.
    for (written, edges) in [
        ("has inherit(relationship)", vec!["has", "has"]),
        ("refs inherit(relationship)", vec!["refs", "refs"]),
        (
            "refs or has inherit(relationship)",
            vec!["refs or has", "refs or has"],
        ),
        (
            "inherit(relationship) has or refs",
            vec!["has or refs", "has or refs"],
        ),
        ("has", vec!["has", "-"]),
        ("refs or has", vec!["refs or has", "-"]),
    ] {
        assert_eq!(
            edges_down(&format!(r#""a" {written} {{ "b" {{ "c" }} }}"#)),
            edges,
            "{written}"
        );
        assert_eq!(
            edges_down(&format!(r#"{written} "a" {{ "b" {{ "c" }} }}"#)),
            edges,
            "{written}"
        );
    }
    // A PREAMBLE CARRIES WHAT IT WRITES.
    parse(r#"preamble has; "a" { "b" { "c" } }"#).unwrap();
    parse(r#"preamble func; "a" { "b" }"#).unwrap();
}

#[test]
fn a_type_reaches_below_only_when_carried() {
    for q in [
        r#"func inherit(type) { { "x" } }"#,
        r#"any inherit(type) "a" { { "x" } }"#,
        r#"func("a") inherit(type) { { "x" } }"#,
        r#"data(g"a*", match="contains") inherit(type) { { "x" } }"#,
    ] {
        let carried = filters_down(q);
        assert!(!carried[2].is_empty(), "{q}: {carried:?}");
    }
    for q in [r#"data { { "x" } }"#, r#"func("a") { { "x" } }"#] {
        let carried = filters_down(q);
        assert!(carried[2].is_empty(), "{q}: {carried:?}");
    }

    // A relationship is a word and a type word takes no `inherit`: neither
    // has a parameterised spelling.
    for q in [
        r#""a" derive(type="has") { "b" }"#,
        r#"func(inherit=true) { "x" }"#,
        r#"func("a", inherit=false) { "x" }"#,
        r#"has(inherit=true) { "x" }"#,
    ] {
        assert!(parse(q).is_err(), "`{q}` must not parse");
    }

    // The generic filter keeps its parameter: a name filter has no
    // `inherit(kind)`.
    for q in [
        r#"filter("type", "func", inherit=true) { { "x" } }"#,
        r#"filter("compound_name", "a", inherit=true) { { "x" } }"#,
        r#"filter("compound_name", "a", inherit=false) { { "x" } }"#,
    ] {
        parse(q).unwrap_or_else(|e| panic!("`{}` failed to parse: {}", q, e));
    }
    assert!(
        !filters_down(r#"filter("compound_name", "a", inherit=true) { { "x" } }"#)[2].is_empty()
    );
}
