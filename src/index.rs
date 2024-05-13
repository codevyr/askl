use anyhow::{bail, Result};
use serde::{Deserialize, Serialize};
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};

use crate::symbols::{FileId, Occurrence, SymbolId, SymbolType};

#[derive(Debug, sqlx::FromRow)]
pub struct Symbol {
    pub id: SymbolId,
    pub name: String,
    pub file_id: FileId,
    pub symbol_type: SymbolType,
    pub line_start: i64,
    pub col_start: i64,
    pub line_end: i64,
    pub col_end: i64,
}

#[derive(Debug, sqlx::FromRow, Deserialize, Serialize, Clone)]
pub struct File {
    pub id: FileId,
    pub path: String,
    pub project: String,
    pub filetype: String,
}

#[derive(Debug, sqlx::FromRow)]
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

    pub async fn new_or_connect(database: &str) -> Result<Self> {
        let options = SqliteConnectOptions::new()
            .filename(database)
            .create_if_missing(true);

        let pool = SqlitePool::connect_with(options).await?;

        sqlx::query!(
            r#"
            CREATE TABLE IF NOT EXISTS files
            (
                id INTEGER PRIMARY KEY,
                path TEXT NOT NULL,
                project TEXT NOT NULL,
                filetype TEXT NOT NULL,
                UNIQUE (path, project)
            );

            CREATE TABLE IF NOT EXISTS symbols
            (
                id INTEGER PRIMARY KEY,
                name TEXT NOT NULL,
                file_id INTEGER NOT NULL,
                symbol_type INTEGER NOT NULL,
                line_start INTEGER NOT NULL,
                col_start INTEGER NOT NULL,
                line_end INTEGER NOT NULL,
                col_end INTEGER NOT NULL,
                FOREIGN KEY (file_id) REFERENCES files(id),
                UNIQUE (name, file_id, symbol_type)
            );

            CREATE TABLE IF NOT EXISTS symbol_refs
            (
                from_symbol INTEGER NOT NULL,
                to_symbol INTEGER NOT NULL,
                from_line INTEGER NOT NULL,
                from_col_start INTEGER NOT NULL,
                from_col_end INTEGER NOT NULL,
                FOREIGN KEY (from_symbol) REFERENCES symbols(id),
                FOREIGN KEY (to_symbol) REFERENCES symbols(id),
                UNIQUE (from_symbol, to_symbol, from_line, from_col_start, from_col_end)
            );
            "#
        )
        .execute(&pool)
        .await?;

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

    pub async fn create_or_get_symbolid(
        &self,
        name: &str,
        symbol_type: SymbolType,
        occurrence: Occurrence,
    ) -> Result<SymbolId> {
        let rec = sqlx::query!(
            r#"
            SELECT id
            FROM symbols
            WHERE name = ?1 AND file_id = ?2 AND symbol_type = ?3
            "#,
            name,
            occurrence.file,
            symbol_type
        )
        .fetch_optional(&self.pool)
        .await?;

        if let Some(rec) = rec {
            return Ok(rec.id.unwrap().into());
        }

        let symbol_id = sqlx::query!(
            r#"
            INSERT INTO symbols (name, file_id, symbol_type, line_start, col_start, line_end, col_end)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
            "#,
            name,
            occurrence.file,
            symbol_type,
            occurrence.line_start,
            occurrence.column_start,
            occurrence.line_end,
            occurrence.column_end
        )
        .execute(&self.pool)
        .await?
        .last_insert_rowid();

        Ok(symbol_id.into())
    }

    pub async fn find_symbols(&self, name: &str) -> Result<Vec<Symbol>> {
        let symbols: Vec<Symbol> = sqlx::query_as!(
            Symbol,
            r#"
            SELECT *
            FROM symbols
            WHERE name = ?1
            "#,
            name
        )
        .fetch_all(&self.pool)
        .await?;

        if symbols.len() == 0 {
            bail!("Symbol not found")
        }

        Ok(symbols)
    }

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
            occurrence.column_start,
            occurrence.column_end
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
            SELECT *
            FROM symbols
            "#
        )
        .fetch_all(&self.pool)
        .await?;

        Ok(symbols)
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
