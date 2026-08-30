use index::symbols::SymbolInstanceId;

use crate::{
    cfg::ControlFlowGraph,
    parser::Value,
    span::Span,
    test_util::{get_shared_index, run_query, VERB_TEST},
    verb::*,
};

#[tokio::test(flavor = "current_thread")]
async fn test_select_matching_name() {
    let index = get_shared_index(VERB_TEST).await;
    let cfg = ControlFlowGraph::from_symbols(index);

    let test_cases = vec![
        ("sort.Sort", vec![96]),
        ("sort.IsSorted", vec![95]),
        ("foo", vec![91]),
        ("bar", vec![92]),
        ("foo.bar", vec![92]),
        ("FOO.bar", vec![]),
        ("FOO", vec![]),
    ];

    for (name, expected_ids) in test_cases {
        let fake_span = Span::synthetic(name);
        let args = Args::of("_name_select").with_named("name", Value::plain(name));
        let selector = NameSelector::new(fake_span, &args).unwrap();

        let result = selector
            .as_selector()
            .unwrap()
            .select_from_all_impl(
                &cfg,
                index::db_diesel::CompositeFilter::And(vec![]),
                index::db_diesel::ScopeContext::Skip,
                index::db_diesel::ScopeContext::Skip,
                &index::db_diesel::EphContext::rooted(cfg.index.load_root_layers().await.unwrap()),
            )
            .await
            .unwrap();

        let mut got_symbol_instances: Vec<SymbolInstanceId> = result
            .unwrap()
            .nodes
            .into_iter()
            .map(|s| SymbolInstanceId::new(s.symbol_instance.id))
            .collect();
        got_symbol_instances.sort();

        let expected_symbol_instances: Vec<SymbolInstanceId> = expected_ids
            .into_iter()
            .map(|i| SymbolInstanceId::new(i))
            .collect();

        assert_eq!(
            got_symbol_instances, expected_symbol_instances,
            "Failed for name: {}",
            name
        );
    }
}

#[test]
fn test_glob_name_selector_construction() {
    use crate::parser::StringKind;

    let named = |name: &str, kind| {
        Args::of("_name_select").with_named(
            "name",
            Value::Str {
                kind,
                text: name.to_string(),
            },
        )
    };

    // Globs are opt-in via g"..."; compound globs are valid too.
    for pattern in ["*ab*", "a.*b", "(*Kubelet).Run", "/src/*"] {
        NameSelector::new(Span::synthetic(pattern), &named(pattern, StringKind::Glob)).unwrap();
    }

    // A glob without any literal would select every symbol.
    let err = NameSelector::new(Span::synthetic("*"), &named("*", StringKind::Glob)).unwrap_err();
    assert!(
        err.to_string().contains("literal"),
        "unexpected error: {err}"
    );

    // Plain strings are never globs — '*' passes through to exact matching
    // (stripped by normalization, restoring pre-glob behavior).
    NameSelector::new(
        Span::synthetic("(*Kubelet).Run"),
        &named("(*Kubelet).Run", StringKind::Plain),
    )
    .unwrap();
}

#[test]
fn test_glob_type_selector_construction() {
    use crate::parser::StringKind;
    use crate::parser_context::SYMBOL_TYPE_FILE;
    use crate::verb::generic::TypeSelector;

    let build = |name: &str, kind, symbol_type_id| {
        TypeSelector::new(
            Span::synthetic(name),
            &Args::of("func").with(Value::Str {
                kind,
                text: name.to_string(),
            }),
            symbol_type_id,
        )
    };

    build("*.go", StringKind::Glob, Some(SYMBOL_TYPE_FILE)).unwrap();
    build("/src/*", StringKind::Glob, Some(SYMBOL_TYPE_FILE)).unwrap();
    assert!(build("*", StringKind::Glob, Some(SYMBOL_TYPE_FILE)).is_err());
    // `any` shares the guard: a glob with no literal is rejected there too.
    assert!(build("*", StringKind::Glob, None).is_err());
    build("ibv_*", StringKind::Glob, None).unwrap();
}

