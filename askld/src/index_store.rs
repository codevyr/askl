use std::collections::{HashMap, HashSet};

use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel::OptionalExtension;
use serde::Serialize;
use sha2::{Digest, Sha256};
use tokio::task;

use crate::proto::askl::index::{IndexUpload, Module as UploadModule};
use index::schema_diesel as index_schema;

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
    pub modules: Vec<ProjectModule>,
    pub file_count: i64,
    pub symbol_count: i64,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::projects)]
struct NewProject {
    project_name: String,
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
    module: i32,
    module_path: String,
    filesystem_path: String,
    filetype: String,
    content_hash: String,
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
    start_offset: i32,
    end_offset: i32,
}

#[derive(Insertable, Clone)]
#[diesel(table_name = index_schema::symbol_refs)]
struct NewSymbolRef {
    to_symbol: i32,
    from_file: i32,
    from_offset_start: i32,
    from_offset_end: i32,
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

    pub async fn upload_index(&self, upload: IndexUpload) -> Result<i32, UploadError> {
        let pool = self.pool.clone();
        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| UploadError::Storage(err.to_string()))?;
            conn.transaction::<_, UploadError, _>(|conn| {
                let project_name = upload.project_name.trim();
                if project_name.is_empty() {
                    return Err(UploadError::Invalid(
                        "project_name is required".to_string(),
                    ));
                }

                let project_id: Option<i32> = diesel::insert_into(index_schema::projects::table)
                    .values(NewProject {
                        project_name: project_name.to_string(),
                    })
                    .on_conflict(index_schema::projects::project_name)
                    .do_nothing()
                    .returning(index_schema::projects::id)
                    .get_result(conn)
                    .optional()?;

                let project_id = match project_id {
                    Some(id) => id,
                    None => return Err(UploadError::Conflict),
                };

                let module_inserts = build_modules(project_id, &upload.modules)?;
                let module_map = insert_modules(conn, module_inserts)?;

                let file_inserts = build_files(&upload.modules, &module_map)?;
                let file_map = insert_files(conn, &file_inserts)?;

                let symbol_inserts = build_symbols(&upload.modules, &module_map)?;
                let symbol_map = insert_symbols(conn, symbol_inserts)?;

                let declaration_rows = build_declarations(&upload.modules, &file_map, &symbol_map)?;
                insert_declarations(conn, &declaration_rows)?;

                let symbol_ref_rows = build_symbol_refs(&upload.modules, &file_map, &symbol_map)?;
                insert_symbol_refs(conn, &symbol_ref_rows)?;

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
            let rows: Vec<(i32, String)> = index_schema::projects::table
                .select((index_schema::projects::id, index_schema::projects::project_name))
                .order(index_schema::projects::id)
                .load(&mut conn)?;
            Ok(rows
                .into_iter()
                .map(|(id, project_name)| ProjectInfo { id, project_name })
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

            let project_row: Option<(i32, String)> = index_schema::projects::table
                .filter(index_schema::projects::id.eq(project_id))
                .select((index_schema::projects::id, index_schema::projects::project_name))
                .first(&mut conn)
                .optional()?;

            let (id, project_name) = match project_row {
                Some(row) => row,
                None => return Ok(None),
            };

            let module_rows: Vec<(i32, String)> = index_schema::modules::table
                .filter(index_schema::modules::project_id.eq(project_id))
                .select((index_schema::modules::id, index_schema::modules::module_name))
                .order(index_schema::modules::id)
                .load(&mut conn)?;

            let modules = module_rows
                .into_iter()
                .map(|(id, module_name)| ProjectModule { id, module_name })
                .collect();

            let file_count: i64 = index_schema::files::table
                .inner_join(
                    index_schema::modules::table
                        .on(index_schema::files::module.eq(index_schema::modules::id)),
                )
                .filter(index_schema::modules::project_id.eq(project_id))
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

fn build_modules(project_id: i32, modules: &[UploadModule]) -> Result<Vec<ModuleInsert>, UploadError> {
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

fn build_files(
    modules: &[UploadModule],
    module_map: &HashMap<i64, i32>,
) -> Result<Vec<FileInsert>, UploadError> {
    let mut seen = HashSet::new();
    let mut inserts = Vec::new();
    for module in modules {
        let module_id = module_map.get(&module.local_id).ok_or_else(|| {
            UploadError::Invalid(format!(
                "missing module mapping for local_id {}",
                module.local_id
            ))
        })?;
        for file in &module.files {
            if !seen.insert(file.local_id) {
                return Err(UploadError::Invalid(format!(
                    "duplicate file local_id {}",
                    file.local_id
                )));
            }
            inserts.push(FileInsert {
                local_id: file.local_id,
                content: file.content.clone(),
                row: NewFile {
                    module: *module_id,
                    module_path: file.module_path.clone(),
                    filesystem_path: file.filesystem_path.clone(),
                    filetype: file.filetype.clone(),
                    content_hash: hash_bytes(&file.content),
                },
            });
        }
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
    modules: &[UploadModule],
    file_map: &HashMap<i64, i32>,
    symbol_map: &HashMap<i64, i32>,
) -> Result<Vec<NewDeclaration>, UploadError> {
    let mut rows = Vec::new();
    for module in modules {
        for file in &module.files {
            let file_id = file_map.get(&file.local_id).ok_or_else(|| {
                UploadError::Invalid(format!(
                    "missing file mapping for local_id {}",
                    file.local_id
                ))
            })?;
            for declaration in &file.declarations {
                let symbol_id = symbol_map.get(&declaration.symbol_local_id).ok_or_else(|| {
                    UploadError::Invalid(format!(
                        "unknown symbol local_id {}",
                        declaration.symbol_local_id
                    ))
                })?;
                rows.push(NewDeclaration {
                    symbol: *symbol_id,
                    file_id: *file_id,
                    symbol_type: declaration.symbol_type,
                    start_offset: declaration.start_offset,
                    end_offset: declaration.end_offset,
                });
            }
        }
    }
    Ok(rows)
}

fn build_symbol_refs(
    modules: &[UploadModule],
    file_map: &HashMap<i64, i32>,
    symbol_map: &HashMap<i64, i32>,
) -> Result<Vec<NewSymbolRef>, UploadError> {
    let mut rows = Vec::new();
    for module in modules {
        for file in &module.files {
            let file_id = file_map.get(&file.local_id).ok_or_else(|| {
                UploadError::Invalid(format!(
                    "missing file mapping for local_id {}",
                    file.local_id
                ))
            })?;
            for reference in &file.refs {
                let symbol_id = symbol_map.get(&reference.to_symbol_local_id).ok_or_else(|| {
                    UploadError::Invalid(format!(
                        "unknown symbol local_id {}",
                        reference.to_symbol_local_id
                    ))
                })?;
                rows.push(NewSymbolRef {
                    to_symbol: *symbol_id,
                    from_file: *file_id,
                    from_offset_start: reference.from_offset_start,
                    from_offset_end: reference.from_offset_end,
                });
            }
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
