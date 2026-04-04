use anyhow::Result;
use clang_ast::SourceRange;
use serde::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fmt;
use std::fs;
use std::path::PathBuf;
use std::{collections::HashMap, hash, hash::Hasher};

use crate::db::{self};

#[derive(Debug, PartialEq, Eq, PartialOrd, Hash, Ord, Copy, Clone, Serialize, Deserialize)]
pub struct FileHash(u64);

impl FileHash {
    pub fn new<T: hash::Hash>(url: &T) -> Self {
        let mut s = DefaultHasher::new();
        url.hash(&mut s);
        FileHash(s.finish())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
pub struct Occurrence {
    pub offset_range: (i32, i32),
    pub file: FileId,
}

impl Occurrence {
    pub fn new(range: &Option<SourceRange>, file_id: FileId) -> Option<Self> {
        let (start_offset, end_offset) = Self::offsets_from_range(range)?;
        Some(Self {
            offset_range: (start_offset, end_offset),
            file: file_id,
        })
    }

    pub(crate) fn offsets_from_range(range: &Option<SourceRange>) -> Option<(i32, i32)> {
        let range = range.as_ref()?;
        let begin = range.begin.expansion_loc.as_ref()?;
        let end = range.end.expansion_loc.as_ref()?;
        let file = begin.file.clone().to_string();
        let content = fs::read(&file).ok()?;
        let start_offset = offset_from_line_col(&content, begin.line as usize, begin.col as usize)?;
        let end_offset = offset_from_line_col(&content, end.line as usize, end.col as usize)?;
        Some((start_offset, end_offset))
    }

    pub fn get_file(range: &Option<SourceRange>) -> Option<PathBuf> {
        let range = if let Some(range) = range {
            range
        } else {
            return None;
        };

        let begin = if let Some(begin) = &range.begin.expansion_loc {
            begin
        } else {
            return None;
        };

        let file = begin.file.clone().to_string();
        if let Ok(path) = fs::canonicalize(file.clone()) {
            Some(path)
        } else {
            Some(PathBuf::from(file))
        }
    }
}

fn offset_from_line_col(content: &[u8], line: usize, col: usize) -> Option<i32> {
    if line == 0 || col == 0 {
        return None;
    }

    let mut current_line = 1usize;
    let mut line_start = 0usize;
    for (idx, byte) in content.iter().enumerate() {
        if *byte == b'\n' {
            if current_line == line {
                let line_len = idx.saturating_sub(line_start);
                let col_idx = col.saturating_sub(1);
                if col_idx <= line_len {
                    return Some((line_start + col_idx) as i32);
                }
                return None;
            }
            current_line += 1;
            line_start = idx + 1;
        }
    }

    if current_line == line {
        let line_len = content.len().saturating_sub(line_start);
        let col_idx = col.saturating_sub(1);
        if col_idx <= line_len {
            return Some((line_start + col_idx) as i32);
        }
    }

    None
}

impl From<db::SymbolInstance> for Occurrence {
    fn from(symbol: db::SymbolInstance) -> Self {
        Occurrence {
            offset_range: symbol.offset_range,
            file: symbol.file_id,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash)]
pub struct Reference {
    pub from: SymbolInstanceId,
    pub to: SymbolId,
    pub occurrence: Option<Occurrence>,
}

impl Reference {
    pub fn new(from: SymbolInstanceId, to: SymbolId) -> Self {
        Self {
            from,
            to,
            occurrence: None,
        }
    }

    pub fn new_occurrence(from: SymbolInstanceId, to: SymbolId, occurrence: Occurrence) -> Self {
        Self {
            from,
            to,
            occurrence: Some(occurrence),
        }
    }
}

pub type SymbolRefs = HashMap<SymbolId, HashSet<Occurrence>>;
pub type SymbolInstanceRefs = HashMap<SymbolInstanceId, HashSet<Occurrence>>;

#[derive(Debug, Deserialize, Clone, Default)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub name_split: Vec<String>,
    pub instances: HashSet<SymbolInstanceId>,
    pub children: SymbolRefs,
    pub parents: SymbolInstanceRefs,
}

impl Symbol {
    pub fn new(id: SymbolId, name: String) -> Self {
        Self {
            id,
            name_split: clean_and_split_string(&name),
            name,
            instances: HashSet::new(),
            children: SymbolRefs::new(),
            parents: SymbolInstanceRefs::new(),
        }
    }

    pub fn add_child(&mut self, id: SymbolId, occurrence: Occurrence) {
        self.children
            .entry(id)
            .and_modify(|occurences| {
                occurences.insert(occurrence.clone());
            })
            .or_insert(HashSet::from([occurrence]));
    }

    pub fn add_parent(&mut self, id: SymbolInstanceId, occurrence: Occurrence) {
        self.parents
            .entry(id)
            .and_modify(|occurences| {
                occurences.insert(occurrence.clone());
            })
            .or_insert(HashSet::from([occurrence]));
    }
}

pub trait Symbols {
    fn into_vec(&self) -> Vec<SymbolId>;
}

#[derive(
    Debug, Default, Serialize, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord,
)]
pub struct SymbolId(pub i32);