#[test]
fn test_ignore_package_filter() {
    let query = r#"preamble {
    ignore(package="foo")
}
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_ignore_compound_name_filter() {
    // `ignore` is the one place the compound-name predicate is negated:
    // CompoundNameMixin now contributes a token containment beside its
    // lquery, so the exclusion is NOT(containment AND lquery).  That is only
    // equivalent to the old NOT(lquery) because an ordered-subset match
    // implies the containment -- exercise it directly.
    let query = r#"preamble {
    ignore("foo.bar")
}
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_ignore_compound_name_respects_token_order() {
    // Compound names match by ORDERED subset, so "bar.foo" excludes nothing:
    // `foo.bar` has both labels, in the wrong order.
    //
    // This is the negated counterpart of the "run.kubelet" guard in
    // index/src/symbols_test.rs.  The token containment alone cannot tell
    // these apart -- {bar,foo} is a subset of {foo,bar} -- so if the lquery
    // recheck is ever dropped as redundant, this test starts wrongly
    // excluding foo.bar.
    let query = r#"preamble {
    ignore("bar.foo")
}
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(92),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_glob_smart_case_insensitive() {
    // All-lowercase glob matches case-insensitively: g"is*" finds sort.IsSorted.
    let res = run_query("verb_test.sql", r#"g"is*""#);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(95)]);
}

#[test]
fn test_glob_compound_anchored() {
    // Compound globs match the full symbol name, anchored: g"sort.*" requires
    // the name to start with "sort." — "contains" needs explicit wildcards.
    let res = run_query("verb_test.sql", r#"g"sort.*""#);
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(95), SymbolInstanceId::new(96)]
    );

    // A trailing literal without a leading wildcard does not match names that
    // merely contain it: g"ort.*" is anchored and matches nothing.
    let res = run_query("verb_test.sql", r#"g"ort.*""#);
    assert_eq!(res.nodes.as_vec(), vec![]);
}

#[test]
fn test_glob_contains_match_mode() {
    // match="contains" wraps a leaf glob in implicit wildcards.
    let res = run_query("verb_test.sql", r#"func(g"oo*", match="contains")"#);
    // Matches foo(91), foo.bar(92 leaf bar? no), foobar(93): leaves foo, bar,
    // foobar — "%oo%" matches foo and foobar.
    assert_eq!(
        res.nodes.as_vec(),
        vec![SymbolInstanceId::new(91), SymbolInstanceId::new(93)]
    );
}

#[test]
fn test_glob_ignore_filter() {
    // ignore() shares glob semantics with selectors.
    let query = r#"preamble { ignore(g"foob*") }
"foo"
"foobar"
"#;
    let res = run_query("verb_test.sql", query);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(91)]);
}

#[test]
fn test_glob_filter_mode_namespace() {
    // Filter-mode type selectors apply glob semantics too: the compound glob
    // g"sort.*" constrains the selection to symbols under sort.
    let query = r#"filter("compound_name", g"sort.*") "IsSorted""#;
    let res = run_query("verb_test.sql", query);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(95)]);

    let query = r#"filter("compound_name", g"nomatch.*") "IsSorted""#;
    let res = run_query("verb_test.sql", query);
    assert_eq!(res.nodes.as_vec(), vec![]);
}

