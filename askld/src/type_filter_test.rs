use crate::test_util::{run_query, TEST_INPUT_TYPE_FILTER};
use index::symbols::SymbolInstanceId;

// Test fixture (test_input_type_filter.sql):
//
// dir_root (Directory, [0-20000])  inst=101
//   └── file_x (File, [0-10000])  inst=102
//         ├── func_a (Function, [100-700])  inst=103
//         │   ├── data_d (Data, [300-400])  inst=106
//         │   └── macro_m (Macro, [500-600])  inst=105
//         │       └── data_macro_only (Data, [510-550])  inst=107   FOLDED
//         └── func_b (Function, [800-900])  inst=104
//             └── gen_g (Function, [820-880])  inst=108             FOLDED
//                 └── data_g (Data, [830-840])  inst=109
//
// Containment (HAS) is determined by offset range nesting.
// data_macro_only's has_parents: macro_m, func_a, file_x, dir_root
//
// FOLDED is a property of the SYMBOL: not listed by default.  The two folded
// symbols are the two shapes there are -- a leaf nothing is inside, and a
// container with an unfolded symbol inside it.  Folding never decides what is
// reached, only what is listed on the way.
//
// Root-level default: all types (no filtering).

#[test]
fn upward_lists_every_container_that_is_not_folded() {
    // { "data_macro_only" } — upward derivation from data_macro_only.
    //
    // A bare `{"x"}` surfaces every container of x whose symbol is not folded:
    // macro_m, func_a, file_x, dir_root.  x itself is folded, and is there
    // because it was NAMED.
    const QUERY: &str = r#"{ "data_macro_only" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!(
        "upward_lists_every_container_that_is_not_folded nodes: {:?}",
        nodes
    );

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)),
        "macro_m contains it"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "and so does func_a: there is no boundary at macro_m to stop at"
    );
}

#[test]
fn upward_passes_over_a_folded_container() {
    // { "data_g" } — data_g sits in gen_g, which sits in func_b.
    //
    // gen_g is folded: the shape of `f() { CHECK(sizeof(struct x)); }`, where
    // the reference to `x` sits in the declaration `CHECK` generated.  Asking
    // who contains `x` should answer `f`, not the generated declaration --
    // and `f` must not be lost because something folded stands in between.
    let ids = |q: &str| {
        run_query(TEST_INPUT_TYPE_FILTER, q)
            .nodes
            .as_vec()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
    };

    let default = ids(r#"{ "data_g" }"#);
    assert!(default.contains(&SymbolInstanceId::new(109)), "data_g");
    assert!(
        default.contains(&SymbolInstanceId::new(104)),
        "func_b contains it, through gen_g: {:?}",
        default
    );
    assert!(
        !default.contains(&SymbolInstanceId::new(108)),
        "gen_g is folded, so it is not listed: {:?}",
        default
    );

    let unnest = ids(r#"unnest { "data_g" }"#);
    assert!(
        unnest.contains(&SymbolInstanceId::new(108)),
        "`unnest` lists it: {:?}",
        unnest
    );

    let named = ids(r#""gen_g" { "data_g" }"#);
    assert!(
        named.contains(&SymbolInstanceId::new(108)) && named.contains(&SymbolInstanceId::new(109)),
        "and so does naming it: {:?}",
        named
    );
}

#[test]
fn a_folded_symbol_is_not_listed_among_children() {
    // func("func_a") {} — downward derivation from func_a.
    //
    // `X { }` lists X's descendants whose symbol is not folded: macro_m and
    // data_d.  data_macro_only is folded and is left out -- because of what it
    // is, not because it sits inside macro_m.
    const QUERY: &str = r#"func("func_a") {}"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!(
        "a_folded_symbol_is_not_listed_among_children nodes: {:?}",
        nodes
    );

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m");
    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only is folded"
    );
}

#[test]
fn children_are_reached_through_a_folded_container() {
    // func("func_b") {} — func_b contains gen_g (folded), which contains
    // data_g (not folded).
    //
    // Folding hides gen_g and nothing else.  It used to be a BOUNDARY: what
    // sat inside a folded instance was one fold level down and out of reach,
    // so a reference typed in a generating macro's arguments had to be kept
    // out of the generated declaration or its function lost it.  Now the
    // declaration can simply contain it.
    const QUERY: &str = r#"func("func_b") {}"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!(
        "children_are_reached_through_a_folded_container nodes: {:?}",
        nodes
    );

    assert!(nodes.contains(&SymbolInstanceId::new(104)), "func_b");
    assert!(
        nodes.contains(&SymbolInstanceId::new(109)),
        "data_g is reached through gen_g"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(108)),
        "gen_g itself is folded"
    );
}