impl SymbolId {
    pub fn new(id: i32) -> Self {
        Self(id)
    }
}

impl From<clang_ast::Id> for SymbolId {
    fn from(string: clang_ast::Id) -> Self {
        let value = format!("{}", string)
            .strip_prefix("0x")
            .and_then(|hex| u64::from_str_radix(hex, 16).ok())
            .unwrap();
        Self(value as i32)
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Option<i32>> for SymbolId {
    fn from(value: Option<i32>) -> Self {
        Self(value.unwrap())
    }
}

impl From<Option<i64>> for SymbolId {
    fn from(value: Option<i64>) -> Self {
        Self(value.unwrap() as i32)
    }
}

impl From<i32> for SymbolId {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<i64> for SymbolId {
    fn from(value: i64) -> Self {
        Self(value as i32)
    }
}

#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct ProjectId(i32);

impl ProjectId {
    pub fn new(id: i32) -> Self {
        Self(id)
    }
}

impl From<i32> for ProjectId {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<i64> for ProjectId {
    fn from(value: i64) -> Self {
        Self(value as i32)
    }
}

impl fmt::Display for ProjectId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl serde::Serialize for ProjectId {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&format!("{}", self.0))
    }
}

#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct ModuleId(i32);

impl ModuleId {
    pub fn new(id: i32) -> Self {
        Self(id)
    }
}

impl From<i32> for ModuleId {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<i64> for ModuleId {
    fn from(value: i64) -> Self {
        Self(value as i32)
    }
}

impl fmt::Display for ModuleId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl serde::Serialize for ModuleId {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&format!("{}", self.0))
    }
}

#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct FileId(i32);

impl FileId {
    pub fn new(id: i32) -> Self {
        Self(id)
    }
}

impl From<i32> for FileId {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl From<i64> for FileId {
    fn from(value: i64) -> Self {
        Self(value as i32)
    }
}

impl Into<i32> for FileId {
    fn into(self) -> i32 {
        self.0
    }
}

impl fmt::Display for FileId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl serde::Serialize for FileId {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&format!("{}", self.0))
    }
}

#[derive(Debug, Default, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SymbolInstanceId(i32);

impl SymbolInstanceId {
    pub fn invalid() -> Self {
        Self(-1)
    }

    pub fn new(id: i32) -> Self {
        Self(id)
    }
}

impl From<i64> for SymbolInstanceId {
    fn from(value: i64) -> Self {
        Self(value as i32)
    }
}

impl From<Option<i64>> for SymbolInstanceId {
    fn from(value: Option<i64>) -> Self {
        Self(value.unwrap() as i32)
    }
}

impl Into<i32> for SymbolInstanceId {
    fn into(self) -> i32 {
        self.0
    }
}

impl fmt::Display for SymbolInstanceId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<i32> for SymbolInstanceId {
    fn from(value: i32) -> Self {
        Self(value)
    }
}

impl serde::Serialize for SymbolInstanceId {
    fn serialize<S>(&self, s: S) -> Result<S::Ok, S::Error>
    where
        S: Serializer,
    {
        s.serialize_str(&format!("{}", self.0))
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize)]
#[repr(i32)]
pub enum SymbolType {
    Function = 1,
    File = 2,
    Module = 3,
    Directory = 4,
    Type = 5,
    Data = 6,
    Macro = 7,
    Field = 8,
}

#[derive(Debug, PartialEq, Eq, Copy, Clone, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
#[repr(i32)]
pub enum InstanceType {
    Definition = 1,
    Declaration = 2,
    Expansion = 3,
    Sentinel = 4,
    Containment = 5,
    Source = 6,
    Header = 7,
    Build = 8,
    File = 9,
    Documentation = 10,
}