#[test]
fn test_data_verb() {
    let res = run_query("verb_test.sql", r#"data "Debug";"#);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(97)]);
}

#[test]
fn test_data_verb_full_name() {
    let res = run_query("verb_test.sql", r#"data "config.Debug";"#);
    assert_eq!(res.nodes.as_vec(), vec![SymbolInstanceId::new(97)]);
}

#[test]
fn test_ignore_package_filter_inline() {
    // Single-line preamble still works (backward compat)
    let query = r#"preamble ignore(package="foo")
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_preamble_scope_multiple_ignores() {
    // Multiple ignore verbs in preamble scope
    let query = r#"preamble {
    ignore(package="foo")
    ignore(package="bar")
}
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_preamble_scope_with_semicolons() {
    // Semicolons still work as separators inside preamble scope
    let query = r#"preamble { ignore(package="foo") }
"foo"
"foo.bar"
"foobar"
"tar"
"#;

    let res = run_query("verb_test.sql", query);

    assert_eq!(
        res.nodes.as_vec(),
        vec![
            SymbolInstanceId::new(91),
            SymbolInstanceId::new(93),
            SymbolInstanceId::new(94),
        ]
    );
}

#[test]
fn test_preamble_empty_scope() {
    // preamble with empty scope is a no-op — should not panic
    let query = r#"preamble {
}
"foo"
"tar"
"#;

    let res = run_query("verb_test.sql", query);
    assert!(!res.nodes.as_vec().is_empty());
}

#[test]
#[should_panic(expected = "budget_bounded")]
fn constraining_by_a_truncated_dependency_is_a_bug() {
    // The fence for `perf/BUDGET_GATE.md`: composition retains against a
    // dependency's rows, so consuming one that stopped at a LIMIT would prune
    // neighbours of the rows it never fetched.  `stage_read` keeps this
    // unreachable today by clearing the result budget for every composed
    // statement; when that gate is retired in favour of predicate-carrying
    // composition, this is the assertion that must stay satisfied.
    use crate::execution_state::{DependencyRole, RelationshipType};
    use index::db_diesel::Selection;

    let mut state = SelectorState::new();
    let mut truncated = Selection::new();
    truncated.budget_bounded = true;

    state.constrain_selection(&truncated, &DependencyRole::Child, RelationshipType::REFS);
}

#[test]
fn verb_classes_are_declared_per_aspect() {
    use super::generic::{HasModifier, ProjectFilter, TypeSelector, UnnestModifier};
    use super::labels::{LabelVerb, UserVerb};
    use super::preamble::PreambleVerb;
    use crate::parser_context::SYMBOL_TYPE_FUNCTION;

    let span = || Span::synthetic("v");
    let bare = |verb: &str| Args::of(verb);
    let one = |verb: &str, value: &str| Args::of(verb).with(Value::plain(value));

    let name = NameSelector::new(
        span(),
        &Args::of("_name_select").with_named("name", Value::plain("foo")),
    )
    .unwrap();
    assert_eq!(name.class(), VerbClass::Predicate);

    let func = TypeSelector::new(span(), &bare("func"), Some(SYMBOL_TYPE_FUNCTION)).unwrap();
    assert_eq!(func.class(), VerbClass::Predicate);
    // A bare inherited type selector is filter-only: the selector role is
    // instance-conditional, the class is not.
    assert!(func.as_filter().is_some());
    assert!(func.as_selector().is_none());

    let project = ProjectFilter::new(span(), &one("project", "p")).unwrap();
    assert_eq!(project.class(), VerbClass::Predicate);

    let unnest = UnnestModifier::new(span(), &bare("unnest")).unwrap();
    assert_eq!(unnest.class(), VerbClass::Relationship);
    let has = HasModifier::new(span(), &bare("has")).unwrap();
    assert_eq!(has.class(), VerbClass::Relationship);

    let label = LabelVerb::new(span(), &one("label", "l")).unwrap();
    assert_eq!(label.class(), VerbClass::Binding);
    let user = UserVerb::new(span(), &one("use", "l")).unwrap();
    assert_eq!(user.class(), VerbClass::Binding);

    let preamble = PreambleVerb::new(span(), &bare("preamble")).unwrap();
    assert_eq!(preamble.class(), VerbClass::Env);
}

/// The slot cascade at the Command level: writing a dimension replaces the
/// previous holder wholesale; dimensionless verbs accumulate.
#[test]
fn slot_cascade_replaces_within_a_dimension() {
    use super::generic::{IgnoreVerb, ProjectFilter, TypeSelector};
    use crate::command::Command;
    use crate::parser_context::{SYMBOL_TYPE_DATA, SYMBOL_TYPE_FUNCTION};

    let span = || Span::synthetic("v");
    let one = |verb: &str, value: &str| Args::of(verb).with(Value::plain(value));

    // project(a) project(b) -> b holds the project slot alone.
    let mut cmd = Command::new(span());
    cmd.extend(ProjectFilter::new(span(), &one("project", "a")).unwrap());
    cmd.extend(ProjectFilter::new(span(), &one("project", "b")).unwrap());
    let filters: Vec<String> = cmd.filters().map(|f| format!("{}", f)).collect();
    assert_eq!(filters, vec!["ProjectFilter(project=b)".to_string()]);

    // func data -> data holds the type slot alone.
    let mut cmd = Command::new(span());
    cmd.extend(TypeSelector::new(span(), &Args::of("func"), Some(SYMBOL_TYPE_FUNCTION)).unwrap());
    cmd.extend(TypeSelector::new(span(), &Args::of("data"), Some(SYMBOL_TYPE_DATA)).unwrap());
    let filters: Vec<String> = cmd.filters().map(|f| format!("{}", f)).collect();
    assert_eq!(filters, vec!["TypeSelector(data)".to_string()]);

    // Exclusions have no dimension: ignore(a) ignore(b) both survive.
    let mut cmd = Command::new(span());
    cmd.extend(IgnoreVerb::new(span(), &one("ignore", "a")).unwrap());
    cmd.extend(IgnoreVerb::new(span(), &one("ignore", "b")).unwrap());
    assert_eq!(cmd.filters().count(), 2);
}

/// `filter("type", ...)` and the bare type verbs share the type dimension,
/// so they replace each other SYMMETRICALLY — under the old per-verb merge
/// hooks `func filter("type","data")` kept both (an always-false
/// conjunction) while the reverse order replaced.
#[test]
fn slot_cascade_type_dimension_is_symmetric() {
    use super::generic::{GenericFilter, TypeSelector};
    use crate::command::Command;
    use crate::parser_context::SYMBOL_TYPE_FUNCTION;

    let span = || Span::synthetic("v");
    let type_filter = |v: &str| {
        GenericFilter::new(
            span(),
            &Args::of("filter")
                .with(Value::plain("type"))
                .with(Value::plain(v)),
        )
        .unwrap()
    };
    let func = || TypeSelector::new(span(), &Args::of("func"), Some(SYMBOL_TYPE_FUNCTION)).unwrap();

    let mut cmd = Command::new(span());
    cmd.extend(type_filter("data"));
    cmd.extend(func());
    let filters: Vec<String> = cmd.filters().map(|f| format!("{}", f)).collect();
    assert_eq!(filters, vec!["TypeSelector(function)".to_string()]);

    let mut cmd = Command::new(span());
    cmd.extend(func());
    cmd.extend(type_filter("data"));
    let filters: Vec<String> = cmd.filters().map(|f| format!("{}", f)).collect();
    assert_eq!(filters, vec!["GenericFilter(type=[6])".to_string()]);
}

/// A type verb no longer evicts layer-backed anchors: `search`/`loc`/`layer`
/// suppress the DEFAULT type filter (a parse-time injection question) but do
/// not occupy the type dimension.  Under the old hooks `search("x") func`
/// silently dropped the search anchor.
#[test]
fn slot_cascade_type_write_keeps_layer_backed_anchors() {
    use super::generic::{SearchSelector, TypeSelector};
    use crate::command::Command;
    use crate::parser_context::SYMBOL_TYPE_FUNCTION;

    let span = || Span::synthetic("v");

    let mut cmd = Command::new(span());
    cmd.extend(SearchSelector::new(span(), &Args::of("search").with(Value::plain("xyz"))).unwrap());
    cmd.extend(TypeSelector::new(span(), &Args::of("func"), Some(SYMBOL_TYPE_FUNCTION)).unwrap());
    assert!(
        cmd.has_layer_spec(),
        "the search anchor must survive a type-dimension write"
    );
    assert_eq!(cmd.filters().count(), 1);
}
