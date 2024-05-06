use clang_ast::SourceRange;
use serde::Serializer;
use serde::{Deserialize, Serialize};
use std::collections::hash_map::DefaultHasher;
use std::collections::HashSet;
use std::fmt;
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

#[derive(Debug, Serialize, Deserialize, Clone, Eq, PartialEq, Hash, PartialOrd, Ord)]
pub struct SymbolChild {
    pub id: SymbolId,
    pub occurrence: Option<Occurrence>,
}

pub type SymbolRefs = HashMap<SymbolId, HashSet<Occurrence>>;

#[derive(Debug, Serialize, Deserialize, Clone)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub ranges: HashSet<Occurrence>,
    pub children: SymbolRefs,
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
}

pub trait Symbols: ToString {
    fn add(&mut self, id: SymbolId, symbol: Symbol);
    fn into_vec(&self) -> Vec<SymbolId>;
}

#[derive(Debug, Serialize, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, sqlx::Type, sqlx::FromRow)]
#[sqlx(transparent)]
pub struct SymbolId(pub i64);

impl SymbolId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }
}

impl fmt::Display for SymbolId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.0)
    }
}

impl From<Option<i64>> for SymbolId {
    fn from(value: Option<i64>) -> Self {
        Self(value.unwrap())
    }
}

#[derive(Debug, Deserialize, Copy, Clone, Eq, PartialEq, Hash, PartialOrd, Ord, sqlx::Type)]
#[sqlx(transparent)]
pub struct FileId(i64);

impl FileId {
    pub fn new(id: i64) -> Self {
        Self(id)
    }
}

impl From<i64> for FileId {
    fn from(value: i64) -> Self {
        Self(value)
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

#[derive(sqlx::Type, Debug)]
#[repr(i32)]
pub enum SymbolType {
    Definition = 1,
    Declaration = 2,
}

impl From<i64> for SymbolType {
    fn from(value: i64) -> Self {
        match value {
            x if x == SymbolType::Definition as i64 => SymbolType::Definition,
            x if x == SymbolType::Declaration as i64 => SymbolType::Declaration,
            _ => panic!("Invalid symbol type value")
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone)]
pub struct SymbolMap {
    pub symbols: HashMap<SymbolId, Symbol>,
    pub files: HashMap<FileId, String>,
}

impl SymbolMap {
    pub fn new() -> Self {
        Self {
            symbols: HashMap::new(),
            files: HashMap::new(),
        }
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

    pub fn get_children(&self, symbol_id: &SymbolId) -> &SymbolRefs {
        let symbol = if let Some(symbol) = self.symbols.get(&symbol_id) {
            symbol
        } else {
            panic!("Unknown symbol");
        };

        &symbol.children
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
            .find_map(|(id, f)| if **f == file { Some(*id) } else { None })
    }

    pub fn set_file_id(&mut self, id: FileId, file: String) {
        self.files.insert(id, file);
    }
}

impl Symbols for SymbolMap {
    fn add(&mut self, id: SymbolId, symbol: Symbol) {
        if let Some(existing) = self.symbols.get_mut(&id) {
            assert_eq!(existing.name, symbol.name);
            existing.ranges.extend(symbol.ranges);
            existing.children.extend(symbol.children);
        } else {
            self.symbols.insert(id, symbol);
        }
    }

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
