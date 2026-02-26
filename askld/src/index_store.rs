use std::collections::{HashMap, HashSet};

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::task;

use crate::proto::askl::index::{
    File as UploadFile, Module as UploadModule, Project as UploadProject,
};
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
pub struct ProjectModule {
    pub id: i32,
    pub module_name: String,
}

#[derive(Debug, Serialize)]
pub struct ProjectDetails {
    pub id: i32,
    pub project_name: String,
    pub root_path: String,
    pub modules: Vec<ProjectModule>,
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
#[diesel(table_name = index_schema::modules)]
struct NewModule {
    module_name: String,
    project_id: i32,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::files)]
struct NewFile {
    project_id: i32,
    module: Option<i32>,
    directory_id: i32,
    module_path: String,
    filesystem_path: String,
    filetype: String,
    content_hash: String,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::directories)]
struct NewDirectory {
    project_id: i32,
    parent_id: Option<i32>,
    path: String,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::file_contents)]
struct NewFileContent {
    file_id: i32,
    content: Vec<u8>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbols)]
struct NewSymbol {
    name: String,
    module: i32,
    symbol_scope: i32,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::declarations)]
struct NewDeclaration {
    symbol: i32,
    file_id: i32,
    symbol_type: i32,
    offset_range: std::ops::Range<i32>,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbol_refs)]
struct NewSymbolRef {
    to_symbol: i32,
    from_file: i32,
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
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
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
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Integer>)]
    child_id: Option<i32>,
    #[diesel(sql_type = diesel::sql_types::Nullable<diesel::sql_types::Text>)]
    child_path: Option<String>,
}

#[derive(Debug, QueryableByName)]
struct DirectoryPathRow {
    #[diesel(sql_type = diesel::sql_types::Text)]
    path: String,
    #[diesel(sql_type = diesel::sql_types::Integer)]
    id: i32,
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

