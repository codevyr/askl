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

#[derive(Debug, QueryableByName)]
struct DirectoryChildStatsRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    path: String,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    child_dir_count: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    file_count: i64,
}

#[derive(Debug, QueryableByName)]
struct DirectoryWalkRow {
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    child_dir_count: i64,
    #[diesel(sql_type = diesel::sql_types::BigInt)]
    file_count: i64,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    child_path: Option<String>,
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

                // Directory paths are collected for creating directory symbols later
                let directory_paths = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("collect_directory_paths").entered();
                    collect_directory_paths(&upload.objects)
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

                // Create directory symbols and their instances
                {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "create_directory_symbols",
                        count = directory_paths.len()
                    )
                    .entered();
                    create_directory_symbols(conn, project_id, &object_inserts, &object_map)?;
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

            let directories = load_directory_children_with_compact(
                &mut conn,
                project_id,
                &normalized,
                compact,
            )?;
            let files = load_file_children(&mut conn, project_id, &normalized)?;

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
                    compact_path: if compact { row.compact_path } else { None },
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

fn load_directory_children_with_compact(
    conn: &mut PgConnection,
    project_id: i32,
    parent_path: &str,
    compact: bool,
) -> Result<Vec<DirectoryChildRow>, StoreError> {
    // Find child directories: directories whose name starts with parent_path
    // and have exactly one more path component than parent_path
    //
    // Example: If parent_path is "/src", child directories are:
    // - /src/lib (depth 2 when parent is depth 1)
    // - /src/util (depth 2)
    // But NOT /src/lib/sub (depth 3)
    //
    // Query: find directory symbols that:
    // 1. Belong to this project
    // 2. Have type DIRECTORY (4)
    // 3. Name starts with parent_path (or equals "/" for root children)
    // 4. Have depth = parent_depth + 1
    // Find direct child directories: starts_with(name, parent || '/') and no extra slash
    let rows = diesel::sql_query(
        r#"
        WITH child_dirs AS (
            SELECT s.id, s.name as path
            FROM index.symbols s
            WHERE s.project_id = $1
              AND s.symbol_type = 4
              AND starts_with(s.name, $2)
              AND s.name != $2
              AND (
                  CASE WHEN $2 = '/' THEN
                      position('/' IN substring(s.name FROM 2)) = 0
                  ELSE
                      position('/' IN substring(s.name FROM length($2) + 2)) = 0
                  END
              )
        ),
        grandchild_dirs AS (
            SELECT cd.id AS parent_id, COUNT(DISTINCT s2.id) AS child_dir_count
            FROM child_dirs cd
            JOIN index.symbols s2 ON s2.project_id = $1
                AND s2.symbol_type = 4
                AND starts_with(s2.name, cd.path || '/')
                AND position('/' IN substring(s2.name FROM length(cd.path) + 2)) = 0
            GROUP BY cd.id
        ),
        child_files AS (
            SELECT cd.id AS parent_id, COUNT(DISTINCT o.id) AS file_count
            FROM child_dirs cd
            JOIN index.symbols fs ON fs.project_id = $1
                AND fs.symbol_type = 2
                AND starts_with(fs.name, cd.path || '/')
                AND position('/' IN substring(fs.name FROM length(cd.path) + 2)) = 0
            JOIN index.symbol_instances si ON si.symbol = fs.id
            JOIN index.objects o ON o.id = si.object_id
            GROUP BY cd.id
        )
        SELECT cd.path,
            COALESCE(gd.child_dir_count, 0) AS child_dir_count,
            COALESCE(cf.file_count, 0) AS file_count
        FROM child_dirs cd
        LEFT JOIN grandchild_dirs gd ON gd.parent_id = cd.id
        LEFT JOIN child_files cf ON cf.parent_id = cd.id
        ORDER BY cd.path
        "#,
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Text, _>(if parent_path == "/" {
        "/".to_string()
    } else {
        format!("{}/", parent_path)
    })
    .load::<DirectoryChildStatsRow>(conn)?;

    let mut children = Vec::with_capacity(rows.len());
    for row in rows {
        let has_children = row.child_dir_count > 0 || row.file_count > 0;
        let compact_path = if compact && row.file_count == 0 && row.child_dir_count == 1 {
            compute_compact_path(conn, project_id, &row.path)?
        } else {
            None
        };
        children.push(DirectoryChildRow {
            path: row.path,
            has_children,
            compact_path,
        });
    }

    Ok(children)
}

fn compute_compact_path(
    conn: &mut PgConnection,
    project_id: i32,
    start_path: &str,
) -> Result<Option<String>, StoreError> {
    let mut current_path = start_path.to_string();
    let mut last_path = None;
    loop {
        let row = load_directory_walk_row(conn, project_id, &current_path)?;
        if row.file_count != 0 || row.child_dir_count != 1 {
            break;
        }
        let child_path = match row.child_path {
            Some(path) => path,
            None => break,
        };
        last_path = Some(child_path.clone());
        current_path = child_path;
    }
    Ok(last_path)
}

fn load_directory_walk_row(
    conn: &mut PgConnection,
    project_id: i32,
    dir_path: &str,
) -> Result<DirectoryWalkRow, StoreError> {
    // Query child directory count and file count for a directory path
    // using symbol-based lookups instead of directories table
    // Note: This function is only called for non-root directories (from compute_compact_path)
    // so we don't need the root '/' special case here
    let query = r#"
        SELECT
            -- Count child directories
            (SELECT COUNT(DISTINCT s.id)
             FROM index.symbols s
             WHERE s.project_id = $1
               AND s.symbol_type = 4
               AND starts_with(s.name, $2 || '/')
               AND position('/' IN substring(s.name FROM length($2) + 2)) = 0
            ) AS child_dir_count,
            -- Count files directly in this directory
            (SELECT COUNT(DISTINCT fs.id)
             FROM index.symbols fs
             WHERE fs.project_id = $1
               AND fs.symbol_type = 2
               AND starts_with(fs.name, $2 || '/')
               AND position('/' IN substring(fs.name FROM length($2) + 2)) = 0
            ) AS file_count,
            -- Get first child directory path
            (SELECT s.name
             FROM index.symbols s
             WHERE s.project_id = $1
               AND s.symbol_type = 4
               AND starts_with(s.name, $2 || '/')
               AND position('/' IN substring(s.name FROM length($2) + 2)) = 0
             ORDER BY s.name
             LIMIT 1
            ) AS child_path
    "#;

    let row = diesel::sql_query(query)
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Text, _>(dir_path)
        .get_result::<DirectoryWalkRow>(conn)?;
    Ok(row)
}

fn load_file_children(
    conn: &mut PgConnection,
    project_id: i32,
    parent_path: &str,
) -> Result<Vec<FileChildRow>, StoreError> {
    // Find files directly in this directory
    // Files are identified by FILE symbols (type=2) whose name is parent_path/filename
    // where filename has no '/' in it
    // Handle root '/' specially: files like '/main.go' have no slash after position 1
    let query = if parent_path == "/" {
        r#"
            SELECT DISTINCT o.id, o.filesystem_path AS path, o.filetype
            FROM index.objects o
            JOIN index.symbols fs ON fs.name = o.filesystem_path
            WHERE fs.project_id = $1
              AND fs.symbol_type = 2
              AND fs.name LIKE '/%'
              AND fs.name != '/'
              AND position('/' IN substring(fs.name FROM 2)) = 0
            ORDER BY o.filesystem_path
        "#
    } else {
        r#"
            SELECT DISTINCT o.id, o.filesystem_path AS path, o.filetype
            FROM index.objects o
            JOIN index.symbols fs ON fs.name = o.filesystem_path
            WHERE fs.project_id = $1
              AND fs.symbol_type = 2
              AND starts_with(fs.name, $2 || '/')
              AND position('/' IN substring(fs.name FROM length($2) + 2)) = 0
            ORDER BY o.filesystem_path
        "#
    };

    let rows = if parent_path == "/" {
        diesel::sql_query(query)
            .bind::<diesel::sql_types::Integer, _>(project_id)
            .load::<FileChildRow>(conn)?
    } else {
        diesel::sql_query(query)
            .bind::<diesel::sql_types::Integer, _>(project_id)
            .bind::<diesel::sql_types::Text, _>(parent_path)
            .load::<FileChildRow>(conn)?
    };
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

/// Validates proto symbol type and returns it as-is since proto enum values
/// match database IDs: FUNCTION=1, FILE=2, MODULE=3, DIRECTORY=4
fn validate_symbol_type(proto_type: i32) -> Result<i32, UploadError> {
    match proto_type {
        1..=4 => Ok(proto_type),
        _ => Err(UploadError::Invalid(format!(
            "invalid symbol type {}",
            proto_type
        ))),
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

fn collect_directory_paths(objects: &[UploadObject]) -> HashSet<String> {
    let mut paths = HashSet::new();
    paths.insert("/".to_string());
    for object in objects {
        let filesystem_path = normalize_full_path(&object.filesystem_path);
        let mut dir_path = parent_dir(&filesystem_path);
        loop {
            paths.insert(dir_path.clone());
            if dir_path == "/" {
                break;
            }
            dir_path = parent_dir(&dir_path);
        }
    }
    paths
}

fn parent_dir(path: &str) -> String {
    let normalized = normalize_full_path(path);
    if normalized == "/" {
        return "/".to_string();
    }
    let trimmed = normalized.trim_end_matches('/');
    match trimmed.rfind('/') {
        Some(0) | None => "/".to_string(),
        Some(idx) => trimmed[..idx].to_string(),
    }
}

// insert_directories, insert_directory_batch, fetch_directory_ids, DirectoryEntry
// have been removed - directories are now created as symbols via create_directory_symbols()

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

/// Creates directory symbols, instances, and parent→child directory refs.
///
/// Each directory symbol gets:
/// - An instance on each direct child file's object (for range-based containment)
/// - A symbol_ref from each direct child directory (for directory hierarchy traversal)
///
/// Containment (via @has) uses strict type level comparison (>), so:
///   directory(4) > module(3) > file(2) > function(1)
/// Directory→directory hierarchy uses symbol_refs (via @refs).
fn create_directory_symbols(
    conn: &mut PgConnection,
    project_id: i32,
    object_inserts: &[ObjectInsert],
    object_map: &HashMap<i64, i32>,
) -> Result<(), UploadError> {
    // Step 1: Collect all directory paths (including all ancestors)
    let mut dir_paths: HashSet<String> = HashSet::new();
    for obj in object_inserts {
        let path = &obj.row.filesystem_path;
        let mut current = parent_dir(path);
        loop {
            dir_paths.insert(normalize_full_path(&current));
            if current == "/" {
                break;
            }
            current = parent_dir(&current);
        }
    }

    if dir_paths.is_empty() {
        return Ok(());
    }

    // Step 2: Create directory symbols
    let dir_symbols: Vec<NewSymbol> = dir_paths
        .iter()
        .map(|path| NewSymbol {
            name: path.clone(),
            project_id,
            symbol_type: 4, // DIRECTORY
            symbol_scope: None,
        })
        .collect();

    let mut dir_symbol_map: HashMap<String, i32> = HashMap::new();
    for chunk in dir_symbols.chunks(MAX_INSERT_ROWS) {
        let rows: Vec<NewSymbol> = chunk.to_vec();
        let ids: Vec<i32> = diesel::insert_into(index_schema::symbols::table)
            .values(&rows)
            .returning(index_schema::symbols::id)
            .get_results(conn)?;

        for (symbol, id) in chunk.iter().zip(ids) {
            dir_symbol_map.insert(symbol.name.clone(), id);
        }
    }

    // Step 3: Create a sentinel object per directory and a self-instance on it.
    // This ensures every directory is visible to the query engine regardless of
    // whether it contains direct files.
    let mut dir_objects: Vec<NewObject> = Vec::new();
    for dir_path in &dir_paths {
        dir_objects.push(NewObject {
            project_id,
            module_path: dir_path.clone(),
            filesystem_path: dir_path.clone(),
            filetype: "directory".to_string(),
            content_hash: String::new(),
        });
    }

    // Insert sentinel objects and build dir_path → db_object_id mapping
    let mut dir_object_map: HashMap<String, i32> = HashMap::new();
    for chunk in dir_objects.chunks(MAX_INSERT_ROWS) {
        let ids: Vec<i32> = diesel::insert_into(index_schema::objects::table)
            .values(chunk)
            .returning(index_schema::objects::id)
            .get_results(conn)?;
        for (obj, id) in chunk.iter().zip(ids) {
            dir_object_map.insert(obj.filesystem_path.clone(), id);
        }
    }

    // Create self-instances: one per directory on its sentinel object, range [0, 0)
    let mut instances: Vec<NewSymbolInstance> = Vec::new();
    for dir_path in &dir_paths {
        if let (Some(&symbol_id), Some(&object_id)) =
            (dir_symbol_map.get(dir_path), dir_object_map.get(dir_path))
        {
            instances.push(NewSymbolInstance {
                symbol: symbol_id,
                object_id,
                offset_range: 0..0,
            });
        }
    }

    // Also create directory instances on direct child files for containment queries.
    // These allow @has to find files/functions inside a directory via range overlap.
    for obj in object_inserts {
        let db_object_id = *object_map.get(&obj.local_id).ok_or_else(|| {
            UploadError::Invalid(format!("Missing object mapping for local_id {}", obj.local_id))
        })?;

        let content_len = obj.content.len() as i32;
        let file_path = &obj.row.filesystem_path;
        let parent = normalize_full_path(&parent_dir(file_path));

        if let Some(&symbol_id) = dir_symbol_map.get(&parent) {
            instances.push(NewSymbolInstance {
                symbol: symbol_id,
                object_id: db_object_id,
                offset_range: 0..content_len,
            });
        }
    }

    for chunk in instances.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::symbol_instances::table)
            .values(chunk)
            .execute(conn)?;
    }

    // Step 4: Create symbol_refs for parent→child directory relationships.
    // Each ref uses the parent's sentinel object as from_object.
    let mut refs: Vec<NewSymbolRef> = Vec::new();

    for parent_dir in &dir_paths {
        let from_object = match dir_object_map.get(parent_dir) {
            Some(&id) => id,
            None => continue,
        };

        for child_dir in &dir_paths {
            if child_dir != parent_dir && is_direct_child_path(parent_dir, child_dir) {
                if let Some(&child_symbol_id) = dir_symbol_map.get(child_dir) {
                    refs.push(NewSymbolRef {
                        to_symbol: child_symbol_id,
                        from_object,
                        from_offset_range: 0..0,
                    });
                }
            }
        }
    }

    insert_symbol_refs(conn, &refs)?;

    Ok(())
}

/// Returns true if child_path is a direct child of parent_path.
/// E.g., is_direct_child_path("/src", "/src/util") => true
///       is_direct_child_path("/src", "/src/util/foo") => false
fn is_direct_child_path(parent: &str, child: &str) -> bool {
    if parent == "/" {
        // Root case: child must be like "/xxx" with no more slashes
        if !child.starts_with('/') || child == "/" {
            return false;
        }
        let after_root = &child[1..];
        !after_root.contains('/')
    } else {
        // Non-root case: child must start with parent + "/"
        let prefix = format!("{}/", parent);
        if !child.starts_with(&prefix) {
            return false;
        }
        let after_prefix = &child[prefix.len()..];
        !after_prefix.contains('/')
    }
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
