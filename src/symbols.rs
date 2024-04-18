use clang_ast::SourceRange;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::fmt::{Display, self};
use std::fs;
use std::path::PathBuf;
use std::{collections::HashMap, hash, hash::Hasher};

#[derive(Debug, PartialEq, Eq, PartialOrd, Hash, Ord, Copy, Clone, Serialize, Deserialize)]
pub struct FileHash(u64);

impl FileHash {
    pub fn new<T: hash::Hash>(url: &T) -> Self {
        let mut s = DefaultHasher::new();
        url.hash(&mut s);
        FileHash(s.finish())
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Occurence {
    pub line_start: i32,
    pub line_end: i32,
    pub column_start: i32,
    pub column_end: i32,
    pub file: PathBuf,
}

impl Occurence {
    pub fn new(file: PathBuf, range: SourceRange) -> Self {
        Self {
            line_start: range.begin.spelling_loc.as_ref().unwrap().line as i32,
            column_start: range.begin.spelling_loc.unwrap().col as i32,
            line_end: range.end.spelling_loc.as_ref().unwrap().line as i32,
            column_end: range.end.spelling_loc.unwrap().col as i32,
            file: fs::canonicalize(file).unwrap(),
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SymbolChild {
    pub symbol_id: SymbolId,
    pub occurence: Option<Occurence>,
}

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Symbol {
    pub name: String,
    pub ranges: Vec<Occurence>,
    pub children: Vec<SymbolChild>,
}

pub trait Symbols: ToString {
    fn add(&mut self, id: SymbolId, symbol: Symbol);
    fn into_vec(&self) -> Vec<SymbolId>;
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SymbolId(pub String);

impl SymbolId {
    pub fn new(id: String) -> Self {
        Self(id)
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SymbolMap {
    pub map: HashMap<SymbolId, Symbol>,
}

impl SymbolMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }

    pub fn merge(mut self, other: SymbolMap) -> Self {
        other.map.into_iter().for_each(|(key, value)| {
            self.map
                .entry(key)
                .and_modify(|cur_symbol| cur_symbol.children.extend(value.children.clone()))
                .or_insert(value);
        });
        self
    }

    pub fn iter<'a>(&'a self) -> impl Iterator<Item = (&SymbolId, &Symbol)> + 'a {
        self.map.iter()
    }

    pub fn get_children(&self, symbol_id: &SymbolId) -> Vec<SymbolChild> {
        let symbol = if let Some(symbol) = self.map.get(&symbol_id) {
            symbol
        } else {
            return vec![];
        };

        symbol.children.clone().into_iter().collect::<Vec<_>>()
    }
}

impl Symbols for SymbolMap {
    fn add(&mut self, id: SymbolId, mut symbol: Symbol) {
        if let Some(existing) = self.map.get_mut(&id) {
            assert_eq!(existing.name, symbol.name);
            existing.ranges.append(&mut symbol.ranges);
            existing.children.extend(symbol.children);
        } else {
            self.map.insert(id, symbol);
        }
    }

    fn into_vec(&self) -> Vec<SymbolId> {
        self.map.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>()
    }
}

impl ToString for SymbolMap {
    fn to_string(&self) -> String {
        serde_json::to_string_pretty(&self.map.clone().into_values().collect::<Vec<Symbol>>())
            .unwrap()
    }
}