#[test]
fn multi_hop_constrains_correctly() {
    // func("func_a") { { "data_macro_only" } } — 2 hops from func_a to data_macro_only.
    //
    // 1. func_a computes: [func_a]
    // 2. Middle {} derives direct children of func_a: [macro_m, data_d]
    // 3. Inner "data_macro_only" computes: [data_macro_only]
    // 4. Inner notifies middle {} (Parent): constrain middle {} — retain nodes that
    //    are parents of data_macro_only → macro_m retained, data_d dropped.
    const QUERY: &str = r#"func("func_a") { { "data_macro_only" } }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("multi_hop_constrains_correctly nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m");
    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(106)),
        "data_d should be constrained away (not a parent of data_macro_only)"
    );
}

#[test]
fn upward_derivation_skips_intermediate() {
    // { { "data_macro_only" } } — 2 hops upward from data_macro_only.
    //
    // 1. data_macro_only computes: [data_macro_only]
    // 2. Inner {} derives innermost has_parent: [macro_m]
    //    (inner has DefaultTypeFilter([]) — all types pass)
    // 3. Outer {} derives parents of macro_m:
    //    - HAS innermost: func_a
    //    - REFS parents: func_a (from ref [200,210)), file_x, dir_root
    //    Outer has DefaultTypeFilter([]) — all types pass
    //
    // file_x and dir_root appear via REFS parents with all-types default.
    const QUERY: &str = r#"{ { "data_macro_only" } }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("upward_derivation_skips_intermediate nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)),
        "macro_m (one hop up)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a (two hops up)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(102)),
        "file_x appears via REFS parents with all-types default"
    );
}

#[test]
fn unnest_overrides_innermost() {
    // unnest { "data_macro_only" } — find parents with unnest.
    //
    // With unnest, ALL has_parents are returned instead of just innermost.
    // Root has DefaultTypeFilter([]) → all types pass.
    // func_a appears because all containment levels are included.
    const QUERY: &str = r#"unnest { "data_macro_only" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest_overrides_innermost nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should be found with unnest (skips innermost filtering)"
    );
}

#[test]
fn unnest_downward_does_not_affect_child() {
    // func("func_a") unnest {} — unnest is on the parent statement.
    //
    // unnest on the parent only affects upward (parent merge) derivation; the
    // child {} reads its own flag for downward derivation, so the default
    // still applies there and the folded data_macro_only stays out.
    const QUERY: &str = r#"func("func_a") unnest {}"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest_downward_does_not_affect_child nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)),
        "macro_m (direct child)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(106)),
        "data_d (direct child)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only stays unlisted — unnest on the PARENT does not \
         change the child's downward derivation"
    );
}

#[test]
fn three_hop_upward() {
    // { { { "data_macro_only" } } } — 3 hops upward from data_macro_only.
    //
    // 1. data_macro_only computes: [data_macro_only]
    // 2. Inner {} derives innermost has_parent: [macro_m]
    // 3. Middle {} derives parents of macro_m:
    //    - HAS innermost: func_a
    //    - REFS parents: func_a (from ref [200,210)), file_x, dir_root
    //    Middle has DefaultTypeFilter([]) → all pass → [func_a, file_x, dir_root]
    // 4. Outer {} derives parents of [func_a, file_x, dir_root]:
    //    Has DefaultTypeFilter([]) → all types pass at outermost level.
    //
    // Final result = union of all levels.
    const QUERY: &str = r#"{ { { "data_macro_only" } } }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("three_hop_upward nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)),
        "macro_m (one hop)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a (two hops)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(102)),
        "file_x appears via REFS parents at intermediate level"
    );
}

