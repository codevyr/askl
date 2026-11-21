use std::{
    collections::{HashMap, HashSet},
    iter::Iterator,
};

use index::db_diesel::{Index, SelectionNode};
use index::roles::Stated;
use index::symbols::Occurrence;
use index::symbols::{SymbolId, SymbolInstanceId};

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

    pub fn as_vec(&self) -> Vec<SymbolInstanceId> {
        let mut res: Vec<_> = self
            .0
            .iter()
            .map(|n| SymbolInstanceId::new(n.symbol_instance.id))
            .collect();
        res.sort();
        res
    }
}

#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub struct SymbolNodeId {
    pub symbol_id: SymbolId,
    pub instance_id: SymbolInstanceId,
}

/// The reference edges of an answer: one entry per occurrence, and beside
/// them what the references of each symbol PAIR state about it.
///
/// What is stated is kept per pair because that is what an answer's edge is,
/// and because it is a union: every reference between the two symbols adds
/// to it, including one whose occurrence was already drawn from another
/// source.
#[derive(Debug, Clone)]
pub struct EdgeList(
    pub HashSet<(SymbolNodeId, SymbolNodeId, Option<Occurrence>)>,
    HashMap<(SymbolId, SymbolId), Stated>,
);

impl EdgeList {
    pub fn new() -> Self {
        Self(HashSet::new(), HashMap::new())
    }

    /// One more reference from `from` to `to` says this.
    pub fn state(&mut self, from: SymbolId, to: SymbolId, said: Stated) {
        self.1.entry((from, to)).or_default().add(said);
    }

    /// What the references from `from` to `to` state, taken together.
    pub fn stated(&self, from: SymbolId, to: SymbolId) -> Stated {
        self.1.get(&(from, to)).copied().unwrap_or_default()
    }

    pub fn add_reference(
        &mut self,
        from: SymbolNodeId,
        to: SymbolNodeId,
        occurrence: Option<Occurrence>,
    ) {
        self.0.insert((from, to, occurrence));
    }

    pub fn as_vec(&self) -> Vec<(SymbolNodeId, SymbolNodeId, Option<Occurrence>)> {
        let mut res: Vec<_> = self.0.clone().into_iter().collect();
        res.sort_by(|(from_a, to_a, _), (from_b, to_b, _)| {
            from_a
                .instance_id
                .cmp(&from_b.instance_id)
                .then_with(|| to_a.instance_id.cmp(&to_b.instance_id))
                .then_with(|| from_a.symbol_id.cmp(&from_b.symbol_id))
                .then_with(|| to_a.symbol_id.cmp(&to_b.symbol_id))
        });
        res
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct HasEdge {
    pub parent: SymbolId,
    pub child: SymbolId,
    pub parent_instance: SymbolInstanceId,
    pub child_instance: SymbolInstanceId,
}

pub struct HasEdgeList(pub HashSet<HasEdge>);

impl HasEdgeList {
    pub fn new() -> Self {
        Self(HashSet::new())
    }

    pub fn add(&mut self, edge: HasEdge) {
        self.0.insert(edge);
    }

    pub fn as_vec(&self) -> Vec<HasEdge> {
        let mut res: Vec<_> = self.0.iter().cloned().collect();
        res.sort_by(|a, b| {
            a.parent
                .cmp(&b.parent)
                .then_with(|| a.child.cmp(&b.child))
                .then_with(|| a.parent_instance.cmp(&b.parent_instance))
                .then_with(|| a.child_instance.cmp(&b.child_instance))
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
}
