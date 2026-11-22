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
    // "Relationship expressions" below.  `derive(...)` is not one.)
    let err = parse_err(r#"not derive(type="has") "a""#);
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
    let ast = parse(r#"func(inherit=true) and not g"test_*" { "x" }"#).unwrap();
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
    let err = parse_err(r#"derive(type="has") or calls { "x" }"#);
    assert!(err.contains("`derive` is a relationship verb"), "{err}");
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
