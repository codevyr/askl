use anyhow::Result;
use serde::{Deserialize, Serialize};
#[cfg(feature = "legacy-sqlite")]
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePool},
    Pool, Sqlite,
};
#[cfg(feature = "legacy-sqlite")]
use std::{path::Path, str::FromStr};

use crate::symbols::{
    DeclarationId, FileId, ModuleId, Occurrence, ProjectId, SymbolId, SymbolScope, SymbolType,
};

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
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

#[derive(Debug, sqlx::FromRow, PartialEq, Eq, Clone, Serialize, Deserialize)]
pub struct Declaration {
    pub id: DeclarationId,
    pub symbol: SymbolId,
    pub file_id: FileId,
    pub symbol_type: SymbolType,
    pub start_offset: i64,
    pub end_offset: i64,
}

impl Declaration {
    pub fn new_nolines(
        id: DeclarationId,
        symbol: SymbolId,
        file_id: FileId,
        symbol_type: SymbolType,
    ) -> Self {
        Self {
            id,
            symbol,
            file_id,
            symbol_type,
            start_offset: 0,
            end_offset: 0,
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
            id: DeclarationId::invalid(),
            symbol,
            file_id,
            symbol_type,
            start_offset: start_offset as i64,
            end_offset: end_offset as i64,
        })
    }

    pub fn with_id(self, id: DeclarationId) -> Self {
        let mut res = self;
        res.id = id;
        res
    }
}

#[derive(Debug, sqlx::FromRow, Deserialize, Serialize, Clone, PartialEq, Eq)]
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

