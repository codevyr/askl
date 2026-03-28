use anyhow::Result;
use serde::{Deserialize, Serialize};

use crate::symbols::{
    SymbolInstanceId, FileId, ModuleId, Occurrence, ProjectId, SymbolId, SymbolScope, SymbolType,
};

#[derive(Debug, PartialEq, Eq)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub module: ModuleId,
    pub symbol_scope: SymbolScope,
}

impl Symbol {
    pub fn new(id: SymbolId, name: &str, module: ModuleId, symbol_scope: SymbolScope) -> Self {
        Self {
            id,
            name: name.to_string(),
            module,
            symbol_scope,
        }
    }
}

#[derive(Debug, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct SymbolInstance {
    pub id: SymbolInstanceId,
    pub symbol: SymbolId,
    pub file_id: FileId,
    pub symbol_type: SymbolType,
    pub offset_range: (i32, i32),
}

impl SymbolInstance {
    pub fn new_nolines(
        id: SymbolInstanceId,
        symbol: SymbolId,
        file_id: FileId,
        symbol_type: SymbolType,
    ) -> Self {
        Self {
            id,
            symbol,
            file_id,
            symbol_type,
            offset_range: (0, 0),
        }
    }

    pub fn new(
        symbol: SymbolId,
        file_id: FileId,
        symbol_type: SymbolType,
        range: &Option<clang_ast::SourceRange>,
    ) -> Result<Self> {
        let (start_offset, end_offset) = Occurrence::offsets_from_range(range)
            .ok_or(anyhow::anyhow!("Range does not provide byte offsets"))?;

        Ok(Self {
            id: SymbolInstanceId::invalid(),
            symbol,
            file_id,
            symbol_type,
            offset_range: (start_offset as i32, end_offset as i32),
        })
    }

    pub fn with_id(self, id: SymbolInstanceId) -> Self {
        let mut res = self;
        res.id = id;
        res
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct Module {
    pub id: ModuleId,
    pub module_name: String,
    pub project_id: ProjectId,
}

impl Module {
    pub fn new(id: ModuleId, module_name: &str, project_id: ProjectId) -> Self {
        Self {
            id,
            module_name: module_name.to_string(),
            project_id,
        }
    }
}

#[derive(Debug, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct File {
    pub id: FileId,
    pub module: ModuleId,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
}

impl File {
    pub fn new(
        id: FileId,
        module: ModuleId,
        module_path: &str,
        filesystem_path: &str,
        filetype: &str,
    ) -> Self {
        Self {
            id,
            module,
            module_path: module_path.to_string(),
            filesystem_path: filesystem_path.to_string(),
            filetype: filetype.to_string(),
            content_hash: "".to_string(),
        }
    }
}

#[derive(Debug, PartialEq, Eq)]
pub struct Reference {
    pub from_symbol_instance: SymbolInstanceId,
    pub to_symbol: SymbolId,
    pub from_file: FileId,
    pub from_offset_start: i64,
    pub from_offset_end: i64,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ModuleFull {
    pub id: ModuleId,
    pub module_name: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct FileFull {
    pub id: FileId,
    pub module: ModuleFull,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
}

#[derive(Debug, PartialEq, Eq)]
pub struct ReferenceFull {
    pub from_symbol_instance: SymbolInstanceId,
    pub to_symbol: SymbolId,
    pub occurrence: Occurrence,
}

#[derive(Debug, PartialEq, Eq)]
pub struct SymbolInstanceFull {
    pub id: SymbolInstanceId,
    pub symbol: SymbolId,
    pub name: String,
    pub symbol_scope: SymbolScope,
    pub file: FileFull,
    pub symbol_type: SymbolType,
    pub occurrence: Occurrence,

    pub children: Vec<ReferenceFull>,
    pub parents: Vec<ReferenceFull>,
}

pub struct Index;

impl Index {
    pub async fn new_or_connect(_database: &str) -> Result<Self> {
        unimplemented!("sqlx-based Index has been removed")
    }

    pub async fn create_or_get_module(&self, _module_name: &str) -> Result<ModuleId> {
        unimplemented!()
    }

    pub async fn create_or_get_fileid(
        &self,
        _module: ModuleId,
        _module_relative_path: &str,
        _file_string: &str,
        _file_type: &str,
    ) -> Result<FileId> {
        unimplemented!()
    }

    pub async fn insert_symbol(
        &self,
        _name: &str,
        _module: ModuleId,
        _scope: SymbolScope,
    ) -> Result<Symbol> {
        unimplemented!()
    }

    pub async fn add_symbol_instance(&self, _instance: SymbolInstance) -> Result<SymbolInstance> {
        unimplemented!()
    }

    pub async fn add_reference(
        &self,
        _from_symbol_instance: SymbolInstanceId,
        _to_symbol: SymbolId,
        _occurrence: &Occurrence,
    ) -> Result<()> {
        unimplemented!()
    }
}
