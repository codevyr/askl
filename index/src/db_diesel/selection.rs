use crate::models_diesel::{Declaration, File, Module, Symbol, SymbolRef};
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
}

#[derive(Debug, Clone, PartialEq)]
pub struct ReferenceResult {
    pub symbol: Symbol,
    pub declaration: Declaration,
    pub symbol_ref: SymbolRef,
    pub from_file: File,
}

pub type ChildReference = ReferenceResult;

#[derive(Debug, Clone, PartialEq)]
pub struct ParentReference {
    pub to_symbol: Symbol,
    pub to_declaration: Declaration,
    pub symbol_ref: SymbolRef,
}

#[derive(Debug, Clone, PartialEq)]
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
}