                let module_inserts = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "build_modules",
                        count = upload.modules.len()
                    )
                    .entered();
                    build_modules(project_id, &upload.modules)?
                };
                let module_map = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_modules",
                        count = module_inserts.len()
                    )
                    .entered();
                    insert_modules(conn, module_inserts)?
                };

                let directory_paths = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("collect_directory_paths").entered();
                    collect_directory_paths(&upload.files)
                };
                let directory_map = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_directories",
                        count = directory_paths.len()
                    )
                    .entered();
                    insert_directories(conn, project_id, &directory_paths)?
                };

                let file_inserts = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "build_files",
                        count = upload.files.len()
                    )
                    .entered();
                    build_files(project_id, &upload.files, &module_map, &directory_map)?
                };
                let file_map = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_files",
                        count = file_inserts.len()
                    )
                    .entered();
                    insert_files(conn, &file_inserts)?
                };

                let symbol_inserts = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "build_symbols",
                        count = upload.modules.len()
                    )
                    .entered();
                    build_symbols(&upload.modules, &module_map)?
                };
                let symbol_map = {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_symbols",
                        count = symbol_inserts.len()
                    )
                    .entered();
                    insert_symbols(conn, symbol_inserts)?
                };

                let declaration_rows = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("build_declarations").entered();
                    build_declarations(&upload.files, &file_map, &symbol_map)?
                };
                {
                    let _span: tracing::span::EnteredSpan = tracing::info_span!(
                        "insert_declarations",
                        count = declaration_rows.len()
                    )
                    .entered();
                    insert_declarations(conn, &declaration_rows)?;
                }

                let symbol_ref_rows = {
                    let _span: tracing::span::EnteredSpan =
                        tracing::info_span!("build_symbol_refs").entered();
                    build_symbol_refs(&upload.files, &file_map, &symbol_map)?
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

            let module_rows: Vec<(i32, String)> = index_schema::modules::table
                .filter(index_schema::modules::project_id.eq(project_id))
                .select((
                    index_schema::modules::id,
                    index_schema::modules::module_name,
                ))
                .order(index_schema::modules::id)
                .load(&mut conn)?;

            let modules = module_rows
                .into_iter()
                .map(|(id, module_name)| ProjectModule { id, module_name })
                .collect();

            let file_count: i64 = index_schema::files::table
                .filter(index_schema::files::project_id.eq(project_id))
                .count()
                .get_result(&mut conn)?;

            let symbol_count: i64 = index_schema::symbols::table
                .inner_join(
                    index_schema::modules::table
                        .on(index_schema::symbols::module.eq(index_schema::modules::id)),
                )
                .filter(index_schema::modules::project_id.eq(project_id))
                .count()
                .get_result(&mut conn)?;

            Ok(Some(ProjectDetails {
                id,
                project_name,
                root_path,
                modules,
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
            let directory_id = index_schema::directories::table
                .filter(index_schema::directories::project_id.eq(project_id))
                .filter(index_schema::directories::path.eq(&normalized))
                .select(index_schema::directories::id)
                .first::<i32>(&mut conn)
                .optional()?;
            let directory_id = match directory_id {
                Some(id) => id,
                None => return Ok(ProjectTreeResult::NotDirectory),
            };

            let directories = load_directory_children_with_compact(
                &mut conn,
                project_id,
                directory_id,
                compact,
            )?;
            let files = load_file_children(&mut conn, directory_id)?;

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
            let content = index_schema::file_contents::table
                .inner_join(
                    index_schema::files::table
                        .on(index_schema::file_contents::file_id.eq(index_schema::files::id)),
                )
                .filter(index_schema::files::project_id.eq(project_id))
                .filter(index_schema::files::filesystem_path.eq(normalized))
                .select(index_schema::file_contents::content)
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
    directory_id: i32,
    compact: bool,
) -> Result<Vec<DirectoryChildRow>, StoreError> {
    let rows = diesel::sql_query(
        r#"
        SELECT
            d.id,
            d.path,
            COUNT(DISTINCT c.id) AS child_dir_count,
            COUNT(DISTINCT f.id) AS file_count
        FROM index.directories d
        LEFT JOIN index.directories c ON c.parent_id = d.id
        LEFT JOIN index.files f ON f.directory_id = d.id
        WHERE d.project_id = $1 AND d.parent_id = $2
        GROUP BY d.id
        ORDER BY d.path
        "#,
    )
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Integer, _>(directory_id)
        .load::<DirectoryChildStatsRow>(conn)?;

    let mut children = Vec::with_capacity(rows.len());
    for row in rows {
        let has_children = row.child_dir_count > 0 || row.file_count > 0;
        let compact_path = if compact && row.file_count == 0 && row.child_dir_count == 1 {
            compute_compact_path(conn, project_id, row.id)?
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
    start_id: i32,
) -> Result<Option<String>, StoreError> {
    let mut current_id = start_id;
    let mut last_path = None;
    loop {
        let row = load_directory_walk_row(conn, project_id, current_id)?;
        if row.file_count != 0 || row.child_dir_count != 1 {
            break;
        }
        let child_id = match row.child_id {
            Some(id) => id,
            None => break,
        };
        let child_path = match row.child_path {
            Some(path) => path,
            None => break,
        };
        last_path = Some(child_path);
        current_id = child_id;
    }
    Ok(last_path)
}

fn load_directory_walk_row(
    conn: &mut PgConnection,
    project_id: i32,
    directory_id: i32,
) -> Result<DirectoryWalkRow, StoreError> {
    let query = r#"
        SELECT
            (SELECT COUNT(*)
             FROM index.directories
             WHERE project_id = $1 AND parent_id = $2) AS child_dir_count,
            (SELECT COUNT(*)
             FROM index.files
             WHERE project_id = $1 AND directory_id = $2) AS file_count,
            (SELECT id
             FROM index.directories
             WHERE project_id = $1 AND parent_id = $2
             ORDER BY path
             LIMIT 1) AS child_id,
            (SELECT path
             FROM index.directories
             WHERE project_id = $1 AND parent_id = $2
             ORDER BY path
             LIMIT 1) AS child_path
    "#;

    let row = diesel::sql_query(query)
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Integer, _>(directory_id)
        .get_result::<DirectoryWalkRow>(conn)?;
    Ok(row)
}

fn load_file_children(
    conn: &mut PgConnection,
    directory_id: i32,
) -> Result<Vec<FileChildRow>, StoreError> {
    let query = r#"
        SELECT id, filesystem_path AS path, filetype
        FROM index.files
        WHERE directory_id = $1
        ORDER BY filesystem_path
    "#;

    let rows = diesel::sql_query(query)
        .bind::<diesel::sql_types::Integer, _>(directory_id)
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

struct ModuleInsert {
    local_id: i64,
    row: NewModule,
}

struct FileInsert {
    local_id: i64,
    content: Vec<u8>,
    row: NewFile,
}

struct SymbolInsert {
    local_id: i64,
    row: NewSymbol,
}

fn build_modules(
    project_id: i32,
    modules: &[UploadModule],
) -> Result<Vec<ModuleInsert>, UploadError> {
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for module in modules {
        if !seen.insert(module.local_id) {
            return Err(UploadError::Invalid(format!(
                "duplicate module local_id {}",
                module.local_id
            )));
        }
        inserts.push(ModuleInsert {
            local_id: module.local_id,
            row: NewModule {
                module_name: module.module_name.clone(),
                project_id,
            },
        });
    }
    Ok(inserts)
}

fn insert_modules(
    conn: &mut PgConnection,
    inserts: Vec<ModuleInsert>,
) -> Result<HashMap<i64, i32>, UploadError> {
    if inserts.is_empty() {
        return Ok(HashMap::new());
    }

    let mut module_map = HashMap::new();
    for chunk in inserts.chunks(MAX_INSERT_ROWS) {
        let rows: Vec<NewModule> = chunk.iter().map(|entry| entry.row.clone()).collect();
        let ids: Vec<i32> = diesel::insert_into(index_schema::modules::table)
            .values(&rows)
            .returning(index_schema::modules::id)
            .get_results(conn)?;
        for (entry, id) in chunk.iter().zip(ids) {
            module_map.insert(entry.local_id, id);
        }
    }

    Ok(module_map)
}

fn collect_directory_paths(files: &[UploadFile]) -> HashSet<String> {
    let mut paths = HashSet::new();
    paths.insert("/".to_string());
    for file in files {
        let filesystem_path = normalize_full_path(&file.filesystem_path);
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

fn path_depth(path: &str) -> usize {
    if path == "/" {
        return 0;
    }
    path.trim_matches('/')
        .split('/')
        .filter(|segment| !segment.is_empty())
        .count()
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

fn insert_directories(
    conn: &mut PgConnection,
    project_id: i32,
    directory_paths: &HashSet<String>,
) -> Result<HashMap<String, i32>, UploadError> {
    use index_schema::directories;

    let mut mapping = HashMap::new();

    if directory_paths.contains("/") {
        let inserted: Option<i32> = diesel::insert_into(directories::table)
            .values(NewDirectory {
                project_id,
                parent_id: None,
                path: "/".to_string(),
            })
            .on_conflict((directories::project_id, directories::path))
            .do_nothing()
            .returning(directories::id)
            .get_result(conn)
            .optional()?;

        let root_id = match inserted {
            Some(id) => id,
            None => directories::table
                .filter(directories::project_id.eq(project_id))
                .filter(directories::path.eq("/"))
                .select(directories::id)
                .first::<i32>(conn)?,
        };
        mapping.insert("/".to_string(), root_id);
    }

    let mut entries = Vec::new();
    for path in directory_paths {
        if path == "/" {
            continue;
        }
        entries.push(DirectoryEntry {
            path: path.clone(),
            parent_path: parent_dir(path),
            depth: path_depth(path),
        });
    }
    entries.sort_by(|a, b| a.depth.cmp(&b.depth).then_with(|| a.path.cmp(&b.path)));

    let mut index = 0;
    while index < entries.len() {
        let depth = entries[index].depth;
        let mut end_index = index + 1;
        while end_index < entries.len() && entries[end_index].depth == depth {
            end_index += 1;
        }

        for chunk in entries[index..end_index].chunks(MAX_INSERT_ROWS) {
            insert_directory_batch(conn, project_id, chunk)?;
            let paths: Vec<String> = chunk.iter().map(|entry| entry.path.clone()).collect();
            for row in fetch_directory_ids(conn, project_id, &paths)? {
                mapping.insert(row.path, row.id);
            }
        }

        index = end_index;
    }

    Ok(mapping)
}

fn insert_directory_batch(
    conn: &mut PgConnection,
    project_id: i32,
    entries: &[DirectoryEntry],
) -> Result<(), UploadError> {
    if entries.is_empty() {
        return Ok(());
    }

    let paths: Vec<String> = entries.iter().map(|entry| entry.path.clone()).collect();
    let parent_paths: Vec<String> = entries
        .iter()
        .map(|entry| entry.parent_path.clone())
        .collect();

    let query = r#"
        INSERT INTO index.directories (project_id, parent_id, path)
        SELECT $1, parent.id, v.path
        FROM unnest($2::text[], $3::text[]) AS v(path, parent_path)
        JOIN index.directories parent
          ON parent.project_id = $1 AND parent.path = v.parent_path
        ON CONFLICT (project_id, path) DO NOTHING
    "#;

    diesel::sql_query(query)
        .bind::<diesel::sql_types::Integer, _>(project_id)
        .bind::<diesel::sql_types::Array<diesel::sql_types::Text>, _>(paths)
        .bind::<diesel::sql_types::Array<diesel::sql_types::Text>, _>(parent_paths)
        .execute(conn)?;
    Ok(())
}

fn fetch_directory_ids(
    conn: &mut PgConnection,
    project_id: i32,
    paths: &[String],
) -> Result<Vec<DirectoryPathRow>, UploadError> {
    if paths.is_empty() {
        return Ok(Vec::new());
    }

    let rows = diesel::sql_query(
        "SELECT path, id FROM index.directories WHERE project_id = $1 AND path = ANY($2)",
    )
    .bind::<diesel::sql_types::Integer, _>(project_id)
    .bind::<diesel::sql_types::Array<diesel::sql_types::Text>, _>(paths.to_vec())
    .load::<DirectoryPathRow>(conn)?;
    Ok(rows)
}

#[derive(Clone)]
struct DirectoryEntry {
    path: String,
    parent_path: String,
    depth: usize,
}

fn build_files(
    project_id: i32,
    files: &[UploadFile],
    module_map: &HashMap<i64, i32>,
    directory_map: &HashMap<String, i32>,
) -> Result<Vec<FileInsert>, UploadError> {
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for file in files {
        if !seen.insert(file.local_id) {
            return Err(UploadError::Invalid(format!(
                "duplicate file local_id {}",
                file.local_id
            )));
        }
        let filesystem_path_raw = file.filesystem_path.trim();
        if filesystem_path_raw.is_empty() {
            return Err(UploadError::Invalid(format!(
                "filesystem_path is required for file {}",
                file.local_id
            )));
        }
        if !filesystem_path_raw.starts_with('/') {
            return Err(UploadError::Invalid(format!(
                "filesystem_path must be an absolute path for file {}",
                file.local_id
            )));
        }
        let module_id = match file.module_id {
            Some(local_id) => {
                let mapped = module_map.get(&local_id).ok_or_else(|| {
                    UploadError::Invalid(format!(
                        "missing module mapping for local_id {}",
                        local_id
                    ))
                })?;
                Some(*mapped)
            }
            None => None,
        };
        let filesystem_path = normalize_full_path(filesystem_path_raw);
        let directory_path = parent_dir(&filesystem_path);
        let directory_id = directory_map.get(&directory_path).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing directory mapping for path {}",
                directory_path
            ))
        })?;
        inserts.push(FileInsert {
            local_id: file.local_id,
            content: file.content.clone(),
            row: NewFile {
                project_id,
                module: module_id,
                directory_id: *directory_id,
                module_path: file.module_path.clone(),
                filesystem_path,
                filetype: file.filetype.clone(),
                content_hash: hash_bytes(&file.content),
            },
        });
    }
    Ok(inserts)
}

fn insert_files(
    conn: &mut PgConnection,
    inserts: &[FileInsert],
) -> Result<HashMap<i64, i32>, UploadError> {
    if inserts.is_empty() {
        return Ok(HashMap::new());
    }

    let mut file_map = HashMap::new();
    for chunk in inserts.chunks(MAX_INSERT_ROWS) {
        let rows: Vec<NewFile> = chunk.iter().map(|entry| entry.row.clone()).collect();
        let ids: Vec<i32> = diesel::insert_into(index_schema::files::table)
            .values(&rows)
            .returning(index_schema::files::id)
            .get_results(conn)?;

        let mut file_contents = Vec::with_capacity(ids.len());
        for (entry, id) in chunk.iter().zip(ids.iter()) {
            file_map.insert(entry.local_id, *id);
            file_contents.push(NewFileContent {
                file_id: *id,
                content: entry.content.clone(),
            });
        }

        diesel::insert_into(index_schema::file_contents::table)
            .values(&file_contents)
            .execute(conn)?;
    }

    Ok(file_map)
}

fn build_symbols(
    modules: &[UploadModule],
    module_map: &HashMap<i64, i32>,
) -> Result<Vec<SymbolInsert>, UploadError> {
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for module in modules {
        let module_id = module_map.get(&module.local_id).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing module mapping for local_id {}",
                module.local_id
            ))
        })?;
        for symbol in &module.symbols {
            if !seen.insert(symbol.local_id) {
                return Err(UploadError::Invalid(format!(
                    "duplicate symbol local_id {}",
                    symbol.local_id
                )));
            }
            inserts.push(SymbolInsert {
                local_id: symbol.local_id,
                row: NewSymbol {
                    name: symbol.name.clone(),
                    module: *module_id,
                    symbol_scope: symbol.scope,
                },
            });
        }
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

fn build_declarations(
    files: &[UploadFile],
    file_map: &HashMap<i64, i32>,
    symbol_map: &HashMap<i64, i32>,
) -> Result<Vec<NewDeclaration>, UploadError> {
    let mut rows = Vec::new();
    for file in files {
        let file_id = file_map.get(&file.local_id).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing file mapping for local_id {}",
                file.local_id
            ))
        })?;
        for declaration in &file.declarations {
            let symbol_id = symbol_map
                .get(&declaration.symbol_local_id)
                .ok_or_else(|| {
                    UploadError::Invalid(format!(
                        "unknown symbol local_id {}",
                        declaration.symbol_local_id
                    ))
                })?;
            rows.push(NewDeclaration {
                symbol: *symbol_id,
                file_id: *file_id,
                symbol_type: declaration.symbol_type,
                offset_range: declaration.start_offset..declaration.end_offset,
            });
        }
    }
    Ok(rows)
}

fn build_symbol_refs(
    files: &[UploadFile],
    file_map: &HashMap<i64, i32>,
    symbol_map: &HashMap<i64, i32>,
) -> Result<Vec<NewSymbolRef>, UploadError> {
    let mut rows = Vec::new();
    for file in files {
        let file_id = file_map.get(&file.local_id).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing file mapping for local_id {}",
                file.local_id
            ))
        })?;
        for reference in &file.refs {
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
                from_file: *file_id,
                from_offset_range: reference.from_offset_start..reference.from_offset_end,
            });
        }
    }
    Ok(rows)
}

fn insert_declarations(
    conn: &mut PgConnection,
    rows: &[NewDeclaration],
) -> Result<(), UploadError> {
    for chunk in rows.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::declarations::table)
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