#[test]
fn innermost_from_data_d() {
    // { "data_d" } — upward derivation from data_d.
    //
    // data_d's has_parents: func_a, file_x, dir_root.
    // Innermost is func_a (directly contains data_d, no intermediary).
    // REFS parents include func_a, file_x, dir_root (ref from [150,160) contained by all).
    // Root has DefaultTypeFilter([]) → all types pass.
    const QUERY: &str = r#"{ "data_d" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("innermost_from_data_d nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d");
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should appear (innermost HAS parent and REFS parent)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(102)),
        "file_x appears via REFS parents with all-types default"
    );
}

#[test]
fn upward_from_func_b() {
    // { "func_b" } — upward derivation from func_b.
    //
    // func_b's has_parents: file_x, dir_root.
    // Innermost is file_x (directly contains func_b).
    // Root has DefaultTypeFilter([]) → all types pass → file_x appears.
    const QUERY: &str = r#"{ "func_b" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("upward_from_func_b nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(104)), "func_b");
    assert!(
        nodes.contains(&SymbolInstanceId::new(102)),
        "file_x should appear (innermost HAS parent, passes all-types filter)"
    );
}

#[test]
fn has_only_upward_innermost() {
    // has { "data_macro_only" } — upward using only HAS (containment), no REFS.
    //
    // The containment chain comes back, every container whose symbol is not
    // folded: macro_m and func_a above it.
    const QUERY: &str = r#"has { "data_macro_only" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("has_only_upward_innermost nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)),
        "macro_m (HAS parent, passes all-types filter)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a (HAS parent, two levels up)"
    );
}

#[test]
fn refs_parents_from_macro_m() {
    // { "macro_m" } — upward derivation from macro_m.
    //
    // macro_m's has_parents: func_a, file_x, dir_root.
    // Innermost is func_a.
    // macro_m also has REFS parents: func_a refs macro_m from [200,210).
    // REFS parents include func_a, file_x, dir_root (all contain [200,210)).
    // Root has DefaultTypeFilter([]) → all types pass.
    const QUERY: &str = r#"{ "macro_m" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("refs_parents_from_macro_m nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m");
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should appear (innermost HAS parent and REFS parent)"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(102)),
        "file_x appears via REFS parents with all-types default"
    );
}

#[test]
fn unnest_all_parents_from_data_d() {
    // unnest { "data_d" } — all parents with unnest.
    //
    // data_d's has_parents: func_a, file_x, dir_root.
    // With unnest, all are returned. DefaultTypeFilter([]) at outer level
    // means all types pass.
    const QUERY: &str = r#"unnest { "data_d" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("unnest_all_parents_from_data_d nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d");
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should appear"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(102)),
        "file_x should appear (unnest returns all parents)"
    );
}

// ============================================================================
// `any` — the type-agnostic member of the type-verb family.
// ============================================================================