#[derive(Debug, sqlx::FromRow, Deserialize, Serialize, Clone, PartialEq, Eq)]
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

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct Reference {
    pub from_decl: DeclarationId,
    pub to_symbol: SymbolId,
    pub from_file: FileId,
    pub from_offset_start: i64,
    pub from_offset_end: i64,
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct ModuleFull {
    pub id: ModuleId,
    pub module_name: String,
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct FileFull {
    pub id: FileId,
    pub module: ModuleFull,
    pub module_path: String,
    pub filesystem_path: String,
    pub filetype: String,
    pub content_hash: String,
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct ReferenceFull {
    pub from_decl: DeclarationId,
    pub to_symbol: SymbolId,
    pub occurrence: Occurrence,
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct DeclarationFull {
    pub id: DeclarationId,
    pub symbol: SymbolId,
    pub name: String,
    pub symbol_scope: SymbolScope,
    pub file: FileFull,
    pub symbol_type: SymbolType,
    pub occurrence: Occurrence,

    pub children: Vec<ReferenceFull>,
    pub parents: Vec<ReferenceFull>,
}

#[cfg(feature = "legacy-sqlite")]
pub struct Index {
    pool: SqlitePool,
}

#[cfg(feature = "legacy-sqlite")]
impl Index {
    pub async fn connect(database: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new().filename(database);

        let pool = SqlitePool::connect_with(options).await?;

        Ok(Self { pool })
    }

    async fn create_tables(_pool: &Pool<Sqlite>) -> Result<()> {
        panic!("Use migrations instead of create_tables");
        // sqlx::query_file!("../sql/create_tables.sql")
        //     .execute(pool)
        //     .await?;
    }

    pub async fn new_in_memory() -> Result<Self> {
        let options = SqliteConnectOptions::from_str("sqlite::memory:")?.create_if_missing(true);

        let pool = SqlitePool::connect_with(options).await?;

        Self::create_tables(&pool).await?;

        Ok(Self { pool })
    }

    pub async fn new_or_connect(database: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(database)
            .create_if_missing(true)
            .pragma("journal_mode", "OFF")
            .pragma("temp_store", "MEMORY")
            .pragma("synchronous", "0");

        let pool = SqlitePool::connect_with(options).await?;

        Self::create_tables(&pool).await?;

        Ok(Self { pool })
    }

    pub const TEST_INPUT_A: &'static str = "test_input_a.sql";
    pub const TEST_INPUT_B: &'static str = "test_input_b.sql";
    pub const TEST_INPUT_MODULES: &'static str = "test_input_modules.sql";

    pub async fn create_or_get_module(&self, module_name: &str) -> Result<ModuleId> {
        let rec = sqlx::query!(
            r#"
            SELECT id AS "module_id?: ModuleId"
            FROM modules
            WHERE module_name = ?1
            "#,
            module_name
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(rec) = rec {
            return Ok(rec.module_id.unwrap());
        }

        let module_id = sqlx::query!(
            r#"
            INSERT INTO modules (module_name)
            VALUES (?1)
            "#,
            module_name,
        )
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        Ok(module_id.into())
    }

    pub async fn create_or_get_fileid(
        &self,
        module: ModuleId,
        module_relative_path: &str,
        file_string: &str,
        file_type: &str,
    ) -> Result<FileId> {
        let path_in_root = Path::new(module_relative_path).join(file_string);

        let filesystem_path = if !file_string.starts_with("/") {
            path_in_root.as_os_str().to_str().unwrap()
        } else {
            file_string
        };

        let rec = sqlx::query!(
            r#"
            SELECT id AS "file_id?: FileId"
            FROM files
            WHERE module = ?1 AND module_path = ?2
            "#,
            module,
            module_relative_path
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(rec) = rec {
            return Ok(rec.file_id.unwrap());
        }

        let file_id = sqlx::query!(
            r#"
            INSERT INTO files (module, module_path, filesystem_path, filetype)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            module,
            module_relative_path,
            filesystem_path,
            file_type,
        )
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        Ok(file_id.into())
    }

    pub async fn insert_symbol(
        &self,
        name: &str,
        module: ModuleId,
        scope: SymbolScope,
    ) -> Result<Symbol> {
        let rec = sqlx::query_as!(
            Symbol,
            r#"
                SELECT id, name, module AS "module: ModuleId", symbol_scope
                FROM symbols
                WHERE name = ? AND module = ? AND symbol_scope = ?
                "#,
            name,
            module,
            scope
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(symbol) = rec {
            return Ok(symbol);
        }

        let rec = sqlx::query!(
            r#"
            INSERT INTO symbols(name, module, symbol_scope)
            VALUES (?1, ?2, ?3)
            RETURNING id
            "#,
            name,
            module,
            scope
        )
        .fetch_one(&self.pool)
        .await?;

        let id: SymbolId = rec.id.into();
        let new_symbol = Symbol::new(id, name, module, scope);
        return Ok(new_symbol);
    }

    pub async fn add_declaration(&self, declaration: Declaration) -> Result<Declaration> {
        let rec = sqlx::query_as!(
            Declaration,
            r#"
                SELECT id, symbol, file_id, symbol_type, start_offset, end_offset
                FROM declarations
                WHERE symbol = ? AND file_id = ? AND start_offset = ? AND end_offset = ?
                "#,
            declaration.symbol,
            declaration.file_id,
            declaration.start_offset,
            declaration.end_offset
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(declaration) = rec {
            return Ok(declaration);
        }

        let rec = sqlx::query!(
            r#"
                INSERT INTO declarations (symbol, file_id, symbol_type, start_offset, end_offset)
                VALUES (?, ?, ?, ?, ?)
                RETURNING id
                "#,
            declaration.symbol,
            declaration.file_id,
            declaration.symbol_type,
            declaration.start_offset,
            declaration.end_offset
        )
        .fetch_one(&self.pool)
        .await?;

        let id: DeclarationId = rec.id.into();

        Ok(declaration.with_id(id))
    }

    pub async fn add_reference(
        &self,
        from_decl: DeclarationId,
        to_symbol: SymbolId,
        occurrence: &Occurrence,
    ) -> Result<()> {
        let res = sqlx::query!(
            r#"
            INSERT OR IGNORE INTO symbol_refs (to_symbol, from_file, from_offset_start, from_offset_end)
            VALUES (?, ?, ?, ?)
            "#,
            to_symbol,
            occurrence.file,
            occurrence.start_offset,
            occurrence.end_offset
        )
        .execute(&self.pool)
        .await;

        if let Err(err) = &res {
            log::error!(
                "Failed to add reference {} {}->{} {:?}",
                err,
                from_decl,
                to_symbol,
                occurrence
            );
            res?;
        }

        Ok(())
    }

    pub async fn all_symbols(&self) -> Result<Vec<Symbol>> {
        let symbols: Vec<Symbol> = sqlx::query_as!(
            Symbol,
            r#"
            SELECT id, name, module, symbol_scope
            FROM symbols
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(symbols)
    }

    pub async fn all_declarations(&self) -> Result<Vec<Declaration>> {
        let declarations: Vec<Declaration> = sqlx::query_as!(
            Declaration,
            r#"
            SELECT id, symbol, file_id, symbol_type, start_offset, end_offset
            FROM declarations
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(declarations)
    }

    pub async fn symbol_declarations(&self, symbol_id: SymbolId) -> Result<Vec<Declaration>> {
        let declarations: Vec<Declaration> = sqlx::query_as!(
            Declaration,
            r#"
            SELECT id, symbol, file_id, symbol_type, start_offset, end_offset
            FROM declarations
            WHERE symbol = ?
            "#,
            symbol_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(declarations)
    }

    pub async fn all_modules(&self) -> Result<Vec<Module>> {
        let files: Vec<Module> = sqlx::query_as!(
            Module,
            r#"
            SELECT *
            FROM modules
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(files)
    }

    pub async fn all_files(&self) -> Result<Vec<File>> {
        let files: Vec<File> = sqlx::query_as!(
            File,
            r#"
            SELECT id, module, module_path, filesystem_path, filetype, content_hash
            FROM files
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(files)
    }

    pub async fn all_refs(&self) -> Result<Vec<Reference>> {
        let references: Vec<Reference> = sqlx::query_as!(
            Reference,
            r#"
            SELECT from_decls.id as "from_decl!: DeclarationId",
                   symbol_refs.to_symbol as "to_symbol: SymbolId",
                   symbol_refs.from_file as "from_file: FileId",
                   symbol_refs.from_offset_start,
                   symbol_refs.from_offset_end
            FROM symbol_refs
            JOIN declarations AS from_decls
              ON symbol_refs.from_file = from_decls.file_id
             AND from_decls.start_offset <= symbol_refs.from_offset_start
             AND from_decls.end_offset >= symbol_refs.from_offset_end
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(references)
    }
}
