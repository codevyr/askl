use std::fmt;
use std::io::Write;

use diesel::prelude::*;
use diesel::{
    deserialize::{self, FromSql},
    pg::Pg,
    serialize::{self, IsNull, Output, ToSql},
};
use diesel_async::pooled_connection::{bb8, bb8::Pool};
use diesel_async::AsyncPgConnection;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use index::ltree::LtreeValue;
use index::schema_diesel as index_schema;
use index::symbols::FileId;

mod query;
mod upload;

#[cfg(test)]
mod tests;

const MAX_INSERT_ROWS: usize = 1000;
// NewSymbol has 7 fields; PostgreSQL limits bind parameters to 65_535 (u16::MAX).
// 65_535 / 7 = 9_362; use 1_000 to leave a safe margin.
const MAX_SYMBOL_INSERT_ROWS: usize = 1_000;

#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
#[derive(diesel::expression::AsExpression, diesel::deserialize::FromSqlRow)]
#[diesel(sql_type = diesel::sql_types::Text)]
pub enum UploadStatus {
    Uploading,
    Complete,
    Failed,
    Deleting,
}

impl fmt::Display for UploadStatus {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            UploadStatus::Uploading => f.write_str("uploading"),
            UploadStatus::Complete => f.write_str("complete"),
            UploadStatus::Failed => f.write_str("failed"),
            UploadStatus::Deleting => f.write_str("deleting"),
        }
    }
}

impl ToSql<diesel::sql_types::Text, Pg> for UploadStatus {
    fn to_sql<'b>(&'b self, out: &mut Output<'b, '_, Pg>) -> serialize::Result {
        out.write_all(self.to_string().as_bytes())?;
        Ok(IsNull::No)
    }
}

impl FromSql<diesel::sql_types::Text, Pg> for UploadStatus {
    fn from_sql(
        bytes: <Pg as diesel::backend::Backend>::RawValue<'_>,
    ) -> deserialize::Result<Self> {
        let s = <String as FromSql<diesel::sql_types::Text, Pg>>::from_sql(bytes)?;
        match s.as_str() {
            "uploading" => Ok(UploadStatus::Uploading),
            "complete" => Ok(UploadStatus::Complete),
            "failed" => Ok(UploadStatus::Failed),
            "deleting" => Ok(UploadStatus::Deleting),
            _ => Err(format!("unknown upload_status value: {}", s).into()),
        }
    }
}

#[derive(Clone)]
pub struct IndexStore {
    pool: Pool<AsyncPgConnection>,
}

#[derive(Debug, PartialEq)]
pub enum UploadError {
    Conflict,
    Invalid(String),
    Storage(String),
}

#[derive(Debug)]
pub enum StoreError {
    Storage(String),
}

#[derive(Debug, Serialize)]
pub struct ProjectInfo {
    pub id: i32,
    pub project_name: String,
    pub root_path: String,
    pub upload_status: UploadStatus,
}

#[derive(Debug, Serialize)]
pub struct ProjectDetails {
    pub id: i32,
    pub project_name: String,
    pub root_path: String,
    pub upload_status: UploadStatus,
    pub file_count: i64,
    pub symbol_count: i64,
    pub symbol_chunks_total: Option<i32>,
    pub object_chunks_total: Option<i32>,
    pub committed_symbol_chunks: Vec<i32>,
    pub committed_object_chunks: Vec<i32>,
}

#[derive(Debug, Serialize, Clone)]
pub struct ProjectTreeNode {
    pub name: String,
    pub path: String,
    pub node_type: String,
    pub has_children: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub file_id: Option<FileId>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub filetype: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub compact_path: Option<String>,
}

#[derive(Debug)]
pub enum ProjectTreeResult {
    ProjectNotFound,
    NotReady,
    NotDirectory,
    Nodes(Vec<ProjectTreeNode>),
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::projects)]
struct NewProject {
    project_name: String,
    root_path: String,
    upload_status: UploadStatus,
    symbol_chunks_total: Option<i32>,
    object_chunks_total: Option<i32>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::project_symbol_chunks)]
pub(super) struct NewProjectSymbolChunk {
    pub project_id: i32,
    pub seq: i32,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::project_object_chunks)]
pub(super) struct NewProjectObjectChunk {
    pub project_id: i32,
    pub seq: i32,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::objects)]
