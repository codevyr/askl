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

use crate::db::{File, Index, self};

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
pub struct Occurrence {
    pub line_start: i32,
    pub line_end: i32,
    pub column_start: i32,
    pub column_end: i32,
    pub file: FileId,
}

impl Occurrence {
    pub fn new(range: &Option<SourceRange>, file_id: FileId) -> Option<Self> {
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

        let end = if let Some(end) = &range.end.expansion_loc {
            end
        } else {
            return None;
        };

        // let file = begin.file.clone().to_string();
        // fs::canonicalize(file.clone())
        // .or::<PathBuf>(Ok(PathBuf::from(file)))
        // .unwrap()

        Some(Self {
            line_start: begin.line as i32,
            column_start: begin.col as i32,
            line_end: end.line as i32,
            column_end: end.col as i32,
            file: file_id,
        })
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

impl From<db::Declaration> for Occurrence {
    fn from(symbol: db::Declaration) -> Self {
        Occurrence {
            line_start: symbol.line_start as i32,
            line_end: symbol.line_end as i32,
            column_start: symbol.col_start as i32,
            column_end: symbol.col_end as i32,
            file: symbol.file_id,
        }
    }
}

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct Reference {
    pub from: SymbolId,
    pub to: SymbolId,
    pub occurrence: Option<Occurrence>,
}

impl Reference {
    pub fn new(from: SymbolId, to: SymbolId) -> Self {
        Self {
            from,
            to,
            occurrence: None,
        }
    }

    pub fn new_occurrence(from: SymbolId, to: SymbolId, occurrence: Occurrence) -> Self {
        Self {
            from,
            to,
            occurrence: Some(occurrence),
        }
    }
}

pub type SymbolRefs = HashMap<SymbolId, HashSet<Occurrence>>;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub occurrence: Occurrence,
    pub children: SymbolRefs,
    pub parents: SymbolRefs,
}

impl Symbol {
    pub fn add_child(&mut self, id: SymbolId, occurrence: Occurrence) {
        self.children
            .entry(id)
            .and_modify(|occurences| {
                occurences.insert(occurrence.clone());
            })
            .or_insert(HashSet::from([occurrence]));
    }

    pub fn add_parent(&mut self, id: SymbolId, occurrence: Occurrence) {
        self.parents
            .entry(id)
            .and_modify(|occurences| {
                occurences.insert(occurrence.clone());
            })
            .or_insert(HashSet::from([occurrence]));
    }
}

pub trait Symbols: ToString {
    fn into_vec(&self) -> Vec<SymbolId>;
}

#[derive(
    Debug,
    Default,
    Serialize,
    Deserialize,
    Copy,
    Clone,
    Eq,
    PartialEq,
    Hash,
    PartialOrd,
    Ord,
    sqlx::Type,
    sqlx::FromRow,
)]
#[sqlx(transparent)]
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

#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, sqlx::Type)]
#[sqlx(transparent)]
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

#[derive(sqlx::Type, Debug, PartialEq, Eq, Copy, Clone)]
#[repr(i32)]
pub enum SymbolType {
    Definition = 1,
    Declaration = 2,
}

impl SymbolType {
    pub fn as_i64(&self) -> i64 {
        return *self as i64;
    }
}

impl From<i64> for SymbolType {
    fn from(value: i64) -> Self {
        match value {
            x if x == SymbolType::Definition as i64 => SymbolType::Definition,
            x if x == SymbolType::Declaration as i64 => SymbolType::Declaration,
            _ => panic!("Invalid symbol type value"),
        }
    }
}

#[derive(sqlx::Type, Debug, PartialEq, Eq, Copy, Clone)]
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

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SymbolMap {
    pub symbols: HashMap<SymbolId, Symbol>,
    pub files: HashMap<FileId, File>,
}

impl SymbolMap {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
            files: HashMap::new(),
        }
    }

    pub async fn from_index(index: Index) -> Result<Self> {
        let symbols = index.all_declarations().await?;
        unimplemented!("XXX");
        let mut symbols_map = HashMap::new();
        for symbol in symbols {
            symbols_map.insert(
                symbol.symbol,
                Symbol {
                    id: symbol.symbol,
                    children: SymbolRefs::new(),
                    parents: SymbolRefs::new(),
                    // name: symbol.name.clone(),
                    name: String::from(""),
                    occurrence: symbol.into(),
                },
            );
        }

        let files = index.all_files().await?;
        let mut files_map = HashMap::new();
        for file in files {
            files_map.insert(file.id, file);
        }

        let references = index.all_refs().await?;
        for reference in references {
            let from_symbol = symbols_map.get_mut(&reference.from_symbol).unwrap();
            let occurrence = Occurrence {
                file: from_symbol.occurrence.file,
                line_start: reference.from_line as i32,
                line_end: reference.from_line as i32,
                column_start: reference.from_col_start as i32,
                column_end: reference.from_col_end as i32,
            };
            from_symbol.add_child(reference.to_symbol, occurrence.clone());

            let to_symbol = symbols_map.get_mut(&reference.to_symbol).unwrap();
            to_symbol.add_parent(reference.from_symbol, occurrence);
        }

        Ok(Self {
            symbols: symbols_map,
            files: files_map,
        })
    }

    pub fn merge(mut self, other: SymbolMap) -> Self {
        other.symbols.into_iter().for_each(|(key, value)| {
            self.symbols
                .entry(key)
                .and_modify(|cur_symbol| cur_symbol.children.extend(value.children.clone()))
                .or_insert(value);
        });
        self
    }

    pub fn iter<'a>(&'a self) -> impl Iterator<Item = (&SymbolId, &Symbol)> + 'a {
        self.symbols.iter()
    }

    pub fn get_children(&self, symbol_id: SymbolId) -> &SymbolRefs {
        let symbol = if let Some(symbol) = self.symbols.get(&symbol_id) {
            symbol
        } else {
            panic!("Unknown symbol");
        };

        &symbol.children
    }

    pub fn get_parents(&self, symbol_id: SymbolId) -> &SymbolRefs {
        let symbol = if let Some(symbol) = self.symbols.get(&symbol_id) {
            symbol
        } else {
            panic!("Unknown symbol");
        };

        &symbol.parents
    }

    pub fn find(&self, symbol_name: &str) -> Option<&Symbol> {
        self.symbols
            .iter()
            .find_map(|(_, s)| if s.name == symbol_name { Some(s) } else { None })
    }

    pub fn find_mut(&mut self, symbol_name: &str) -> Option<&mut Symbol> {
        self.symbols
            .iter_mut()
            .find_map(|(_, s)| if s.name == symbol_name { Some(s) } else { None })
    }

    pub fn get_mut(&mut self, symbol_id: &SymbolId) -> Option<&mut Symbol> {
        self.symbols.get_mut(symbol_id)
    }

    pub fn get_file_id(&self, file: String) -> Option<FileId> {
        self.files
            .iter()
            .find_map(|(id, f)| if f.path == file { Some(*id) } else { None })
    }

    pub fn set_file_id(&mut self, id: FileId, file: File) {
        self.files.insert(id, file);
    }
}

impl Symbols for SymbolMap {
    fn into_vec(&self) -> Vec<SymbolId> {
        self.symbols
            .iter()
            .map(|(k, _)| k.clone())
            .collect::<Vec<_>>()
    }
}

impl ToString for SymbolMap {
    fn to_string(&self) -> String {
        serde_json::to_string_pretty(&self.symbols.clone().into_values().collect::<Vec<Symbol>>())
            .unwrap()
    }
}
