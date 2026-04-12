use std::collections::{HashMap, HashSet};

use diesel::prelude::*;
use diesel::OptionalExtension;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use tracing::Instrument;

use crate::proto::askl::index::{ContentBatch, Object as UploadObject, Project as UploadProject, Symbol as UploadSymbol};
use index::schema_diesel as index_schema;

use super::{
    hash_bytes, normalize_full_path, IndexStore, NewContentStoreRow, NewObject, NewObjectContent,
    NewProject, NewSymbol, NewSymbolInstance, NewSymbolRef, UploadError, MAX_INSERT_ROWS,
};

struct ObjectInsert {
    local_id: i64,
    content: Option<Vec<u8>>,
    row: NewObject,
}

struct SymbolInsert {
    local_id: i64,
    row: NewSymbol,
}

impl IndexStore {
    pub async fn upload_index(&self, upload: UploadProject) -> Result<i32, UploadError> {
        let mut conn = self.get_upload_conn().await?;

        let upload_span = tracing::info_span!("index_upload_store");
        conn.transaction::<_, UploadError, _>(|conn| {
            async move {
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

                let project_id: Option<i32> = diesel::insert_into(index_schema::projects::table)
                    .values(NewProject {
                        project_name: project_name.to_string(),
                        root_path: root_path.to_string(),
                    })
                    .on_conflict(index_schema::projects::project_name)
                    .do_nothing()
                    .returning(index_schema::projects::id)
                    .get_result(conn)
                    .await
                    .optional()?;

                let project_id = match project_id {
                    Some(id) => id,
                    None => return Err(UploadError::Conflict),
                };

                let mut object_inserts = build_objects(project_id, &upload.objects)?;

                // Upfront validation: verify all hash-only objects have content in content_store
                let hash_only_hashes: Vec<String> = object_inserts
                    .iter()
                    .filter(|oi| oi.content.is_none() && !oi.row.content_hash.is_empty())
                    .map(|oi| oi.row.content_hash.clone())
                    .collect::<HashSet<_>>()
                    .into_iter()
                    .collect();

                if !hash_only_hashes.is_empty() {
                    let existing: Vec<String> = index_schema::content_store::table
                        .filter(index_schema::content_store::content_hash.eq_any(&hash_only_hashes))
                        .select(index_schema::content_store::content_hash)
                        .load(conn)
                        .await?;
                    let existing_set: HashSet<&str> = existing.iter().map(|s| s.as_str()).collect();
                    let missing: Vec<&str> = hash_only_hashes
                        .iter()
                        .filter(|h| !existing_set.contains(h.as_str()))
                        .map(|h| h.as_str())
                        .collect();
                    if !missing.is_empty() {
                        return Err(UploadError::Invalid(format!(
                            "missing content for {} hash(es): {}",
                            missing.len(),
                            missing.join(", ")
                        )));
                    }
                }

                let object_map = insert_objects(conn, &mut object_inserts).await?;

                let symbol_inserts = build_symbols(project_id, &upload.symbols)?;
                let symbol_map = insert_symbols(conn, symbol_inserts).await?;

                let symbol_instance_rows =
                    build_symbol_instances(&upload.objects, &object_map, &symbol_map)?;
                insert_symbol_instances(conn, &symbol_instance_rows).await?;

                let symbol_ref_rows =
                    build_symbol_refs(&upload.objects, &object_map, &symbol_map)?;
                insert_symbol_refs(conn, &symbol_ref_rows).await?;

                Ok(project_id)
            }
            .scope_boxed()
        })
        .instrument(upload_span)
        .await
    }

    pub async fn upload_contents(&self, batch: ContentBatch) -> Result<usize, UploadError> {
        let mut conn = self.get_upload_conn().await?;

        // Validate all hashes upfront before any inserts
        let mut rows = Vec::with_capacity(batch.contents.len());
        for entry in &batch.contents {
            let expected_hash = entry.content_hash.trim();
            if expected_hash.is_empty() {
                return Err(UploadError::Invalid(
                    "content_hash is required on ObjectContent".to_string(),
                ));
            }
            let actual_hash = hash_bytes(&entry.content);
            if actual_hash != expected_hash {
                return Err(UploadError::Invalid(format!(
                    "content_hash mismatch: expected {}, got {}",
                    expected_hash, actual_hash
                )));
            }
            rows.push(NewContentStoreRow {
                content_hash: entry.content_hash.clone(),
                content: entry.content.clone(),
            });
        }

        let mut new_count = 0usize;
        for chunk in rows.chunks(MAX_INSERT_ROWS) {
            let inserted = diesel::insert_into(index_schema::content_store::table)
                .values(chunk)
                .on_conflict(index_schema::content_store::content_hash)
                .do_nothing()
                .execute(&mut conn)
                .await?;
            new_count += inserted;
        }

        Ok(new_count)
    }
}

