use std::{collections::HashSet, iter::Iterator};

use anyhow::Result;
use index::db::{File, Module};
use index::db_diesel::{Index, Selection, SelectionNode};
use index::symbols::{DeclarationId, DeclarationRefs, ModuleId};
use index::symbols::{FileId, Occurrence, Symbol, SymbolId, SymbolMap};

pub struct ControlFlowGraph {
    pub symbols: SymbolMap,
    pub index: Index,
    pub nodes: HashSet<SymbolId>,
}

#[derive(Debug, Clone)]
pub struct NodeList(pub HashSet<SelectionNode>);

impl NodeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add(&mut self, node: SelectionNode) {
        self.0.insert(node);
    }

    pub fn as_vec(&self) -> Vec<DeclarationId> {
        let mut res: Vec<_> = self
            .0
            .iter()
            .map(|n| DeclarationId::new(n.declaration.id))
            .collect();
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

    pub fn add_reference(
        &mut self,
        from: DeclarationId,
        to: DeclarationId,
        occurrence: Option<Occurrence>,
    ) {
        self.0.insert((from, to, occurrence));
    }

    pub fn as_vec(&self) -> Vec<(DeclarationId, DeclarationId, Option<Occurrence>)> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort();
        res
    }
}

impl ControlFlowGraph {
    pub fn from_symbols(symbols: SymbolMap, index_diesel: Index) -> Self {
        let nodes: HashSet<SymbolId> = symbols.iter().map(|(id, _)| id.clone()).collect();
        Self {
            symbols,
            index: index_diesel,
            nodes,
        }
    }

    pub fn iter_symbols(&self) -> impl Iterator<Item = (&SymbolId, &Symbol)> {
        self.symbols.iter()
    }

    pub fn get_symbol(&self, id: SymbolId) -> Option<&Symbol> {
        self.symbols.symbols.get(&id)
    }

    pub fn get_file(&self, id: FileId) -> Option<&File> {
        self.symbols.files.get(&id)
    }

    pub fn get_module(&self, id: ModuleId) -> Option<&Module> {
        self.symbols.modules.get(&id)
    }

    pub fn find_module(&self, name: &str) -> Option<&Module> {
        for (_, module) in self.symbols.modules.iter() {
            if module.module_name == name {
                return Some(&module);
            }
        }

        None
    }

    pub fn get_declarations_from_symbols(&self, symbols: &Vec<SymbolId>) -> DeclarationRefs {
        let mut res = DeclarationRefs::new();
        if symbols.len() == 0 {
            return res;
        }

        for symbol in symbols {
            for (declaration_id, declaration) in self.symbols.declarations.iter() {
                if declaration.symbol == *symbol {
                    res.insert(*declaration_id, HashSet::new());
                }
            }
        }

        res
    }

    pub async fn find_symbol_by_name(&self, name: &str) -> Result<Selection> {
        self.index
            .find_symbol_by_name(&name)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find symbol by name: {}", e))
    }
}
