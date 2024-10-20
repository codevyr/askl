use std::{path::Path, str::FromStr};

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePool},
    Pool, Sqlite,
};

use crate::symbols::{FileId, SymbolId, SymbolScope, SymbolType, Occurrence, DeclarationId};

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub module_id: Option<FileId>,
    pub symbol_scope: SymbolScope,
}

impl Symbol {
    pub fn new(
        id: SymbolId,
        name: &str,
        module_id: Option<FileId>,
        symbol_scope: SymbolScope,
    ) -> Self {
        Self {
            id,
            name: name.to_string(),
            module_id,
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
    pub line_start: i64,
    pub col_start: i64,
    pub line_end: i64,
    pub col_end: i64,
}

impl Declaration {
    pub fn new_nolines(id: DeclarationId, symbol: SymbolId, file_id: FileId, symbol_type: SymbolType) -> Self {
        Self {
            id,
            symbol,
            file_id,
            symbol_type,
            line_start: 1,
            col_start: 1,
            line_end: 1,
            col_end: 1,
        }
    }

    pub fn new(
        symbol: SymbolId,
        file_id: FileId,
        symbol_type: SymbolType,
        range: &Option<clang_ast::SourceRange>,
    ) -> Result<Self> {
        let range = if let Some(range) = range {
            range
        } else {
            bail!("Range does not exist");
        };

        let begin = if let Some(begin) = &range.begin.expansion_loc {
            begin
        } else {
            bail!("Begin does not exist");
        };

        let end = if let Some(end) = &range.end.expansion_loc {
            end
        } else {
            bail!("End does not exist");
        };

        Ok(Self {
            id: DeclarationId::invalid(),
            symbol,
            file_id,
            symbol_type,
            line_start: begin.line as i64,
            col_start: begin.col as i64,
            line_end: end.line as i64,
            col_end: end.col as i64,
        })
    }

    pub fn with_id(self, id: DeclarationId) -> Self {
        let mut res = self;
        res.id = id;
        res
    }
}

#[derive(Debug, sqlx::FromRow, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct File {
    pub id: FileId,
    pub project: String,
    pub root_dir: String,
    pub path: String,
    pub filetype: String,
}

impl File {
    pub fn new(id: FileId, project: &str, root_dir: &str, path: &str, filetype: &str) -> Self {
        Self {
            id,
            project: project.to_string(),
            root_dir: root_dir.to_string(),
            path: path.to_string(),
            filetype: filetype.to_string(),
        }
    }
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct Reference {
    pub from_decl: DeclarationId,
    pub to_symbol: SymbolId,
    pub from_line: i64,
    pub from_col_start: i64,
    pub from_col_end: i64,
}

pub struct Index {
    pool: SqlitePool,
}

impl Index {
    pub async fn connect(database: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new().filename(database);

        let pool = SqlitePool::connect_with(options).await?;

        Ok(Self { pool })
    }

