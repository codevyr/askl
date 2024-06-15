use std::{collections::HashSet, iter::Iterator};

use index::db::{Declaration, File, Index};
use index::symbols::{DeclarationId, DeclarationRefs};
use index::symbols::{FileId, Occurrence, Symbol, SymbolId, SymbolMap};

pub struct ControlFlowGraph {
    pub symbols: SymbolMap,
    pub index: Index,
    pub nodes: HashSet<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub HashSet<DeclarationId>);

impl NodeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add(&mut self, node: DeclarationId) {
        self.0.insert(node);
    }

    pub fn as_vec(&self) -> Vec<DeclarationId> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort();
        res
    }
}

#[derive(Debug, Clone)]
pub struct EdgeList(pub HashSet<(DeclarationId, DeclarationId, Option<Occurrence>)>);

impl EdgeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add_reference(&mut self, from: DeclarationId, to: DeclarationId, occurrence: Option<Occurrence>) {
        self.0
            .insert((from, to, occurrence));
    }

    pub fn as_vec(&self) -> Vec<(DeclarationId, DeclarationId, Option<Occurrence>)> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort();
        res
    }
}

impl ControlFlowGraph {
    pub fn from_symbols(symbols: SymbolMap, index: Index) -> Self {
        let nodes: HashSet<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();
        Self {
            symbols,
            index,
            nodes,
        }
    }

    pub fn iter_symbols(&self) -> impl Iterator<Item = (&SymbolId, &Symbol)> {
        self.symbols.iter()
    }

    pub fn get_symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.symbols.get(&id)
    }

    pub fn get_symbol_by_name(&self, name: &str) -> Vec<&Symbol> {
        self.symbols
            .symbols
            .iter()
            .filter_map(|(_, v)| if v.name == *name { Some(v) } else { None })
            .collect()
    }

    pub fn get_file(&self, id: FileId) -> Option<&File> {
        self.symbols.files.get(&id)
    }

    pub fn get_declaration(&self, id: DeclarationId) -> Option<&Declaration> {
        self.symbols.declarations.get(&id)
    }

    pub fn get_declarations_by_name(&self, name: &str) -> DeclarationRefs {
        let mut res = DeclarationRefs::new();
        let symbols = self.get_symbol_by_name(name);
        if symbols.len() == 0 {
            return res;
        }

        for symbol in symbols {
            for (declaration_id, declaration) in self.symbols.declarations.iter() {
                if declaration.symbol == symbol.id {
                    res.insert(*declaration_id, HashSet::new());
                }
            }
        }

        res
    }
}
