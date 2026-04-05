use crate::test_util::{run_query, TEST_INPUT_TYPE_FILTER};
use index::symbols::SymbolInstanceId;

// Test fixture (test_input_type_filter.sql):
//
// dir_root (Directory, [0-20000])  inst=101
//   └── file_x (File, [0-10000])  inst=102
//         ├── func_a (Function, [100-700])  inst=103
//         │   ├── data_d (Data, [300-400])  inst=106
//         │   └── macro_m (Macro, [500-600])  inst=105
//         │       └── data_macro_only (Data, [510-550])  inst=107
//         └── func_b (Function, [800-900])  inst=104
//
// Containment (HAS) is determined by offset range nesting.
// data_macro_only's has_parents: macro_m, func_a, file_x, dir_root

#[test]
fn has_parent_innermost_only() {
    // { "data_macro_only" } — upward derivation from data_macro_only.
    //
    // data_macro_only's has_parents: macro_m, func_a, file_x, dir_root.
    // With innermost filtering: only macro_m is derived.
    // Outer {} has DefaultTypeFilter([FUNCTION]) → macro_m (type 7) is filtered out.
    // So func_a must NOT appear (it would only appear if all parents were derived).
    const QUERY: &str = r#"{ "data_macro_only" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("has_parent_innermost_only nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should be in results"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should NOT leak through multi-level containment"
    );
}

#[test]
fn nested_macro_not_in_children() {
    // func("func_a") {} — downward derivation from func_a.
    //
    // Direct HAS children of func_a: macro_m, data_d (NOT data_macro_only).
    // data_macro_only is inside macro_m, not directly inside func_a.
    const QUERY: &str = r#"func("func_a") {}"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("nested_macro_not_in_children nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m (direct child)");
    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d (direct child)");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should NOT be a direct child of func_a"
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
    assert!(nodes.contains(&SymbolInstanceId::new(107)), "data_macro_only");
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
    // 3. Outer {} derives innermost has_parent of macro_m: [func_a]
    //    (func_a passes DefaultTypeFilter([FUNCTION]))
    //
    // Each upward hop traverses exactly one containment level.
    // file_x and dir_root should NOT appear (they are not innermost).
    const QUERY: &str = r#"{ { "data_macro_only" } }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("upward_derivation_skips_intermediate nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(107)), "data_macro_only");
    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m (one hop up)");
    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a (two hops up)");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(102)),
        "file_x should NOT appear (not innermost parent of macro_m)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(101)),
        "dir_root should NOT appear (not innermost parent of anything)"
    );
}

#[test]
fn flatten_overrides_innermost() {
    // flatten { "data_macro_only" } — find parents with flatten.
    //
    // With flatten, ALL has_parents are returned instead of just innermost.
    // Outer has DefaultTypeFilter([FUNCTION]) → func_a (type 1) passes.
    // Without flatten (test 1), func_a would NOT appear because innermost
    // is macro_m which gets filtered by the [FUNCTION] type filter.
    const QUERY: &str = r#"flatten { "data_macro_only" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("flatten_overrides_innermost nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should be in results"
    );
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should be found with flatten (skips innermost filtering)"
    );
}

#[test]
fn flatten_downward_does_not_affect_child() {
    // func("func_a") flatten {} — flatten is on the parent statement.
    //
    // The child {} reads its own flatten flag (false) for downward derivation,
    // so direct_only filtering still applies. data_macro_only should NOT appear
    // because it's inside macro_m, not directly inside func_a.
    // flatten on the parent only affects upward (parent merge) derivation.
    const QUERY: &str = r#"func("func_a") flatten {}"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("flatten_downward_does_not_affect_child nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a");
    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m (direct child)");
    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d (direct child)");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should NOT appear (flatten on parent doesn't affect child derivation)"
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
    //    Has DefaultTypeFilter([FUNCTION]) → only functions pass at outermost level.
    //
    // Final result = union of all levels. Middle level's file_x and dir_root
    // persist in the final result because intermediate DefaultTypeFilter([]) allows them.
    const QUERY: &str = r#"{ { { "data_macro_only" } } }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("three_hop_upward nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(107)), "data_macro_only");
    assert!(nodes.contains(&SymbolInstanceId::new(105)), "macro_m (one hop)");
    assert!(nodes.contains(&SymbolInstanceId::new(103)), "func_a (two hops)");
    // file_x and dir_root appear because middle level has DefaultTypeFilter([])
    // and REFS parents of macro_m include them (ref from [200,210) is inside file_x/dir_root)
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
    // Outer {} has DefaultTypeFilter([FUNCTION]) → func_a (type 1) passes.
    const QUERY: &str = r#"{ "data_d" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("innermost_from_data_d nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d");
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should appear (innermost has_parent of data_d, passes FUNCTION filter)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(102)),
        "file_x should NOT appear (not innermost)"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(101)),
        "dir_root should NOT appear (not innermost)"
    );
}

