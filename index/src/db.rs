use std::str::FromStr;

use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sqlx::{
    sqlite::{SqliteConnectOptions, SqlitePool},
    Pool, Sqlite,
};

use crate::symbols::{FileId, SymbolId, SymbolScope, SymbolType};

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

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct Occurrence {
    pub symbol: SymbolId,
    pub file_id: FileId,
    pub symbol_type: SymbolType,
    pub line_start: i64,
    pub col_start: i64,
    pub line_end: i64,
    pub col_end: i64,
}

impl Occurrence {
    pub fn new_nolines(
        symbol: SymbolId,
        file_id: FileId,
        symbol_type: SymbolType,
    ) -> Self {
        Self {
            symbol,
            file_id,
            symbol_type,
            line_start: 1,
            col_start: 1,
            line_end: 1,
            col_end: 1,
        }
    }
}

#[derive(Debug, sqlx::FromRow, Deserialize, Serialize, Clone, PartialEq, Eq)]
pub struct File {
    pub id: FileId,
    pub path: String,
    pub project: String,
    pub filetype: String,
}

impl File {
    pub fn new(id: FileId, path: &str, project: &str, filetype: &str) -> Self {
        Self {
            id,
            path: path.to_string(),
            filetype: filetype.to_string(),
            project: project.to_string(),
        }
    }
}

#[derive(Debug, sqlx::FromRow, PartialEq, Eq)]
pub struct Reference {
    pub from_symbol: SymbolId,
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
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options).await?;

        Self::create_tables(&pool).await?;

        Ok(Self { pool })
    }

    pub async fn create_or_get_fileid(
        &self,
        file_string: &str,
        project: &str,
        file_type: &str,
    ) -> Result<FileId> {
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
            INSERT INTO files (path, project, filetype)
            VALUES (?1, ?2, ?3)
            "#,
            file_string,
            project,
            file_type,
        )
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        Ok(file_id.into())
    }

    // pub async fn create_symbol(&self, new_symbol: Symbol) -> Result<SymbolId> {
    //     let id = sqlx::query!(
    //             r#"
    //             INSERT INTO symbol_occ (file_id, symbol_type, symbol_scope, line_start, col_start, line_end, col_end)
    //             VALUES (?, ?, ?, ?, ?, ?, ?)
    //             "#,
    //             new_symbol.file_id,
    //             new_symbol.symbol_type,
    //             new_symbol.symbol_scope,
    //             new_symbol.line_start,
    //             new_symbol.col_start,
    //             new_symbol.line_end,
    //             new_symbol.col_end
    //         )
    //         .execute(&self.pool)
    //         .await?
    //         .last_insert_rowid();

    //     Ok(id.into())
    // }

    pub async fn get_symbol(
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
        println!("NEW SYMBOL {:?}", new_symbol);
        return Ok(new_symbol);
    }

    // pub async fn create_or_get_symbol(
    //     &self,
    //     name: &str,
    //     symbol_type: SymbolType,
    //     symbol_scope: SymbolScope,
    //     occurrence: Occurrence,
    // ) -> Result<Symbol> {
    //     let rec = sqlx::query_as!(
    //         Symbol,
    //         r#"
    //         SELECT id, name, module_id
    //         FROM symbols
    //         WHERE name = ?1
    //         "#,
    //         name,
    //     )
    //     .fetch_optional(&self.pool)
    //     .await?;

    //     if let Some(rec) = rec {
    //         return Ok(rec.into());
    //     }

    //     let symbol = sqlx::query_as!(
    //         Symbol,
    //         r#"
    //         INSERT INTO symbols (name, file_id, symbol_type, symbol_scope, line_start, col_start, line_end, col_end)
    //         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)
    //         RETURNING id, name, file_id, symbol_type, symbol_scope, line_start, col_start, line_end, col_end
    //         "#,
    //         name,
    //         occurrence.file,
    //         symbol_type,
    //         symbol_scope,
    //         occurrence.line_start,
    //         occurrence.column_start,
    //         occurrence.line_end,
    //         occurrence.column_end
    //     )
    //     .fetch_one(&self.pool)
    //     .await?;

    //     Ok(symbol.into())
    // }

    // pub async fn find_symbols(&self, name: &str) -> Result<Vec<Symbol>> {
    //     let symbols: Vec<Symbol> = sqlx::query_as!(
    //         Symbol,
    //         r#"
    //         SELECT id, name, module_id
    //         FROM symbols
    //         WHERE name = ?1
    //         "#,
    //         name
    //     )
    //     .fetch_all(&self.pool)
    //     .await?;

    //     if symbols.len() == 0 {
    //         bail!("Symbol not found")
    //     }

    //     Ok(symbols)
    // }

    pub async fn add_reference(
        &self,
        from_symbol: SymbolId,
        to_symbol: SymbolId,
        occurrence: &Occurrence,
    ) -> Result<()> {
        let res = sqlx::query!(
            r#"
            INSERT INTO symbol_refs (from_symbol, to_symbol, from_line, from_col_start, from_col_end)
            VALUES (?, ?, ?, ?, ?)
            "#,
            from_symbol,
            to_symbol,
            occurrence.line_start,
            occurrence.col_start,
            occurrence.col_end
        )
        .execute(&self.pool)
        .await;

        if let Err(err) = &res {
            log::error!(
                "Failed to add reference {} {}->{} {:?}",
                err,
                from_symbol,
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

    pub async fn all_occurrences(&self) -> Result<Vec<Occurrence>> {
        let occurrences: Vec<Occurrence> = sqlx::query_as!(
            Occurrence,
            r#"
            SELECT symbol, file_id, symbol_type, line_start, col_start, line_end, col_end
            FROM occurrences
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(occurrences)
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

    pub async fn all_refs(&self) -> Result<Vec<Reference>> {
        let references: Vec<Reference> = sqlx::query_as!(
            Reference,
            r#"
            SELECT *
            FROM symbol_refs
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(references)
    }
}