/// Validates proto symbol type against known symbol type constants.
fn validate_symbol_type(proto_type: i32) -> Result<i32, UploadError> {
    const VALID_TYPES: &[i32] = &[
        index::db_diesel::SYMBOL_TYPE_FUNCTION,
        index::db_diesel::SYMBOL_TYPE_FILE,
        index::db_diesel::SYMBOL_TYPE_MODULE,
        index::db_diesel::SYMBOL_TYPE_DIRECTORY,
        index::db_diesel::SYMBOL_TYPE_TYPE,
        index::db_diesel::SYMBOL_TYPE_DATA,
        index::db_diesel::SYMBOL_TYPE_MACRO,
        index::db_diesel::SYMBOL_TYPE_FIELD,
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

/// Validates proto instance type against known instance type constants.
fn validate_instance_type(proto_type: i32) -> Result<i32, UploadError> {
    const VALID_TYPES: &[i32] = &[
        index::db_diesel::INSTANCE_TYPE_DEFINITION,
        index::db_diesel::INSTANCE_TYPE_DECLARATION,
        index::db_diesel::INSTANCE_TYPE_EXPANSION,
        index::db_diesel::INSTANCE_TYPE_SENTINEL,
        index::db_diesel::INSTANCE_TYPE_CONTAINMENT,
        index::db_diesel::INSTANCE_TYPE_SOURCE,
        index::db_diesel::INSTANCE_TYPE_HEADER,
        index::db_diesel::INSTANCE_TYPE_BUILD,
        index::db_diesel::INSTANCE_TYPE_FILE,
        index::db_diesel::INSTANCE_TYPE_DOCUMENTATION,
    ];
    if VALID_TYPES.contains(&proto_type) {
        Ok(proto_type)
    } else {
        Err(UploadError::Invalid(format!(
            "invalid instance type {}",
            proto_type
        )))
    }
}

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
        if filesystem_path.split('/').any(|c| c == "..") {
            return Err(UploadError::Invalid(format!(
                "filesystem_path contains '..' after normalization for object {}",
                object.local_id
            )));
        }
        let (content, content_hash) = if !object.content_hash.is_empty() && object.content.is_empty() {
            // Hash-only object: content lives in content_store
            (None, object.content_hash.clone())
        } else {
            // Inline content (legacy or new with content)
            let hash = hash_bytes(&object.content);
            (Some(object.content.clone()), hash)
        };
        inserts.push(ObjectInsert {
            local_id: object.local_id,
            content,
            row: NewObject {
                project_id,
                module_path: object.module_path.clone(),
                filesystem_path,
                filetype: object.filetype.clone(),
                content_hash,
            },
        });
    }
    Ok(inserts)
}

async fn insert_objects(
    conn: &mut AsyncPgConnection,
    inserts: &mut [ObjectInsert],
) -> Result<HashMap<i64, i32>, UploadError> {
    if inserts.is_empty() {
        return Ok(HashMap::new());
    }

    let mut object_map = HashMap::new();
    for chunk in inserts.chunks_mut(MAX_INSERT_ROWS) {
        let rows: Vec<NewObject> = chunk.iter().map(|entry| entry.row.clone()).collect();
        let ids: Vec<i32> = diesel::insert_into(index_schema::objects::table)
            .values(&rows)
            .returning(index_schema::objects::id)
            .get_results(conn)
            .await?;

        let mut object_contents = Vec::new();
        let mut content_store_rows = Vec::new();
        for (entry, id) in chunk.iter_mut().zip(ids.iter()) {
            object_map.insert(entry.local_id, *id);
            if let Some(content) = entry.content.take() {
                // Inline content: insert into object_contents (legacy) and content_store (dedup)
                content_store_rows.push(NewContentStoreRow {
                    content_hash: entry.row.content_hash.clone(),
                    content: content.clone(),
                });
                object_contents.push(NewObjectContent {
                    object_id: *id,
                    content, // moved, no extra clone
                });
            }
            // Hash-only objects: no insert into object_contents — content lives in content_store
        }

        if !object_contents.is_empty() {
            diesel::insert_into(index_schema::object_contents::table)
                .values(&object_contents)
                .execute(conn)
                .await?;
        }
        if !content_store_rows.is_empty() {
            diesel::insert_into(index_schema::content_store::table)
                .values(&content_store_rows)
                .on_conflict(index_schema::content_store::content_hash)
                .do_nothing()
                .execute(conn)
                .await?;
        }
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

async fn insert_symbols(
    conn: &mut AsyncPgConnection,
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
            .get_results(conn)
            .await?;
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
            let instance_type = if instance.instance_type != 0 {
                validate_instance_type(instance.instance_type)?
            } else {
                index::db_diesel::INSTANCE_TYPE_DEFINITION
            };
            rows.push(NewSymbolInstance {
                symbol: *symbol_id,
                object_id: *object_id,
                offset_range: instance.start_offset..instance.end_offset,
                instance_type,
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

async fn insert_symbol_instances(
    conn: &mut AsyncPgConnection,
    rows: &[NewSymbolInstance],
) -> Result<(), UploadError> {
    for chunk in rows.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::symbol_instances::table)
            .values(chunk)
            .execute(conn)
            .await?;
    }
    Ok(())
}

async fn insert_symbol_refs(conn: &mut AsyncPgConnection, rows: &[NewSymbolRef]) -> Result<(), UploadError> {
    for chunk in rows.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::symbol_refs::table)
            .values(chunk)
            .execute(conn)
            .await?;
    }
    Ok(())
}