    async fn create_tables(pool: &Pool<Sqlite>) -> Result<()> {
        sqlx::query_file!("../sql/create_tables.sql")
            .execute(pool)
            .await?;

        Ok(())
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

    pub async fn load_test_input(&self, input_path: &str) -> Result<()> {
        match input_path {
            "test_input_a.sql" => sqlx::query_file!("../sql/test_input_a.sql")
            .execute(&self.pool)
            .await?,
            "test_input_b.sql" => sqlx::query_file!("../sql/test_input_b.sql")
            .execute(&self.pool)
            .await?,
            _ => panic!("Impossible input file"),
        };
        

        Ok(())
    }

    pub async fn create_or_get_fileid(
        &self,
        project: &str,
        root_dir: &str,
        file_string: &str,
        file_type: &str,
    ) -> Result<FileId> {
        let path_in_root = Path::new(root_dir).join(file_string);

        let file_string = if !file_string.starts_with("/") {
            path_in_root.as_os_str().to_str().unwrap()
        } else {
            file_string
        };

        let rec = sqlx::query!(
            r#"
            SELECT id
            FROM files
            WHERE path = ?1 AND project = ?2
            "#,
            file_string,
            project
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(rec) = rec {
            return Ok(rec.id.unwrap().into());
        }

        let file_id = sqlx::query!(
            r#"
            INSERT INTO files (project, root_dir, path, filetype)
            VALUES (?1, ?2, ?3, ?4)
            "#,
            project,
            root_dir,
            file_string,
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
        module: Option<FileId>,
        scope: SymbolScope,
    ) -> Result<Symbol> {
        let rec = if let Some(module_id) = module {
            sqlx::query_as!(
                Symbol,
                r#"
                SELECT id, name, module_id AS "module_id?: FileId", symbol_scope
                FROM symbols
                WHERE name = ? AND module_id = ? AND symbol_scope = ?
                "#,
                name,
                module_id,
                scope
            )
            .fetch_optional(&self.pool)
            .await?
        } else {
            sqlx::query_as!(
                Symbol,
                r#"
                SELECT id, name, module_id AS "module_id?: FileId", symbol_scope
                FROM symbols
                WHERE name = ? AND symbol_scope = ?
                "#,
                name,
                scope
            )
            .fetch_optional(&self.pool)
            .await?
        };

        if let Some(symbol) = rec {
            return Ok(symbol);
        }

        let rec = sqlx::query!(
            r#"
            INSERT INTO symbols(name, module_id, symbol_scope)
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
                SELECT id, symbol, file_id, symbol_type, line_start, col_start, line_end, col_end
                FROM declarations
                WHERE symbol = ? AND file_id = ? AND line_start = ? AND col_start = ? AND line_end = ? AND col_end = ?
                "#,
                declaration.symbol,
                declaration.file_id,
                declaration.line_start,
                declaration.col_start,
                declaration.line_end,
                declaration.col_end                
            )
            .fetch_optional(&self.pool)
            .await?;

        if let Some(declaration) = rec {
            return Ok(declaration);
        }

        let rec = sqlx::query!(
                r#"
                INSERT INTO declarations (symbol, file_id, symbol_type, line_start, col_start, line_end, col_end)
                VALUES (?, ?, ?, ?, ?, ?, ?)
                RETURNING id
                "#,
                declaration.symbol,
                declaration.file_id,
                declaration.symbol_type,
                declaration.line_start,
                declaration.col_start,
                declaration.line_end,
                declaration.col_end
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
            INSERT OR IGNORE INTO symbol_refs (from_decl, to_symbol, from_line, from_col_start, from_col_end)
            VALUES (?, ?, ?, ?, ?)
            "#,
            from_decl,
            to_symbol,
            occurrence.line_start,
            occurrence.column_start,
            occurrence.column_end
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
            SELECT id, name, module_id AS "module_id?: FileId" , symbol_scope
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
            SELECT id, symbol, file_id, symbol_type, line_start, col_start, line_end, col_end
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
            SELECT id, symbol, file_id, symbol_type, line_start, col_start, line_end, col_end
            FROM declarations
            WHERE symbol = ?
            "#,
            symbol_id
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(declarations)
    }

    pub async fn all_files(&self) -> Result<Vec<File>> {
        let files: Vec<File> = sqlx::query_as!(
            File,
            r#"
            SELECT *
            FROM files
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(files)
    }

    pub async fn get_file(&self, file_id: FileId) -> Result<File> {
        let files: File = sqlx::query_as!(
            File,
            r#"
            SELECT *
            FROM files
            WHERE id = ?
            "#,
            file_id
        )
        .fetch_one(&self.pool)
        .await?;

        Ok(files)
    }

    pub async fn all_refs(&self) -> Result<Vec<Reference>> {
        let references: Vec<Reference> = sqlx::query_as!(
            Reference,
            r#"
            SELECT from_decl, to_symbol, from_line, from_col_start, from_col_end
            FROM symbol_refs
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(references)
    }

    pub async fn get_parents(&self, child_declaration: DeclarationId) -> Result<Vec<Reference>> {
        let references: Vec<Reference> = sqlx::query_as!(
            Reference,
            r#"
            SELECT symbol_refs.*
            FROM symbol_refs
            INNER JOIN declarations
            ON declarations.symbol = symbol_refs.to_symbol
            WHERE declarations.id = ?;
            "#,
            child_declaration
        ).fetch_all(&self.pool)
        .await?;

        Ok(references)
    }

    pub async fn get_children(&self, parent_declaration: DeclarationId) -> Result<Vec<Reference>> {
        let references: Vec<Reference> = sqlx::query_as!(
            Reference,
            r#"
            SELECT symbol_refs.*
            FROM symbol_refs
            INNER JOIN declarations
            ON declarations.symbol = symbol_refs.to_symbol
            WHERE symbol_refs.from_decl = ?;
            "#,
            parent_declaration
        ).fetch_all(&self.pool)
        .await?;

        Ok(references)
    }
}