impl InstanceType {
    pub fn as_i64(&self) -> i64 {
        return *self as i64;
    }
}

impl From<i64> for InstanceType {
    fn from(value: i64) -> Self {
        match value {
            x if x == InstanceType::Definition as i64 => InstanceType::Definition,
            x if x == InstanceType::Declaration as i64 => InstanceType::Declaration,
            x if x == InstanceType::Expansion as i64 => InstanceType::Expansion,
            x if x == InstanceType::Sentinel as i64 => InstanceType::Sentinel,
            x if x == InstanceType::Containment as i64 => InstanceType::Containment,
            x if x == InstanceType::Source as i64 => InstanceType::Source,
            x if x == InstanceType::Header as i64 => InstanceType::Header,
            x if x == InstanceType::Build as i64 => InstanceType::Build,
            x if x == InstanceType::File as i64 => InstanceType::File,
            x if x == InstanceType::Documentation as i64 => InstanceType::Documentation,
            _ => panic!("Invalid instance type value {}", value),
        }
    }
}

impl From<i32> for InstanceType {
    fn from(value: i32) -> Self {
        match value {
            x if x == InstanceType::Definition as i32 => InstanceType::Definition,
            x if x == InstanceType::Declaration as i32 => InstanceType::Declaration,
            x if x == InstanceType::Expansion as i32 => InstanceType::Expansion,
            x if x == InstanceType::Sentinel as i32 => InstanceType::Sentinel,
            x if x == InstanceType::Containment as i32 => InstanceType::Containment,
            x if x == InstanceType::Source as i32 => InstanceType::Source,
            x if x == InstanceType::Header as i32 => InstanceType::Header,
            x if x == InstanceType::Build as i32 => InstanceType::Build,
            x if x == InstanceType::File as i32 => InstanceType::File,
            x if x == InstanceType::Documentation as i32 => InstanceType::Documentation,
            _ => panic!("Invalid instance type value {}", value),
        }
    }
}

impl SymbolType {
    pub fn as_i64(&self) -> i64 {
        return *self as i64;
    }
}

impl From<i64> for SymbolType {
    fn from(value: i64) -> Self {
        match value {
            x if x == SymbolType::Function as i64 => SymbolType::Function,
            x if x == SymbolType::File as i64 => SymbolType::File,
            x if x == SymbolType::Module as i64 => SymbolType::Module,
            x if x == SymbolType::Directory as i64 => SymbolType::Directory,
            x if x == SymbolType::Type as i64 => SymbolType::Type,
            x if x == SymbolType::Data as i64 => SymbolType::Data,
            x if x == SymbolType::Macro as i64 => SymbolType::Macro,
            x if x == SymbolType::Field as i64 => SymbolType::Field,
            _ => panic!("Invalid symbol type value {}", value),
        }
    }
}

impl From<i32> for SymbolType {
    fn from(value: i32) -> Self {
        match value {
            x if x == SymbolType::Function as i32 => SymbolType::Function,
            x if x == SymbolType::File as i32 => SymbolType::File,
            x if x == SymbolType::Module as i32 => SymbolType::Module,
            x if x == SymbolType::Directory as i32 => SymbolType::Directory,
            x if x == SymbolType::Type as i32 => SymbolType::Type,
            x if x == SymbolType::Data as i32 => SymbolType::Data,
            x if x == SymbolType::Macro as i32 => SymbolType::Macro,
            x if x == SymbolType::Field as i32 => SymbolType::Field,
            _ => panic!("Invalid symbol type value {}", value),
        }
    }
}

#[derive(Debug, PartialEq, Eq, Copy, Clone)]
#[repr(i32)]
pub enum SymbolScope {
    Local = 1,
    Global = 2,
}

impl SymbolScope {
    pub fn as_i64(&self) -> i64 {
        return *self as i64;
    }
}

impl From<i64> for SymbolScope {
    fn from(value: i64) -> Self {
        match value {
            x if x == SymbolScope::Local as i64 => SymbolScope::Local,
            x if x == SymbolScope::Global as i64 => SymbolScope::Global,
            _ => panic!("Invalid symbol scope value"),
        }
    }
}

