use super::{hash_bytes, normalize_full_path, path_basename};

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

// path_basename

#[test]
fn basename_returns_last_component() {
    assert_eq!(path_basename("/a/b/c"), "c");
}

#[test]
fn basename_single_component() {
    assert_eq!(path_basename("/file.c"), "file.c");
}

#[test]
fn basename_root_returns_slash() {
    assert_eq!(path_basename("/"), "/");
}

#[test]
fn basename_normalizes_before_splitting() {
    // trailing slash stripped, dotdot resolved
    assert_eq!(path_basename("/a/b/"), "b");
    assert_eq!(path_basename("/a/b/../c"), "c");
}

// hash_bytes

#[test]
fn hash_bytes_is_64_hex_chars() {
    let h = hash_bytes(b"anything");
    assert_eq!(h.len(), 64);
    assert!(h.chars().all(|c| c.is_ascii_hexdigit()));
}

#[test]
fn hash_bytes_deterministic() {
    assert_eq!(hash_bytes(b"hello world"), hash_bytes(b"hello world"));
}

#[test]
fn hash_bytes_distinct_for_different_inputs() {
    assert_ne!(hash_bytes(b"abc"), hash_bytes(b"abd"));
}

#[test]
fn hash_bytes_empty_known_value() {
    // SHA-256 of the empty string is well-defined
    assert_eq!(
        hash_bytes(b""),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
}
