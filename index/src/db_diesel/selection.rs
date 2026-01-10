use std::collections::HashSet;

use crate::models_diesel::{Declaration, File, Module, Project, Symbol, SymbolRef};
use crate::symbols::{
    DeclarationId, FileId, ModuleId, Occurrence, SymbolId, SymbolScope, SymbolType,
};

#[derive(Debug, PartialEq, Eq)]
pub struct ModuleFullDiesel {
    pub id: ModuleId,
    pub module_name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileFullDiesel {
    pub id: FileId,
    pub module: ModuleFullDiesel,
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
pub struct DeclarationFullDiesel {
    pub id: DeclarationId,
    pub symbol: SymbolId,
    pub name: String,
    pub symbol_scope: SymbolScope,
    pub file: FileFullDiesel,
    pub symbol_type: SymbolType,
    pub occurrence: Occurrence,

    pub children: Vec<ReferenceFullDiesel>,
    pub parents: Vec<ReferenceFullDiesel>,
}

#[derive(Debug, Clone, PartialEq, Hash, Eq)]
pub struct SelectionNode {
    pub symbol: Symbol,
    pub declaration: Declaration,
    pub module: Module,
    pub file: File,
    pub project: Project,
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceResult {
    pub parent_symbol: Symbol,
    pub symbol: Symbol,
    pub declaration: Declaration,
    pub from_declaration: Declaration,
    pub symbol_ref: SymbolRef,
    pub from_file: File,
}

pub type ChildReference = ReferenceResult;

#[derive(Debug, Clone, PartialEq)]
pub struct ParentReference {
    pub to_symbol: Symbol,
    pub to_declaration: Declaration,
    pub from_declaration: Declaration,
    pub symbol_ref: SymbolRef,
}

#[derive(Clone, PartialEq)]
pub struct Selection {
    pub nodes: Vec<SelectionNode>,
    pub parents: Vec<ParentReference>,
    pub children: Vec<ChildReference>,
}

impl Selection {
    pub fn new() -> Self {
        Self {
            nodes: Vec::new(),
            parents: Vec::new(),
            children: Vec::new(),
        }
    }

    pub fn extend(&mut self, other: Selection) {
        self.nodes.extend(other.nodes);
        self.parents.extend(other.parents);
        self.children.extend(other.children);
    }

    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    pub fn get_decl_ids(&self) -> Vec<i32> {
        self.nodes.iter().map(|node| node.declaration.id).collect()
    }

    /// Remove any references to declarations that are no longer in the selection
    pub fn prune_references(&mut self) {
        let selection = self;

        let node_declaration_ids: HashSet<_> = selection
            .nodes
            .iter()
            .map(|s| DeclarationId::new(s.declaration.id))
            .collect();
        selection
            .parents
            .retain(|c| node_declaration_ids.contains(&DeclarationId::new(c.to_declaration.id)));
        selection
            .children
            .retain(|c| node_declaration_ids.contains(&DeclarationId::new(c.from_declaration.id)));
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
                    .map(|p| p.from_declaration.id)
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
            .finish()
    }
}