impl From<i32> for SymbolScope {
    fn from(value: i32) -> Self {
        match value {
            x if x == SymbolScope::Local as i32 => SymbolScope::Local,
            x if x == SymbolScope::Global as i32 => SymbolScope::Global,
            _ => panic!("Invalid symbol scope value"),
        }
    }
}

type SymbolMatcher<'a> = Box<dyn Fn((&'a SymbolId, &'a Symbol)) -> Option<&'a Symbol> + 'a>;

pub fn exact_name_match<'a>(name: &'a str) -> SymbolMatcher<'a> {
    Box::new(|(_, s): (&'a SymbolId, &'a Symbol)| if s.name == *name { Some(s) } else { None })
}

/// Removes specified characters from a string, splits it at periods,
/// and filters out empty strings.
///
/// # Arguments
///
/// * `input` - The input string to process
///
/// # Returns
///
/// A vector of strings, split at periods with unwanted characters removed
pub fn clean_and_split_string(input: &str) -> Vec<String> {
    // Characters to remove: */[]{}:,@- and space
    let chars_to_remove = ['*', '[', ']', '{', '}', ',', '@', '-', ' ', '(', ')'];

    // Remove unwanted characters
    let cleaned = input
        .chars()
        .filter(|&c| !chars_to_remove.contains(&c))
        .collect::<String>();

    // Split at periods and filter out empty strings
    cleaned
        .split(['.', '/', ':'])
        .filter(|s| !s.is_empty())
        .map(String::from)
        .collect()
}

pub fn normalize_symbol_tokens(input: &str) -> Vec<String> {
    clean_and_split_string(input)
        .into_iter()
        .filter_map(|token| {
            let cleaned: String = token
                .chars()
                .filter(|c| c.is_ascii_alphanumeric() || *c == '_')
                .collect();
            if cleaned.is_empty() {
                None
            } else {
                Some(cleaned)
            }
        })
        .collect()
}

pub fn symbol_name_to_path(input: &str) -> String {
    let tokens = normalize_symbol_tokens(input);
    if tokens.is_empty() {
        "unknown".to_string()
    } else {
        tokens.join(".")
    }
}

pub fn symbol_query_to_lquery(input: &str) -> Option<String> {
    build_lquery(input, false, true)
}

/// Like `symbol_query_to_lquery` but anchors the last token to the end of the path.
/// This means the last query token must be the last component of the symbol path.
///
/// Examples:
/// - `"kueue"` → `"*.kueue"`
/// - `"pkg/kueue"` → `"*.pkg.*.kueue"`
pub fn symbol_query_to_leaf_lquery(input: &str) -> Option<String> {
    build_lquery(input, true, true)
}

/// Build an lquery pattern from a search string.
///
/// * `anchor_leaf` – when true the last token is anchored to the end of the path
/// * `dot_is_separator` – when true dots split into separate ltree labels (functions, modules);
///   when false dots are replaced with `_` (files, directories) matching the DB trigger behavior
///
/// Examples (`dot_is_separator=true`):
/// - `"pkg/kueue"` → `"*.pkg.*.kueue.*"` (anchor_leaf=false)
/// - `"pkg/kueue"` → `"*.pkg.*.kueue"` (anchor_leaf=true)
///
/// Examples (`dot_is_separator=false`):
/// - `"log.h"` → `"*.log_h.*"`
/// - `"src/main.go"` → `"*.src.*.main_go.*"`
pub(crate) fn build_lquery(
    input: &str,
    anchor_leaf: bool,
    dot_is_separator: bool,
) -> Option<String> {
    let input: std::borrow::Cow<str> = if dot_is_separator {
        std::borrow::Cow::Borrowed(input)
    } else {
        std::borrow::Cow::Owned(input.replace('.', "_"))
    };
    let tokens = normalize_symbol_tokens(&input);
    if tokens.is_empty() {
        return None;
    }

    let mut parts: Vec<String> = Vec::with_capacity(tokens.len() * 2 + 1);
    parts.push("*".to_string());
    for (i, token) in tokens.iter().enumerate() {
        parts.push(token.clone());
        if !anchor_leaf || i < tokens.len() - 1 {
            parts.push("*".to_string());
        }
    }
    Some(parts.join("."))
}