#[test]
fn any_with_a_name_ignores_the_symbol_type() {
    // `macro_m` is a macro, so the typed member of the family finds nothing
    // while `any` finds it.  The name used to have to be written OUTSIDE the
    // parentheses (`any "macro_m"`); written inside, it was silently
    // discarded and the statement then failed the anchor check complaining
    // that no name was given.
    let named = run_query(TEST_INPUT_TYPE_FILTER, r#"any("macro_m")"#);
    assert_eq!(
        named.nodes.as_vec(),
        vec![SymbolInstanceId::new(105)],
        "any(name) must match whatever type carries that name"
    );

    let typed = run_query(TEST_INPUT_TYPE_FILTER, r#"func("macro_m")"#);
    assert!(
        typed.nodes.as_vec().is_empty(),
        "func(name) still constrains by type"
    );
}

#[test]
fn any_name_inside_and_outside_the_parentheses_agree() {
    // The two spellings share `name_filter`, so they are one query.  This is
    // the invariant that keeps them from drifting: if the predicate for
    // `any(...)` is ever built somewhere else, this fails.
    for name in ["macro_m", "func_a", "data_macro_only"] {
        let inside = run_query(TEST_INPUT_TYPE_FILTER, &format!(r#"any("{name}")"#));
        let outside = run_query(TEST_INPUT_TYPE_FILTER, &format!(r#"any "{name}""#));
        assert_eq!(
            inside.nodes.as_vec(),
            outside.nodes.as_vec(),
            "any(\"{name}\") and any \"{name}\" must be the same query"
        );
    }
}

#[test]
fn any_takes_globs_like_the_rest_of_the_family() {
    // Glob support is not special-cased for `any`; it comes from the shared
    // `NamePattern`.  `data_*` spans two data symbols.
    let res = run_query(TEST_INPUT_TYPE_FILTER, r#"any(g"data_*")"#);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(106)) && nodes.contains(&SymbolInstanceId::new(107)),
        "any(glob) must match across the index, got {nodes:?}"
    );
}

#[test]
fn bare_any_still_drops_the_inherited_type_filter() {
    // The behaviour `any` already had: a child scope of a function sees only
    // functions by default, and `any` removes that constraint.  func_a
    // references data_d (106) and macro_m (105), neither of them functions.
    let constrained = run_query(TEST_INPUT_TYPE_FILTER, r#"func("func_a") { func }"#);
    assert!(
        !constrained
            .nodes
            .as_vec()
            .contains(&SymbolInstanceId::new(105)),
        "a func-typed child must not match the macro"
    );

    let unconstrained = run_query(TEST_INPUT_TYPE_FILTER, r#"func("func_a") { any }"#);
    let nodes = unconstrained.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)) && nodes.contains(&SymbolInstanceId::new(106)),
        "bare `any` must let the macro and the data symbol through, got {nodes:?}"
    );
}

#[test]
fn bare_any_does_not_anchor() {
    // `any` alone denotes the whole index, so it is a constraint and not a
    // reason for a statement to exist — same as bare `func`.  `select` is the
    // verb that means "enumerate".
    let err = crate::test_util::run_query_err(TEST_INPUT_TYPE_FILTER, r#"any"#);
    assert!(err.is_err(), "bare `any` must still fail the anchor check");
}

#[test]
fn bare_any_propagates_the_absence_of_a_type() {
    // `func` propagates its type to descendants; `any` propagates the absence
    // of one, so a scope under `any` is unconstrained without having to
    // repeat it.  macro_m (105) sits under func_a and is reached through a
    // plain `{ }` grandchild.
    const QUERY: &str = r#"func("func_a") { any { } }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);
    let nodes = res.nodes.as_vec();
    assert!(
        nodes.contains(&SymbolInstanceId::new(105)),
        "a grandchild under `any` must not inherit the function filter, got {nodes:?}"
    );
}

// --- nested: exactly one structural level ---------------------------------
//
// The third traversal mode, and the only reader of `nesting_level`.  Where
// the default lists every descendant whose symbol is not folded -- at any
// depth, so `mod { func }` works -- `nested` lists every descendant at the
// next NESTING level, folded or not, which is the one question the other two
// modes cannot ask.
//
// The mode belongs to the statement whose reach it bounds: for downward
// derivation that is the statement INSIDE the braces, which is why these read
// `file("file_x") { nested }` rather than `file("file_x") nested { }`.

#[test]
fn nested_reaches_exactly_one_structural_level() {
    // file_x sits at nesting_level 1, so `nested` admits level 2 and nothing
    // deeper: the two functions, but not what they contain.
    const QUERY: &str = r#"file("file_x") { nested }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);
    let nodes = res.nodes.as_vec();

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(nodes.contains(&SymbolInstanceId::new(104)), "func_b");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(106)),
        "data_d is two levels down, so `nested` stops short of it"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(105)),
        "macro_m is two levels down"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only is three levels down"
    );
}

#[test]
fn nested_is_narrower_than_the_default_and_the_default_narrower_than_unnest() {
    // The same scope in all three modes, so the widening is visible in one
    // place.  data_d is the discriminator between `nested` and the default:
    // it is two levels below file_x, so the default lists it and `nested`
    // does not.  data_macro_only is the discriminator between the default and
    // `unnest`: its symbol is folded.
    let ids = |q: &str| {
        run_query(TEST_INPUT_TYPE_FILTER, q)
            .nodes
            .as_vec()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
    };

    let nested = ids(r#"file("file_x") { nested }"#);
    let default = ids(r#"file("file_x") { }"#);
    let unnest = ids(r#"file("file_x") { unnest }"#);

    assert!(
        nested.is_subset(&default) && default.is_subset(&unnest),
        "the three modes must widen monotonically\nnested: {:?}\ndefault: {:?}\nunnest: {:?}",
        nested,
        default,
        unnest
    );
    assert!(
        default.contains(&SymbolInstanceId::new(106))
            && !nested.contains(&SymbolInstanceId::new(106)),
        "data_d separates `nested` from the default"
    );
    assert!(
        unnest.contains(&SymbolInstanceId::new(107))
            && !default.contains(&SymbolInstanceId::new(107)),
        "data_macro_only separates the default from `unnest`"
    );
}

#[test]
fn a_named_child_is_found_though_folded() {
    // FOLDING DECIDES WHAT IS LISTED, NOT WHAT CAN BE ASKED FOR.
    //
    // `func("func_a") { }` enumerates func_a's contents and leaves out
    // data_macro_only, whose symbol is folded (the test above).  A statement
    // that NAMES the symbol is not an enumeration: the user asked for that
    // symbol, and whether it is shown by default is beside the point.  The
    // same holds for a glob, which is still a name.
    //
    // Nothing implements this specially.  A named child selects by name and is
    // then constrained by its parent's interval; the fold filter exists only
    // on the path that derives an unnamed child from its parent.
    let ids = |q: &str| {
        run_query(TEST_INPUT_TYPE_FILTER, q)
            .nodes
            .as_vec()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
    };

    let enumerated = ids(r#"func("func_a") { }"#);
    assert!(
        !enumerated.contains(&SymbolInstanceId::new(107)),
        "precondition: enumeration does not list a folded symbol"
    );

    let named = ids(r#"func("func_a") { "data_macro_only" }"#);
    assert!(
        named.contains(&SymbolInstanceId::new(103)) && named.contains(&SymbolInstanceId::new(107)),
        "a child named exactly is returned though folded: {:?}",
        named
    );

    let globbed = ids(r#"func("func_a") { g"data_macro_*" }"#);
    assert!(
        globbed.contains(&SymbolInstanceId::new(103))
            && globbed.contains(&SymbolInstanceId::new(107)),
        "a child named by a glob is returned though folded: {:?}",
        globbed
    );
}

#[test]
fn a_typed_statement_lists_by_folding_too() {
    // `file { func }` is still an enumeration -- every function in the file --
    // so the folded gen_g is left out, exactly like `mod { func }` leaving out
    // `__compiletime_assert_N`.  `unnest` lists it, and so does its name.
    let ids = |q: &str| {
        run_query(TEST_INPUT_TYPE_FILTER, q)
            .nodes
            .as_vec()
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
    };

    let default = ids(r#"file("file_x") { func }"#);
    assert!(
        default.contains(&SymbolInstanceId::new(103))
            && default.contains(&SymbolInstanceId::new(104)),
        "func_a and func_b: {:?}",
        default
    );
    assert!(
        !default.contains(&SymbolInstanceId::new(108)),
        "gen_g is folded: {:?}",
        default
    );

    let unnest = ids(r#"file("file_x") { unnest func }"#);
    assert!(
        unnest.contains(&SymbolInstanceId::new(108)),
        "`unnest` lists it: {:?}",
        unnest
    );

    let named = ids(r#"file("file_x") { func("gen_g") }"#);
    assert!(
        named.contains(&SymbolInstanceId::new(108)),
        "and so does naming it: {:?}",
        named
    );
}

#[test]
fn nested_upward_returns_only_the_immediate_container() {
    // Upward, the default surfaces the whole ancestor chain (see
    // has_parent_innermost_only).  `nested` asks for just the one container
    // data_d literally sits in.
    const QUERY: &str = r#"nested { "data_d" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);
    let nodes = res.nodes.as_vec();

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(102)),
        "file_x is two levels up, and `nested` is one"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(101)),
        "dir_root is three levels up"
    );
}
