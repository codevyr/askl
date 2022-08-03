use std::{collections::HashMap, hash};

use log::debug;
use serde::Serialize;

#[derive(Debug, PartialEq, Eq, Clone, Serialize)]
pub struct Location {
    pub file: lsp_types::Url,
    pub position: lsp_types::Position,
}

/// URLs hash like their serialization.
impl hash::Hash for Location {
    #[inline]
    fn hash<H>(&self, state: &mut H)
    where
        H: hash::Hasher,
    {
        hash::Hash::hash(&self.file, state);
        hash::Hash::hash(&self.position.character, state);
        hash::Hash::hash(&self.position.line, state);
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Range {
    range: lsp_types::Range,
}

impl From<lsp_types::Range> for Range {
    fn from(r: lsp_types::Range) -> Self {
        Self { range: r }
    }
}

impl Range {
    fn contains(&self, pos: lsp_types::Position) -> bool {
        if (pos.line < self.range.start.line)
            || (self.range.start.line == pos.line && pos.character <= self.range.start.character)
        {
            false
        } else if (self.range.end.line < pos.line)
            || (self.range.end.line == pos.line && self.range.end.character < pos.character)
        {
            false
        } else {
            true
        }
    }
}

#[derive(Debug, Serialize, Clone)]
pub struct Symbol {
    pub path: lsp_types::Url,
    pub name: String,
    pub detail: Option<String>,
    pub kind: lsp_types::SymbolKind,
    pub range: Range,
    pub selection_range: Range,
    pub parents: Vec<Location>,
}

pub trait Symbols: ToString {
    fn add(&mut self, loc: Location, symbol: Symbol);
    fn into_vec(&self) -> Vec<Location>;
    fn find(&self, loc: &Location) -> Option<Location>;
    fn add_parent(&mut self, child: &Location, parent: &Location);
}

#[derive(Debug, Serialize)]
pub struct SymbolMap {
    map: HashMap<Location, Symbol>,
}

impl SymbolMap {
    pub fn new() -> Self {
        Self {
            map: HashMap::new(),
        }
    }
}

impl Symbols for SymbolMap {
    fn add(&mut self, loc: Location, symbol: Symbol) {
        let prev = self.map.insert(loc.clone(), symbol);
        if prev.is_some() {
            panic!("Location duplicate: {:?}", loc);
        }
    }

    fn into_vec(&self) -> Vec<Location> {
        self.map.iter().map(|(k, _)| k.clone()).collect::<Vec<_>>()
    }

    fn find(&self, loc: &Location) -> Option<Location> {
        self.map
            .iter()
            .find(|(k, v)| k.file == loc.file && (v.range.contains(loc.position)))
            .map(|(k, _)| k)
            .cloned()
    }

    fn add_parent(&mut self, child: &Location, parent: &Location) {
        let symbol = self.map.get_mut(child).unwrap();
        symbol.parents.push(parent.clone());
        debug!(
            "add_parent: {:#?} {:#?} {:#?}",
            child, parent, symbol.parents
        );
    }
}

impl ToString for SymbolMap {
    fn to_string(&self) -> String {
        serde_json::to_string_pretty(&self.map.clone().into_values().collect::<Vec<Symbol>>())
            .unwrap()
    }
}