/// Checks if `subset` is an ordered subset of `superset`.
///
/// An ordered subset means that the elements in `subset` appear in the same order
/// in `superset`, though not necessarily consecutively.
///
/// # Arguments
///
/// * `superset` - The sequence that might contain the subset
/// * `subset` - The sequence that might be an ordered subset
///
/// # Returns
/// `true` if `subset` is an ordered subset of `superset`, `false` otherwise
pub fn is_ordered_subset<T: PartialEq>(superset: &[T], subset: &[T]) -> bool {
    // Empty subset is always an ordered subset
    if subset.is_empty() {
        return true;
    }

    // Cannot be a subset if longer than the superset
    if subset.len() > superset.len() {
        return false;
    }

    let mut subset_idx = 0;
    let mut superset_idx = 0;

    // Traverse both sequences
    while subset_idx < subset.len() && superset_idx < superset.len() {
        if subset[subset_idx] == superset[superset_idx] {
            // Found a match, move to the next element in subset
            subset_idx += 1;
        }
        // Always move to the next element in superset
        superset_idx += 1;
    }

    // If we've gone through all elements in subset, it's an ordered subset
    subset_idx == subset.len()
}

/// Checks if a symbol partially matches the searched pattern
///
/// The symbol and the pattern can consist of multiple parts separated by '.' or
/// '/'. We consider the symbol matches the pattern of components of the pattern
/// are an ordered subset of the components of the symbol
///
/// # Arguments
///
/// * `name` - Search pattern
///
/// # Returns
///
/// Symbol matcher that checks if a symbol matches the pattern
pub fn partial_name_match<'a>(name: &'a str) -> SymbolMatcher<'a> {
    let search_pattern = clean_and_split_string(name);
    Box::new(move |(_, s): (&'a SymbolId, &'a Symbol)| {
        if is_ordered_subset(&s.name_split, &search_pattern) {
            Some(s)
        } else {
            None
        }
    })
}

/// Checks if a symbol matches the package name
///
/// The package name consists of multiple parts separated by '.' or '/'. We
/// consider the symbol matches the pattern of all components of the pattern
/// match the beginning component of the symbol, except for the last component
/// of the symbol which is the symbol name itself.
///
/// # Arguments
///
/// * `name` - Search pattern
///
/// # Returns
///
/// Symbol matcher that checks if a symbol matches the pattern
pub fn package_match<'a>(name: &'a str) -> SymbolMatcher<'a> {
    let search_pattern = clean_and_split_string(name);

    Box::new(move |(_, s): (&'a SymbolId, &'a Symbol)| {
        // Handle empty name_split (e.g., "/" directory)
        if s.name_split.is_empty() {
            return None;
        }
        for i in 0..search_pattern.len() {
            // Check bounds: we need at least i+1 elements beyond the match
            if s.name_split.len() <= i + 1 {
                return None;
            }
            if s.name_split[i] != search_pattern[i] {
                return None;
            }
        }
        Some(s)
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_symbol_query_to_leaf_lquery_single_token() {
        assert_eq!(
            symbol_query_to_leaf_lquery("kueue"),
            Some("*.kueue".to_string())
        );
    }

    #[test]
    fn test_symbol_query_to_leaf_lquery_two_tokens() {
        assert_eq!(
            symbol_query_to_leaf_lquery("pkg/kueue"),
            Some("*.pkg.*.kueue".to_string())
        );
    }

    #[test]
    fn test_symbol_query_to_leaf_lquery_three_tokens() {
        assert_eq!(
            symbol_query_to_leaf_lquery("a/b/c"),
            Some("*.a.*.b.*.c".to_string())
        );
    }

    #[test]
    fn test_symbol_query_to_leaf_lquery_empty() {
        assert_eq!(symbol_query_to_leaf_lquery(""), None);
    }

    #[test]
    fn test_lquery_no_dot_sep_simple() {
        // "log.h" with dot_is_separator=false → dot becomes underscore
        assert_eq!(
            build_lquery("log.h", false, false),
            Some("*.log_h.*".to_string())
        );
    }

    #[test]
    fn test_lquery_no_dot_sep_with_path() {
        // "src/main.go" → slash still splits, dot becomes underscore
        assert_eq!(
            build_lquery("src/main.go", false, false),
            Some("*.src.*.main_go.*".to_string())
        );
    }

    #[test]
    fn test_leaf_lquery_no_dot_sep() {
        assert_eq!(
            build_lquery("log.h", true, false),
            Some("*.log_h".to_string())
        );
    }
}
