use std::collections::{HashMap, HashSet};

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::task;

use crate::proto::askl::index::{Object as UploadObject, Project as UploadProject, Symbol as UploadSymbol};
use index::schema_diesel as index_schema;
use index::symbols::FileId;

const MAX_INSERT_ROWS: usize = 1000;

#[derive(Clone)]
pub struct IndexStore {
    pool: Pool<ConnectionManager<PgConnection>>,
}

#[derive(Debug)]
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
}

#[derive(Debug, Serialize)]
pub struct ProjectDetails {
    pub id: i32,
    pub project_name: String,
    pub root_path: String,
    pub file_count: i64,
    pub symbol_count: i64,
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
    NotDirectory,
    Nodes(Vec<ProjectTreeNode>),
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::projects)]
struct NewProject {
    project_name: String,
    root_path: String,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::objects)]
struct NewObject {
    project_id: i32,
    // directory_id removed - directories are now symbols
    module_path: String,
    filesystem_path: String,
    filetype: String,
    content_hash: String,
    // Directory sentinel objects have:
    // - filesystem_path = directory path (e.g., "/src")
    // - filetype = "directory"
    // - content_hash = "" (empty)
}

// NewDirectory removed - directories are now symbols

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::object_contents)]
struct NewObjectContent {
    object_id: i32,
    content: Vec<u8>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbols)]
struct NewSymbol {
    name: String,
    project_id: i32,
    symbol_type: i32,
    symbol_scope: Option<i32>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbol_instances)]
struct NewSymbolInstance {
    symbol: i32,
    object_id: i32,
    offset_range: std::ops::Range<i32>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbol_refs)]
struct NewSymbolRef {
    to_symbol: i32,
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

// DirectoryPathRow removed - directories are now symbols

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
    pub fn from_pool(pool: Pool<ConnectionManager<PgConnection>>) -> Self {
        Self { pool }
    }

    pub async fn upload_index(&self, upload: UploadProject) -> Result<i32, UploadError> {
        let pool = self.pool.clone();
        task::spawn_blocking(move || {
            let _upload_span: tracing::span::EnteredSpan =
                tracing::info_span!("index_upload_store").entered();
            let mut conn = pool
                .get()
                .map_err(|err| UploadError::Storage(err.to_string()))?;
            conn.transaction::<_, UploadError, _>(|conn| {
                let _txn_span: tracing::span::EnteredSpan =
                    tracing::info_span!("index_upload_txn").entered();
                let project_name = upload.project_name.trim();
                if project_name.is_empty() {
                    return Err(UploadError::Invalid("project_name is required".to_string()));
                }

                let root_path = upload.root_path.trim();
                if root_path.is_empty() {
                    return Err(UploadError::Invalid("root_path is required".to_string()));
                }
                if !root_path.starts_with('/') {
                    return Err(UploadError::Invalid(
                        "root_path must be an absolute path".to_string(),
                    ));
                }

                let project_id: Option<i32> = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("insert_project").entered();
                    diesel::insert_into(index_schema::projects::table)
                        .values(NewProject {
                            project_name: project_name.to_string(),
                            root_path: root_path.to_string(),
                        })
                        .on_conflict(index_schema::projects::project_name)
                        .do_nothing()
                        .returning(index_schema::projects::id)
                        .get_result(conn)
                        .optional()?
                };

                let project_id = match project_id {
                    Some(id) => id,
                    None => return Err(UploadError::Conflict),
                };

                let object_inserts = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "build_objects",
                        count = upload.objects.len()
                    )
                    .entered();
                    build_objects(project_id, &upload.objects)?
                };
                let object_map = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_objects",
                        count = object_inserts.len()
                    )
                    .entered();
                    insert_objects(conn, &object_inserts)?
                };

