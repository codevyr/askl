use super::normalize_full_path;

#[test]
fn normalize_resolves_dotdot() {
    assert_eq!(normalize_full_path("/a/b/../c"), "/a/c");
    assert_eq!(normalize_full_path("/a/b/../../c"), "/c");
}

#[test]
fn normalize_resolves_dot() {
    assert_eq!(normalize_full_path("/a/./b"), "/a/b");
    assert_eq!(normalize_full_path("/a/./b/./c"), "/a/b/c");
}

#[test]
fn normalize_collapses_slashes() {
    assert_eq!(normalize_full_path("/a//b///c"), "/a/b/c");
}

#[test]
fn normalize_root_edge_cases() {
    assert_eq!(normalize_full_path("/"), "/");
    assert_eq!(normalize_full_path("/../a"), "/a");
    assert_eq!(normalize_full_path("/a/b/../../.."), "/");
}

#[test]
fn normalize_real_world_bug_report() {
    assert_eq!(
        normalize_full_path("/home/user/project/common/mmu/../../include/header.h"),
        "/home/user/project/include/header.h"
    );
}

#[test]
fn normalize_backslash_conversion() {
    assert_eq!(normalize_full_path("/a\\b/c"), "/a/b/c");
}

#[test]
fn normalize_mixed() {
    assert_eq!(normalize_full_path("/a/./b/../c//d"), "/a/c/d");
}
