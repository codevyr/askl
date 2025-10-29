use std::{collections::HashSet, iter::Iterator};

use anyhow::Result;
use index::db_diesel::{Index, Selection, SelectionNode};
use index::symbols::Occurrence;
use index::symbols::{DeclarationId, SymbolId};

pub struct ControlFlowGraph {
    pub index: Index,
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

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolDeclId {
    pub symbol_id: SymbolId,
    pub declaration_id: DeclarationId,
}

#[derive(Debug, Clone)]
pub struct EdgeList(pub HashSet<(SymbolDeclId, SymbolDeclId, Option<Occurrence>)>);

impl EdgeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add_reference(
        &mut self,
        from: SymbolDeclId,
        to: SymbolDeclId,
        occurrence: Option<Occurrence>,
    ) {
        self.0.insert((from, to, occurrence));
    }

    pub fn as_vec(&self) -> Vec<(SymbolDeclId, SymbolDeclId, Option<Occurrence>)> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort_by(|(from_a, to_a, _), (from_b, to_b, _)| {
            from_a
                .declaration_id
                .cmp(&from_b.declaration_id)
                .then_with(|| to_a.declaration_id.cmp(&to_b.declaration_id))
                .then_with(|| from_a.symbol_id.cmp(&from_b.symbol_id))
                .then_with(|| to_a.symbol_id.cmp(&to_b.symbol_id))
        });
        res
    }
}

impl ControlFlowGraph {
    pub fn from_symbols(index_diesel: Index) -> Self {
        Self {
            index: index_diesel,
        }
    }

    pub async fn find_symbol_by_name(&self, name: &str) -> Result<Selection> {
        self.index
            .find_symbol_by_name(&name)
            .await
            .map_err(|e| anyhow::anyhow!("Failed to find symbol by name: {}", e))
    }
}