                let symbol_inserts = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "build_symbols",
                        count = upload.symbols.len()
                    )
                    .entered();
                    build_symbols(project_id, &upload.symbols)?
                };
                let symbol_map = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_symbols",
                        count = symbol_inserts.len()
                    )
                    .entered();
                    insert_symbols(conn, symbol_inserts)?
                };

                let symbol_instance_rows = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("build_symbol_instances").entered();
                    build_symbol_instances(&upload.objects, &object_map, &symbol_map)?
                };
                {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_symbol_instances",
                        count = symbol_instance_rows.len()
                    )
                    .entered();
                    insert_symbol_instances(conn, &symbol_instance_rows)?;
                }

                let symbol_ref_rows = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("build_symbol_refs").entered();
                    build_symbol_refs(&upload.objects, &object_map, &symbol_map)?
                };
                {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_symbol_refs",
                        count = symbol_ref_rows.len()
                    )
                    .entered();
                    insert_symbol_refs(conn, &symbol_ref_rows)?;
                }

                Ok(project_id)
            })
        })
        .await
        .map_err(|err| UploadError::Storage(err.to_string()))?
    }

    pub async fn list_projects(&self) -> Result<Vec<ProjectInfo>, StoreError> {
        let pool = self.pool.clone();
        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| StoreError::Storage(err.to_string()))?;
            let rows: Vec<(i32, String, String)> = index_schema::projects::table
                .select((
                    index_schema::projects::id,
                    index_schema::projects::project_name,
                    index_schema::projects::root_path,
                ))
                .order(index_schema::projects::id)
                .load(&mut conn)?;
            Ok(rows
                .into_iter()
                .map(|(id, project_name, root_path)| ProjectInfo {
                    id,
                    project_name,
                    root_path,
                })
                .collect())
        })
        .await
        .map_err(|err| StoreError::Storage(err.to_string()))?
    }

    pub async fn get_project_details(
        &self,
        project_id: i32,
    ) -> Result<Option<ProjectDetails>, StoreError> {
        let pool = self.pool.clone();
        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| StoreError::Storage(err.to_string()))?;

            let project_row: Option<(i32, String, String)> = index_schema::projects::table
                .filter(index_schema::projects::id.eq(project_id))
                .select((
                    index_schema::projects::id,
                    index_schema::projects::project_name,
                    index_schema::projects::root_path,
                ))
                .first(&mut conn)
                .optional()?;

            let (id, project_name, root_path) = match project_row {
                Some(row) => row,
                None => return Ok(None),
            };

            let file_count: i64 = index_schema::objects::table
                .filter(index_schema::objects::project_id.eq(project_id))
                .count()
                .get_result(&mut conn)?;

            let symbol_count: i64 = index_schema::symbols::table
                .filter(index_schema::symbols::project_id.eq(project_id))
                .count()
                .get_result(&mut conn)?;

            Ok(Some(ProjectDetails {
                id,
                project_name,
                root_path,
                file_count,
                symbol_count,
            }))
        })
        .await
        .map_err(|err| StoreError::Storage(err.to_string()))?
    }

    pub async fn delete_project(&self, project_id: i32) -> Result<bool, StoreError> {
        let pool = self.pool.clone();
        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| StoreError::Storage(err.to_string()))?;
            let deleted = diesel::delete(
                index_schema::projects::table.filter(index_schema::projects::id.eq(project_id)),
            )
            .execute(&mut conn)?;
            Ok(deleted > 0)
        })
        .await
        .map_err(|err| StoreError::Storage(err.to_string()))?
    }

    pub async fn list_project_tree(
        &self,
        project_id: i32,
        path: &str,
        compact: bool,
    ) -> Result<ProjectTreeResult, StoreError> {
        let pool = self.pool.clone();
        let path = path.to_string();
        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| StoreError::Storage(err.to_string()))?;

            let exists = index_schema::projects::table
                .filter(index_schema::projects::id.eq(project_id))
                .select(index_schema::projects::id)
                .first::<i32>(&mut conn)
                .optional()?;
            if exists.is_none() {
                return Ok(ProjectTreeResult::ProjectNotFound);
            }

            let normalized = normalize_full_path(&path);

            // Find directory symbol for the requested path
            let dir_symbol = index_schema::symbols::table
                .filter(index_schema::symbols::project_id.eq(project_id))
                .filter(index_schema::symbols::symbol_type.eq(4)) // DIRECTORY
                .filter(index_schema::symbols::name.eq(&normalized))
                .select(index_schema::symbols::id)
                .first::<i32>(&mut conn)
                .optional()?;

            // If path is "/" and no root directory symbol exists yet, that's OK
            // (empty project or not yet indexed)
            if dir_symbol.is_none() && normalized != "/" {
                return Ok(ProjectTreeResult::NotDirectory);
            }

            let (directories, files) = load_tree_children(
                &mut conn,
                project_id,
                &normalized,
                compact,
            )?;

            let mut nodes = Vec::with_capacity(directories.len() + files.len());
            for row in directories {
                let name = path_basename(&row.path);
                nodes.push(ProjectTreeNode {
                    name,
                    path: row.path,
                    node_type: "dir".to_string(),
                    has_children: row.has_children,
                    file_id: None,
                    filetype: None,
                    compact_path: row.compact_path,
                });
            }

            for row in files {
                let name = path_basename(&row.path);
                nodes.push(ProjectTreeNode {
                    name,
                    path: row.path,
                    node_type: "file".to_string(),
                    has_children: false,
                    file_id: Some(FileId::new(row.id)),
                    filetype: Some(row.filetype),
                    compact_path: None,
                });
            }

            nodes.sort_by(|a, b| {
                let a_is_dir = a.node_type == "dir";
                let b_is_dir = b.node_type == "dir";
                b_is_dir.cmp(&a_is_dir).then_with(|| a.path.cmp(&b.path))
            });
            Ok(ProjectTreeResult::Nodes(nodes))
        })
        .await
        .map_err(|err| StoreError::Storage(err.to_string()))?
    }

    pub async fn get_project_file_contents_by_path(
        &self,
        project_id: i32,
        path: &str,
    ) -> Result<Option<Vec<u8>>, StoreError> {
        let pool = self.pool.clone();
        let path = path.to_string();
        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| StoreError::Storage(err.to_string()))?;

            let normalized = normalize_full_path(&path);
            let content = index_schema::object_contents::table
                .inner_join(
                    index_schema::objects::table
                        .on(index_schema::object_contents::object_id.eq(index_schema::objects::id)),
                )
                .filter(index_schema::objects::project_id.eq(project_id))
                .filter(index_schema::objects::filesystem_path.eq(normalized))
                .select(index_schema::object_contents::content)
                .first::<Vec<u8>>(&mut conn)
                .optional()?;

            Ok(content)
        })
        .await
        .map_err(|err| StoreError::Storage(err.to_string()))?
    }
}