#[test]
fn upward_from_func_b() {
    // { "func_b" } — upward derivation from func_b.
    //
    // func_b's has_parents: file_x, dir_root.
    // Innermost is file_x (directly contains func_b).
    // Outer {} has DefaultTypeFilter([FUNCTION]) → file_x (type 2) is filtered out.
    // So no parent appears in the final result (only func_b itself).
    const QUERY: &str = r#"{ "func_b" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("upward_from_func_b nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(104)), "func_b");
    assert!(
        !nodes.contains(&SymbolInstanceId::new(102)),
        "file_x should be filtered by DefaultTypeFilter([FUNCTION])"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(101)),
        "dir_root should NOT appear (not innermost, and filtered)"
    );
}

#[test]
fn has_only_upward_innermost() {
    // has { "data_macro_only" } — upward using only HAS (containment), no REFS.
    //
    // Innermost has_parent of data_macro_only: macro_m.
    // Outer {} has DefaultTypeFilter([FUNCTION]) → macro_m (type 7) is filtered out.
    // Same result as has_parent_innermost_only, but explicitly using `has` verb.
    const QUERY: &str = r#"has { "data_macro_only" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("has_only_upward_innermost nodes: {:?}", nodes);

    assert!(
        nodes.contains(&SymbolInstanceId::new(107)),
        "data_macro_only should be in results"
    );
    assert!(
        !nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should NOT appear (innermost is macro_m, filtered by [FUNCTION])"
    );
}

#[test]
fn refs_parents_from_macro_m() {
    // { "macro_m" } — upward derivation from macro_m.
    //
    // macro_m's has_parents: func_a, file_x, dir_root.
    // Innermost is func_a.
    // macro_m also has REFS parents: func_a refs macro_m from [200,210).
    // func_a is both REFS parent and innermost HAS parent.
    // Outer {} has DefaultTypeFilter([FUNCTION]) → func_a passes.
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
        !nodes.contains(&SymbolInstanceId::new(102)),
        "file_x should NOT appear (not innermost)"
    );
}

#[test]
fn flatten_all_parents_from_data_d() {
    // flatten { "data_d" } — all parents with flatten.
    //
    // data_d's has_parents: func_a, file_x, dir_root.
    // With flatten, all are returned. DefaultTypeFilter([FUNCTION]) at outer level
    // filters to only func_a (type 1).
    // Without flatten (innermost_from_data_d test), same result since func_a
    // is already the innermost. This test ensures flatten doesn't break simple cases.
    const QUERY: &str = r#"flatten { "data_d" }"#;
    let res = run_query(TEST_INPUT_TYPE_FILTER, QUERY);

    let nodes = res.nodes.as_vec();
    println!("flatten_all_parents_from_data_d nodes: {:?}", nodes);

    assert!(nodes.contains(&SymbolInstanceId::new(106)), "data_d");
    assert!(
        nodes.contains(&SymbolInstanceId::new(103)),
        "func_a should appear (passes FUNCTION filter)"
    );
    // file_x and dir_root are returned by flatten but filtered by DefaultTypeFilter([FUNCTION])
    assert!(
        !nodes.contains(&SymbolInstanceId::new(102)),
        "file_x filtered by DefaultTypeFilter([FUNCTION])"
    );
}