struct NewObject {
    project_id: i32,         // 1
    module_path: String,     // 2
    filesystem_path: String, // 3
    filetype: String,        // 4
    content_hash: String,    // 5
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::content_store)]
struct NewContentStoreRow {
    content_hash: String,
    content: Vec<u8>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbols)]
struct NewSymbol {
    id: i64,
    name: String,
    // Pre-computed in Rust; trigger only fires on UPDATE OF name now.
    // serialize_as wraps the String in LtreeValue (which implements AsExpression<Ltree>)
    // since String cannot directly implement AsExpression<Ltree> due to orphan rules.
    #[diesel(serialize_as = LtreeValue)]
    symbol_path: String,
    project_id: i32,
    symbol_type: i32,
    symbol_scope: Option<i32>,
    leaf_name: String, // pre-computed in Rust; trigger only fires on UPDATE OF name
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbol_instances)]
struct NewSymbolInstance {
    symbol: i64,
    object_id: i32,
    offset_range: std::ops::Range<i32>,
    instance_type: i32,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbol_refs)]
struct NewSymbolRef {
    to_symbol: i64,
    from_object: i32,
    from_offset_range: std::ops::Range<i32>,
}

#[derive(Debug, QueryableByName)]
struct DirectoryChildRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    path: String,
    #[diesel(sql_type = diesel::sql_types::Bool)]
    has_children: bool,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    compact_path: Option<String>,
}

/// Single name column from a query.
#[derive(Debug, QueryableByName)]
struct NameRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    name: String,
}

/// Generic boolean result from an EXISTS query.
#[derive(Debug, QueryableByName)]
struct ExistsRow {
    #[diesel(sql_type = diesel::sql_types::Bool)]
    exists: bool,
}

/// Per-directory child counts for determining has_children and compact eligibility.
#[derive(Debug, QueryableByName)]
struct ChildCountsRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    dir_count: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    file_count: i64,
}

#[derive(Debug, QueryableByName)]
struct FileChildRow {
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
    #[diesel(sql_type = diesel::sql_types::Text)]
    path: String,
    #[diesel(sql_type = diesel::sql_types::Text)]
    filetype: String,
}

impl From<diesel::result::Error> for UploadError {
    fn from(err: diesel::result::Error) -> Self {
        UploadError::Storage(err.to_string())
    }
}

impl From<diesel::result::Error> for StoreError {
    fn from(err: diesel::result::Error) -> Self {
        StoreError::Storage(err.to_string())
    }
}

impl IndexStore {
    pub fn from_pool(pool: Pool<AsyncPgConnection>) -> Self {
        Self { pool }
    }

    async fn get_conn(&self) -> Result<bb8::PooledConnection<'_, AsyncPgConnection>, StoreError> {
        self.pool
            .get()
            .await
            .map_err(|err| StoreError::Storage(err.to_string()))
    }

    async fn get_upload_conn(
        &self,
    ) -> Result<bb8::PooledConnection<'_, AsyncPgConnection>, UploadError> {
        self.pool
            .get()
            .await
            .map_err(|err| UploadError::Storage(err.to_string()))
    }
}

fn normalize_posix(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize_full_path(path: &str) -> String {
    let normalized = normalize_posix(path);
    let has_leading = normalized.starts_with('/');
    let mut stack: Vec<&str> = Vec::new();
    for part in normalized.split('/') {
        match part {
            "" | "." => continue,
            ".." => {
                stack.pop();
            }
            _ => stack.push(part),
        }
    }
    let mut result = stack.join("/");
    if has_leading {
        result.insert(0, '/');
    }
    if result.is_empty() {
        result.push('/');
    }
    result
}

fn path_basename(path: &str) -> String {
    let normalized = normalize_full_path(path);
    if normalized == "/" {
        return "/".to_string();
    }
    normalized
        .trim_end_matches('/')
        .rsplit('/')
        .next()
        .unwrap_or("/")
        .to_string()
}

fn hash_bytes(bytes: &[u8]) -> String {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    let digest = hasher.finalize();
    let mut out = String::with_capacity(digest.len() * 2);
    for byte in digest {
        use std::fmt::Write as _;
        let _ = write!(&mut out, "{:02x}", byte);
    }
    out
}