/// Load direct child directories and files for a given parent path.
/// Uses individual per-directory queries with static LIKE prefixes so that
/// PostgreSQL uses the fast composite btree index (project_id, symbol_type,
/// name text_pattern_ops) instead of falling back to the slow trigram index.
fn load_tree_children(
    conn: &mut PgConnection,
    project_id: i32,
    parent_path: &str,
    compact: bool,
) -> Result<(Vec<DirectoryChildRow>, Vec<FileChildRow>), StoreError> {
    let prefix = if parent_path == "/" {
        "/".to_string()
    } else {
        format!("{}/", parent_path)
    };

    // 1. Get direct child directories (depth-1 under parent).
    //    Uses composite index via static LIKE $2||'%'.
    let child_dir_names: Vec<String> = diesel::sql_query(
        r#"
        SELECT s.name
        FROM index.symbols s
        WHERE s.project_id = $1
          AND s.symbol_type = 4
          AND s.name LIKE $2 || '%'
          AND s.name != $2
          AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
        ORDER BY s.name
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(&prefix)
    .load::<NameRow>(conn)?
    .into_iter()
    .map(|r| r.name)
    .collect();

    // 2. For each child dir, get child counts with individual queries.
    //    Each query uses the composite index (~0.1ms each).
    //    Counts enable both has_children and compact eligibility checks.
    let mut dir_children = Vec::with_capacity(child_dir_names.len());
    for dir_name in &child_dir_names {
        let child_prefix = format!("{}/", dir_name);
        let counts = query_child_counts(conn, project_id, &child_prefix)?;
        let has_children = counts.dir_count > 0 || counts.file_count > 0;

        // Only compute compact path for eligible dirs (exactly 1 subdir, 0 files).
        let compact_path = if compact && counts.dir_count == 1 && counts.file_count == 0 {
            compute_compact_path(conn, project_id, &dir_name)?
        } else {
            None
        };

        dir_children.push(DirectoryChildRow {
            path: dir_name.clone(),
            has_children,
            compact_path,
        });
    }

    // 4. Get direct child files.
    let files = load_file_children(conn, project_id, &prefix)?;

    Ok((dir_children, files))
}

/// Query direct child dir count and file count under a prefix.
fn query_child_counts(
    conn: &mut PgConnection,
    project_id: i32,
    child_prefix: &str,
) -> Result<ChildCountsRow, StoreError> {
    let row = diesel::sql_query(
        r#"
        SELECT
            (SELECT COUNT(*) FROM index.symbols s
             WHERE s.project_id = $1 AND s.symbol_type = 4
               AND s.name LIKE $2 || '%'
               AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            ) AS dir_count,
            (SELECT COUNT(*) FROM index.symbols s
             WHERE s.project_id = $1 AND s.symbol_type = 2
               AND s.name LIKE $2 || '%'
               AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            ) AS file_count
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(child_prefix)
    .get_result::<ChildCountsRow>(conn)?;
    Ok(row)
}

/// Walk down a chain of single-child-no-files directories for compact display.
/// Returns the deepest directory path in the chain, or None if no compaction.
/// Caller should only invoke this for dirs with exactly 1 subdir and 0 files.
fn compute_compact_path(
    conn: &mut PgConnection,
    project_id: i32,
    dir_path: &str,
) -> Result<Option<String>, StoreError> {
    let mut current = dir_path.to_string();
    for _ in 0..20 {
        let child_prefix = format!("{}/", current);

        // Get up to 2 child dirs — we only need to know if there's exactly 1.
        let child_dirs: Vec<NameRow> = diesel::sql_query(
            r#"
            SELECT s.name
            FROM index.symbols s
            WHERE s.project_id = $1
              AND s.symbol_type = 4
              AND s.name LIKE $2 || '%'
              AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            LIMIT 2
            "#,
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Text, _>(&child_prefix)
        .load(conn)?;

        if child_dirs.len() != 1 {
            break;
        }

        // Check for any direct child files
        let has_files = diesel::sql_query(
            r#"
            SELECT EXISTS(
                SELECT 1 FROM index.symbols s
                WHERE s.project_id = $1
                  AND s.symbol_type = 2
                  AND s.name LIKE $2 || '%'
                  AND position('/' IN substring(s.name FROM length($2) + 1)) = 0
            ) AS exists
            "#,
        )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Text, _>(&child_prefix)
        .get_result::<ExistsRow>(conn)?;

        let single_child = child_dirs.into_iter().next().unwrap().name;
        if has_files.exists {
            // Has files — stop compaction but include this dir
            current = single_child;
            break;
        }
        current = single_child;
    }

    if current != dir_path {
        Ok(Some(current))
    } else {
        Ok(None)
    }
}

/// Load direct child files under a parent prefix.
fn load_file_children(
    conn: &mut PgConnection,
    project_id: i32,
    parent_prefix: &str,
) -> Result<Vec<FileChildRow>, StoreError> {
    let rows = diesel::sql_query(
        r#"
        SELECT DISTINCT o.id, o.filesystem_path AS path, o.filetype
        FROM index.objects o
        JOIN index.symbols fs ON fs.name = o.filesystem_path
        WHERE fs.project_id = $1
          AND fs.symbol_type = 2
          AND fs.name LIKE $2 || '%'
          AND position('/' IN substring(fs.name FROM length($2) + 1)) = 0
        ORDER BY o.filesystem_path
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(parent_prefix)
    .load::<FileChildRow>(conn)?;
    Ok(rows)
}

fn normalize_posix(path: &str) -> String {
    path.replace('\\', "/")
}

fn normalize_full_path(path: &str) -> String {
    let mut normalized = normalize_posix(path);
    let has_leading = normalized.starts_with('/');
    let parts: Vec<&str> = normalized.split('/').filter(|p| !p.is_empty()).collect();
    normalized = parts.join("/");
    if has_leading {
        normalized.insert(0, '/');
    }
    if normalized.is_empty() && has_leading {
        normalized.push('/');
    }
    normalized
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

/// Validates proto symbol type against known symbol type constants.
fn validate_symbol_type(proto_type: i32) -> Result<i32, UploadError> {
    const VALID_TYPES: &[i32] = &[
        index::db_diesel::SYMBOL_TYPE_FUNCTION,
        index::db_diesel::SYMBOL_TYPE_FILE,
        index::db_diesel::SYMBOL_TYPE_MODULE,
        index::db_diesel::SYMBOL_TYPE_DIRECTORY,
        index::db_diesel::SYMBOL_TYPE_TYPE,
    ];
    if VALID_TYPES.contains(&proto_type) {
        Ok(proto_type)
    } else {
        Err(UploadError::Invalid(format!(
            "invalid symbol type {}",
            proto_type
        )))
    }
}

struct ObjectInsert {
    local_id: i64,
    content: Vec<u8>,
    row: NewObject,
}

struct SymbolInsert {
    local_id: i64,
    row: NewSymbol,
}

// Directory symbols are now created by the indexer, not the server.

fn build_objects(
    project_id: i32,
    objects: &[UploadObject],
) -> Result<Vec<ObjectInsert>, UploadError> {
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for object in objects {
        if !seen.insert(object.local_id) {
            return Err(UploadError::Invalid(format!(
                "duplicate object local_id {}",
                object.local_id
            )));
        }
        let filesystem_path_raw = object.filesystem_path.trim();
        if filesystem_path_raw.is_empty() {
            return Err(UploadError::Invalid(format!(
                "filesystem_path is required for object {}",
                object.local_id
            )));
        }
        if !filesystem_path_raw.starts_with('/') {
            return Err(UploadError::Invalid(format!(
                "filesystem_path must be an absolute path for object {}",
                object.local_id
            )));
        }
        let filesystem_path = normalize_full_path(filesystem_path_raw);
        inserts.push(ObjectInsert {
            local_id: object.local_id,
            content: object.content.clone(),
            row: NewObject {
                project_id,
                module_path: object.module_path.clone(),
                filesystem_path,
                filetype: object.filetype.clone(),
                content_hash: hash_bytes(&object.content),
            },
        });
    }
    Ok(inserts)
}

fn insert_objects(
    conn: &mut PgConnection,
    inserts: &[ObjectInsert],
) -> Result<HashMap<i64, i32>, UploadError> {
    if inserts.is_empty() {
        return Ok(HashMap::new());
    }

    let mut object_map = HashMap::new();
    for chunk in inserts.chunks(MAX_INSERT_ROWS) {
        let rows: Vec<NewObject> = chunk.iter().map(|entry| entry.row.clone()).collect();
        let ids: Vec<i32> = diesel::insert_into(index_schema::objects::table)
            .values(&rows)
            .returning(index_schema::objects::id)
            .get_results(conn)?;

        let mut object_contents = Vec::with_capacity(ids.len());
        for (entry, id) in chunk.iter().zip(ids.iter()) {
            object_map.insert(entry.local_id, *id);
            object_contents.push(NewObjectContent {
                object_id: *id,
                content: entry.content.clone(),
            });
        }

        diesel::insert_into(index_schema::object_contents::table)
            .values(&object_contents)
            .execute(conn)?;
    }

    Ok(object_map)
}

fn build_symbols(
    project_id: i32,
    symbols: &[UploadSymbol],
) -> Result<Vec<SymbolInsert>, UploadError> {
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for symbol in symbols {
        if !seen.insert(symbol.local_id) {
            return Err(UploadError::Invalid(format!(
                "duplicate symbol local_id {}",
                symbol.local_id
            )));
        }
        let symbol_type = validate_symbol_type(symbol.r#type)?;
        // symbol_scope is only meaningful for function types
        let symbol_scope = if symbol.scope != 0 {
            Some(symbol.scope)
        } else {
            None
        };
        inserts.push(SymbolInsert {
            local_id: symbol.local_id,
            row: NewSymbol {
                name: symbol.name.clone(),
                project_id,
                symbol_type,
                symbol_scope,
            },
        });
    }
    Ok(inserts)
}

fn insert_symbols(
    conn: &mut PgConnection,
    inserts: Vec<SymbolInsert>,
) -> Result<HashMap<i64, i32>, UploadError> {
    if inserts.is_empty() {
        return Ok(HashMap::new());
    }

    let mut symbol_map = HashMap::new();
    for chunk in inserts.chunks(MAX_INSERT_ROWS) {
        let rows: Vec<NewSymbol> = chunk.iter().map(|entry| entry.row.clone()).collect();
        let ids: Vec<i32> = diesel::insert_into(index_schema::symbols::table)
            .values(&rows)
            .returning(index_schema::symbols::id)
            .get_results(conn)?;
        for (entry, id) in chunk.iter().zip(ids) {
            symbol_map.insert(entry.local_id, id);
        }
    }

    Ok(symbol_map)
}

fn build_symbol_instances(
    objects: &[UploadObject],
    object_map: &HashMap<i64, i32>,
    symbol_map: &HashMap<i64, i32>,
) -> Result<Vec<NewSymbolInstance>, UploadError> {
    let mut rows = Vec::new();
    for object in objects {
        let object_id = object_map.get(&object.local_id).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing object mapping for local_id {}",
                object.local_id
            ))
        })?;
        for instance in &object.symbol_instances {
            let symbol_id = symbol_map
                .get(&instance.symbol_local_id)
                .ok_or_else(|| {
                    UploadError::Invalid(format!(
                        "unknown symbol local_id {}",
                        instance.symbol_local_id
                    ))
                })?;
            rows.push(NewSymbolInstance {
                symbol: *symbol_id,
                object_id: *object_id,
                offset_range: instance.start_offset..instance.end_offset,
            });
        }
    }
    Ok(rows)
}

fn build_symbol_refs(
    objects: &[UploadObject],
    object_map: &HashMap<i64, i32>,
    symbol_map: &HashMap<i64, i32>,
) -> Result<Vec<NewSymbolRef>, UploadError> {
    let mut rows = Vec::new();
    for object in objects {
        let object_id = object_map.get(&object.local_id).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing object mapping for local_id {}",
                object.local_id
            ))
        })?;
        for reference in &object.refs {
            let symbol_id = symbol_map
                .get(&reference.to_symbol_local_id)
                .ok_or_else(|| {
                    UploadError::Invalid(format!(
                        "unknown symbol local_id {}",
                        reference.to_symbol_local_id
                    ))
                })?;
            rows.push(NewSymbolRef {
                to_symbol: *symbol_id,
                from_object: *object_id,
                from_offset_range: reference.from_offset_start..reference.from_offset_end,
            });
        }
    }
    Ok(rows)
}

fn insert_symbol_instances(
    conn: &mut PgConnection,
    rows: &[NewSymbolInstance],
) -> Result<(), UploadError> {
    for chunk in rows.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::symbol_instances::table)
            .values(chunk)
            .execute(conn)?;
    }
    Ok(())
}

fn insert_symbol_refs(conn: &mut PgConnection, rows: &[NewSymbolRef]) -> Result<(), UploadError> {
    for chunk in rows.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::symbol_refs::table)
            .values(chunk)
            .execute(conn)?;
    }
    Ok(())
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
