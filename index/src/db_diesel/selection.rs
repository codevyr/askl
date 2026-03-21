use std::collections::HashSet;

use crate::models_diesel::{Object, Project, Symbol, SymbolInstance, SymbolRef};
use crate::symbols::{
    DeclarationId, FileId, Occurrence, SymbolId, SymbolScope, SymbolType,
};

#[derive(Debug, PartialEq, Eq)]
pub struct ObjectFullDiesel {
    pub id: FileId,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReferenceFullDiesel {
    pub from_decl: DeclarationId,
    pub to_symbol: SymbolId,
    pub occurrence: Occurrence,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SymbolInstanceFullDiesel {
    pub id: DeclarationId,
    pub symbol: SymbolId,
    pub name: String,
    pub symbol_scope: SymbolScope,
    pub object: ObjectFullDiesel,
    pub symbol_type: SymbolType,
    pub occurrence: Occurrence,

    pub children: Vec<ReferenceFullDiesel>,
    pub parents: Vec<ReferenceFullDiesel>,
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct SelectionNode {
    pub symbol: Symbol,
    pub symbol_instance: SymbolInstance,
    pub object: Object,
    pub project: Project,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceResult {
    pub parent_symbol: Symbol,
    pub symbol: Symbol,
    pub symbol_instance: SymbolInstance,
    pub from_instance: SymbolInstance,
    pub symbol_ref: SymbolRef,
    pub from_object: Object,
}

pub type ChildReference = ReferenceResult;

#[derive(Debug, Clone, PartialEq)]
pub struct ParentReference {
    pub to_symbol: Symbol,
    pub to_instance: SymbolInstance,
    pub from_instance: SymbolInstance,
    pub symbol_ref: SymbolRef,
}

/// Containment relationship: parent contains child (parent.offset_range @> child.offset_range)
#[derive(Debug, Clone, PartialEq)]
pub struct HasChildReference {
    pub parent_symbol: Symbol,
    pub parent_instance: SymbolInstance,
    pub child_symbol: Symbol,
    pub child_instance: SymbolInstance,
    pub parent_object: Object,
}

/// Containment relationship: child is contained by parent
#[derive(Debug, Clone, PartialEq)]
pub struct HasParentReference {
    pub child_symbol: Symbol,
    pub child_instance: SymbolInstance,
    pub parent_symbol: Symbol,
    pub parent_instance: SymbolInstance,
}

#[derive(Clone, PartialEq)]
pub struct Selection {
    pub nodes: Vec<SelectionNode>,
    // Reference-based relationships (calls)
    pub parents: Vec<ParentReference>,
    pub children: Vec<ChildReference>,
    // Containment relationships (composition)
    pub has_parents: Vec<HasParentReference>,
    pub has_children: Vec<HasChildReference>,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
            has_parents: Vec::new(),
            has_children: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: Selection) {
        self.nodes.extend(other.nodes);
        self.parents.extend(other.parents);
        self.children.extend(other.children);
        self.has_parents.extend(other.has_parents);
        self.has_children.extend(other.has_children);
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get_decl_ids(&self) -> Vec<i32> {
        self.nodes.iter().map(|node| node.symbol_instance.id).collect()
    }

    /// Remove any references to symbol instances that are no longer in the selection
    pub fn prune_references(&mut self) {
        let selection = self;

        let node_instance_ids: HashSet<_> = selection
            .nodes
            .iter()
            .map(|s| DeclarationId::new(s.symbol_instance.id))
            .collect();
        selection
            .parents
            .retain(|c| node_instance_ids.contains(&DeclarationId::new(c.to_instance.id)));
        selection
            .children
            .retain(|c| node_instance_ids.contains(&DeclarationId::new(c.from_instance.id)));
        // Prune containment relationships
        selection
            .has_parents
            .retain(|c| node_instance_ids.contains(&DeclarationId::new(c.child_instance.id)));
        selection
            .has_children
            .retain(|c| node_instance_ids.contains(&DeclarationId::new(c.parent_instance.id)));
    }
}

impl std::fmt::Debug for Selection {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Selection")
            .field(
                "nodes",
                &self
                    .nodes
                    .iter()
                    .map(|n| n.symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field(
                "parents",
                &self
                    .parents
                    .iter()
                    .map(|p| p.from_instance.id)
                    .collect::<Vec<_>>(),
            )
            .field(
                "children",
                &self
                    .children
                    .iter()
                    .map(|c| c.symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field(
                "has_parents",
                &self
                    .has_parents
                    .iter()
                    .map(|p| p.parent_symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .field(
                "has_children",
                &self
                    .has_children
                    .iter()
                    .map(|c| c.child_symbol.name.clone())
                    .collect::<Vec<_>>(),
            )
            .finish()
    }
}
