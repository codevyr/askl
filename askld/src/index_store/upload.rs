use std::collections::{HashMap, HashSet};

use diesel::prelude::*;
use diesel::result::{DatabaseErrorKind, Error as DieselError};
use diesel::OptionalExtension;
use diesel_async::scoped_futures::ScopedFutureExt;
use diesel_async::{AsyncConnection, AsyncPgConnection, RunQueryDsl};
use tracing::Instrument;

use crate::proto::askl::index::{ContentBatch, Object as UploadObject, Project as UploadProject, Symbol as UploadSymbol};
use index::symbols::symbol_path_and_leaf;
use index::schema_diesel as index_schema;

use super::{
    hash_bytes, normalize_full_path, IndexStore, NewContentStoreRow, NewObject,
    NewProject, NewSymbol, NewSymbolInstance, NewSymbolRef, UploadError,
    UploadStatus, MAX_INSERT_ROWS, MAX_SYMBOL_INSERT_ROWS,
};

struct ObjectInsert {
    local_id: i64,
    content: Option<Vec<u8>>,
    row: NewObject,
}

impl IndexStore {
    pub async fn upload_index(&self, upload: UploadProject) -> Result<i32, UploadError> {
        let project_name = upload.project_name.trim().to_string();
        if project_name.is_empty() {
            return Err(UploadError::Invalid("project_name is required".to_string()));
        }
        let root_path = upload.root_path.trim().to_string();
        if root_path.is_empty() {
            return Err(UploadError::Invalid("root_path is required".to_string()));
        }
        if !root_path.starts_with('/') {
            return Err(UploadError::Invalid(
                "root_path must be an absolute path".to_string(),
            ));
        }

        // Transaction 1: create project row, cleaning up any zombie from a prior failed upload
        let project_id = {
            let mut conn = self.get_upload_conn().await?;
            conn.transaction::<_, UploadError, _>(move |conn| {
                create_project(conn, project_name, root_path).scope_boxed()
            })
            .await?
        };

        // IDs are computed as project_id << 32 | local_id — no DB roundtrip needed.
        let symbol_rows = build_symbols(project_id, &upload.symbols)?;
        let total_batches = (symbol_rows.len() + MAX_SYMBOL_INSERT_ROWS - 1).max(1) / MAX_SYMBOL_INSERT_ROWS.max(1);
        tracing::info!(
            count = symbol_rows.len(),
            batches = total_batches,
            "upload_index: inserting symbols"
        );

        let mut conn = self.get_upload_conn().await?;
        for (batch_idx, chunk) in symbol_rows.chunks(MAX_SYMBOL_INSERT_ROWS).enumerate() {
            let rows: Vec<NewSymbol> = chunk.to_vec();
            let result = diesel::insert_into(index_schema::symbols::table)
                .values(rows)
                .execute(&mut conn)
                .await;

            if let Err(e) = result {
                // Use a fresh connection — the current one may be broken after the insert error.
                if let Ok(mut fallback) = self.get_upload_conn().await {
                    let _ = diesel::update(
                        index_schema::projects::table
                            .filter(index_schema::projects::id.eq(project_id)),
                    )
                    .set(index_schema::projects::upload_status.eq(UploadStatus::Failed))
                    .execute(&mut fallback)
                    .await;
                }
                return Err(UploadError::Storage(e.to_string()));
            }

            tracing::info!(
                batch = batch_idx + 1,
                total = total_batches,
                "upload_index: symbol batch committed"
            );
        }

        tracing::info!(project_id, "upload_index: symbols done");
        Ok(project_id)
    }

    pub async fn finalize_project(&self, project_id: i32) -> Result<bool, UploadError> {
        let mut conn = self.get_upload_conn().await?;
        conn.transaction::<_, UploadError, _>(|conn| {
            async move {
                // SELECT FOR UPDATE so the status check and update are atomic: no concurrent
                // delete can sneak in between the read and the write.
                let status: Option<UploadStatus> = index_schema::projects::table
                    .filter(index_schema::projects::id.eq(project_id))
                    .select(index_schema::projects::upload_status)
                    .for_update()
                    .first(conn)
                    .await
                    .optional()?;
                match status {
                    None => Ok(false),
                    Some(UploadStatus::Uploading) => {
                        diesel::update(
                            index_schema::projects::table
                                .filter(index_schema::projects::id.eq(project_id)),
                        )
                        .set(index_schema::projects::upload_status.eq(UploadStatus::Complete))
                        .execute(conn)
                        .await?;
                        Ok(true)
                    }
                    Some(UploadStatus::Complete) => Ok(true),  // idempotent: already finalized
                    Some(_) => Err(UploadError::Conflict),
                }
            }
            .scope_boxed()
        })
        .await
    }

    pub async fn upload_objects(
        &self,
        project_id: i32,
        upload: UploadProject,
    ) -> Result<(), UploadError> {
        if !upload.symbols.is_empty() {
            return Err(UploadError::Invalid(
                "symbols must be uploaded in phase 1 (header) only".to_string(),
            ));
        }

        let mut conn = self.get_upload_conn().await?;
        let upload_span = tracing::info_span!("index_upload_objects");
        do_upload_objects(&mut conn, project_id, upload)
            .instrument(upload_span)
            .await
    }

    pub async fn upload_contents(&self, batch: ContentBatch) -> Result<usize, UploadError> {
        let mut conn = self.get_upload_conn().await?;

        // Validate all hashes and build rows, consuming the batch to avoid a double-allocation.
        let rows: Vec<NewContentStoreRow> = batch
            .contents
            .into_iter()
            .map(|entry| {
                let hash_trimmed = entry.content_hash.trim();
                if hash_trimmed.is_empty() {
                    return Err(UploadError::Invalid(
                        "content_hash is required on ObjectContent".to_string(),
                    ));
                }
                let actual_hash = hash_bytes(&entry.content);
                if actual_hash != hash_trimmed {
                    return Err(UploadError::Invalid(format!(
                        "content_hash mismatch: expected {}, got {}",
                        entry.content_hash, actual_hash
                    )));
                }
                Ok(NewContentStoreRow {
                    content_hash: entry.content_hash,
                    content: entry.content,
                })
            })
            .collect::<Result<Vec<_>, _>>()?;

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

/// Transaction body for project creation in phase 1.
///
/// Zombie cleanup: `Failed`, `Deleting`, or `Uploading` projects (from a previous aborted upload
/// or a crashed client) are deleted and replaced. `Complete` projects return `Conflict`.
/// The `FOR UPDATE` lock serializes concurrent zombie cleanups on the same name; the unique
/// constraint on `project_name` catches any remaining races at INSERT time.
async fn create_project(
    conn: &mut AsyncPgConnection,
    project_name: String,
    root_path: String,
) -> Result<i32, UploadError> {
    let existing: Option<(i32, UploadStatus)> = index_schema::projects::table
        .filter(index_schema::projects::project_name.eq(&project_name))
        .select((index_schema::projects::id, index_schema::projects::upload_status))
        .for_update()
        .first(conn)
        .await
        .optional()?;

    if let Some((existing_id, existing_status)) = existing {
        if matches!(existing_status, UploadStatus::Failed | UploadStatus::Deleting | UploadStatus::Uploading) {
            tracing::info!(project_id = existing_id, status = %existing_status, "upload_index: deleting zombie project");
            diesel::delete(
                index_schema::projects::table.filter(index_schema::projects::id.eq(existing_id)),
            )
            .execute(conn)
            .await?;
        } else {
            // Complete — project already exists.
            return Err(UploadError::Conflict);
        }
    }

    let insert_result: Result<i32, DieselError> = diesel::insert_into(index_schema::projects::table)
        .values(NewProject {
            project_name,
            root_path,
            upload_status: UploadStatus::Uploading,
        })
        .returning(index_schema::projects::id)
        .get_result(conn)
        .await;

    match insert_result {
        Ok(id) => Ok(id),
        Err(DieselError::DatabaseError(DatabaseErrorKind::UniqueViolation, _)) => {
            Err(UploadError::Conflict)
        }
        Err(e) => Err(UploadError::Storage(e.to_string())),
    }
}

/// Transaction body for phase 2 object upload.
///
/// Symbol IDs are computed directly from `project_id << 32 | local_id` — no DB lookup needed.
async fn do_upload_objects(
    conn: &mut AsyncPgConnection,
    project_id: i32,
    upload: UploadProject,
) -> Result<(), UploadError> {
    let mut object_inserts = build_objects(project_id, &upload.objects)?;
    tracing::info!(objects = object_inserts.len(), "upload_objects: built inserts");

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

    tracing::info!("upload_objects: inserting objects");
    let object_map = insert_objects(conn, &mut object_inserts).await?;
    tracing::info!(inserted = object_map.len(), "upload_objects: objects done");

    let instance_rows = build_symbol_instances(project_id, &upload.objects, &object_map)?;
    tracing::info!(count = instance_rows.len(), "upload_objects: inserting instances");
    insert_symbol_instances(conn, &instance_rows).await?;
    tracing::info!("upload_objects: instances done");

    let ref_rows = build_symbol_refs(project_id, &upload.objects, &object_map)?;
    tracing::info!(count = ref_rows.len(), "upload_objects: inserting refs");
    insert_symbol_refs(conn, &ref_rows).await?;
    tracing::info!("upload_objects: refs done");

    Ok(())
}

fn resolve_object_id(map: &HashMap<i64, i32>, local_id: i64) -> Result<i32, UploadError> {
    map.get(&local_id).copied().ok_or_else(|| {
        UploadError::Invalid(format!("missing object mapping for local_id {}", local_id))
    })
}

/// Compute a globally unique symbol DB id from a project id and client-assigned local id.
///
/// Layout: `project_id as i64 << 32 | local_id`
///
/// Invariants (enforced here):
/// - `local_id` must be in `[0, 2^32)` so the two halves don't overlap.
/// - `project_id` is a positive SERIAL so the combined value is always positive and fits in i64.
fn compute_symbol_id(project_id: i32, local_id: i64) -> Result<i64, UploadError> {
    if project_id <= 0 {
        return Err(UploadError::Invalid(format!(
            "project_id {} must be positive", project_id
        )));
    }
    if local_id < 0 || local_id >= (1i64 << 32) {
        return Err(UploadError::Invalid(format!(
            "symbol local_id {} is out of range [0, 2^32)",
            local_id
        )));
    }
    Ok((project_id as i64) << 32 | local_id)
}

fn validate_type(value: i32, valid: &[i32], label: &str) -> Result<i32, UploadError> {
    if valid.contains(&value) {
        Ok(value)
    } else {
        Err(UploadError::Invalid(format!("invalid {} {}", label, value)))
    }
}

fn validate_symbol_type(proto_type: i32) -> Result<i32, UploadError> {
    const VALID: &[i32] = &[
        index::db_diesel::SYMBOL_TYPE_FUNCTION,
        index::db_diesel::SYMBOL_TYPE_FILE,
        index::db_diesel::SYMBOL_TYPE_MODULE,
        index::db_diesel::SYMBOL_TYPE_DIRECTORY,
        index::db_diesel::SYMBOL_TYPE_TYPE,
        index::db_diesel::SYMBOL_TYPE_DATA,
        index::db_diesel::SYMBOL_TYPE_MACRO,
        index::db_diesel::SYMBOL_TYPE_FIELD,
    ];
    validate_type(proto_type, VALID, "symbol type")
}

fn validate_instance_type(proto_type: i32) -> Result<i32, UploadError> {
    const VALID: &[i32] = &[
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
    validate_type(proto_type, VALID, "instance type")
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

        let (content, content_hash) = if !object.content_hash.is_empty() && object.content.is_empty() {
            // Hash-only object: content lives in content_store
            (None, object.content_hash.clone())
        } else {
            // Inline content: compute hash from content; reject if client-supplied hash disagrees
            let computed = hash_bytes(&object.content);
            if !object.content_hash.is_empty() && object.content_hash != computed {
                return Err(UploadError::Invalid(format!(
                    "content_hash mismatch for object {}: client sent {} but computed {}",
                    object.local_id, object.content_hash, computed
                )));
            }
            (Some(object.content.clone()), computed)
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
            .on_conflict((
                index_schema::objects::project_id,
                index_schema::objects::filesystem_path,
            ))
            .do_update()
            .set((
                index_schema::objects::content_hash
                    .eq(diesel::upsert::excluded(index_schema::objects::content_hash)),
                index_schema::objects::filetype
                    .eq(diesel::upsert::excluded(index_schema::objects::filetype)),
                index_schema::objects::module_path
                    .eq(diesel::upsert::excluded(index_schema::objects::module_path)),
            ))
            .returning(index_schema::objects::id)
            .get_results(conn)
            .await?;

        let mut content_store_rows = Vec::new();
        for (entry, id) in chunk.iter_mut().zip(ids.iter()) {
            object_map.insert(entry.local_id, *id);
            if let Some(content) = entry.content.take() {
                // Inline content: store in content_store only (object_contents is legacy)
                content_store_rows.push(NewContentStoreRow {
                    content_hash: entry.row.content_hash.clone(),
                    content,
                });
            }
            // Hash-only objects: content already in content_store, nothing to insert
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
) -> Result<Vec<NewSymbol>, UploadError> {
    let mut seen = HashSet::new();
    let mut rows = Vec::new();
    for symbol in symbols {
        if !seen.insert(symbol.local_id) {
            return Err(UploadError::Invalid(format!(
                "duplicate symbol local_id {}",
                symbol.local_id
            )));
        }
        let id = compute_symbol_id(project_id, symbol.local_id)?;
        let symbol_type = validate_symbol_type(symbol.r#type)?;
        let symbol_scope = if symbol.scope != 0 {
            Some(symbol.scope)
        } else {
            None
        };
        let (symbol_path, leaf_name) = symbol_path_and_leaf(&symbol.name, symbol_type);
        rows.push(NewSymbol {
            id,
            name: symbol.name.clone(),
            symbol_path,
            project_id,
            symbol_type,
            symbol_scope,
            leaf_name,
        });
    }
    Ok(rows)
}

fn build_symbol_instances(
    project_id: i32,
    objects: &[UploadObject],
    object_map: &HashMap<i64, i32>,
) -> Result<Vec<NewSymbolInstance>, UploadError> {
    let mut rows = Vec::new();
    for object in objects {
        let object_id = resolve_object_id(object_map, object.local_id)?;
        for instance in &object.symbol_instances {
            let symbol_id = compute_symbol_id(project_id, instance.symbol_local_id)?;
            let instance_type = if instance.instance_type != 0 {
                validate_instance_type(instance.instance_type)?
            } else {
                index::db_diesel::INSTANCE_TYPE_DEFINITION
            };
            rows.push(NewSymbolInstance {
                symbol: symbol_id,
                object_id,
                offset_range: instance.start_offset..instance.end_offset,
                instance_type,
            });
        }
    }
    Ok(rows)
}

fn build_symbol_refs(
    project_id: i32,
    objects: &[UploadObject],
    object_map: &HashMap<i64, i32>,
) -> Result<Vec<NewSymbolRef>, UploadError> {
    let mut rows = Vec::new();
    for object in objects {
        let object_id = resolve_object_id(object_map, object.local_id)?;
        for reference in &object.refs {
            let symbol_id = compute_symbol_id(project_id, reference.to_symbol_local_id)?;
            rows.push(NewSymbolRef {
                to_symbol: symbol_id,
                from_object: object_id,
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
            .on_conflict((
                index_schema::symbol_instances::symbol,
                index_schema::symbol_instances::object_id,
                index_schema::symbol_instances::offset_range,
            ))
            .do_nothing()
            .execute(conn)
            .await?;
    }
    Ok(())
}

async fn insert_symbol_refs(
    conn: &mut AsyncPgConnection,
    rows: &[NewSymbolRef],
) -> Result<(), UploadError> {
    for chunk in rows.chunks(MAX_INSERT_ROWS) {
        diesel::insert_into(index_schema::symbol_refs::table)
            .values(chunk)
            .on_conflict((
                index_schema::symbol_refs::to_symbol,
                index_schema::symbol_refs::from_object,
                index_schema::symbol_refs::from_offset_range,
            ))
            .do_nothing()
            .execute(conn)
            .await?;
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;

    use crate::proto::askl::index::{
        Object as UploadObject, Symbol as UploadSymbol, SymbolInstance, SymbolRef,
    };

    use super::{
        build_objects, build_symbol_instances, build_symbol_refs, build_symbols,
        compute_symbol_id, resolve_object_id, validate_instance_type, validate_symbol_type,
        validate_type,
    };

    // --- validate_type ---

    #[test]
    fn validate_type_accepts_known() {
        assert_eq!(validate_type(2, &[1, 2, 3], "x"), Ok(2));
    }

    #[test]
    fn validate_type_rejects_unknown() {
        assert!(validate_type(99, &[1, 2, 3], "x").is_err());
    }

    #[test]
    fn validate_type_error_contains_label_and_value() {
        let err = validate_type(42, &[1], "widget type").unwrap_err();
        let msg = format!("{:?}", err);
        assert!(msg.contains("widget type") && msg.contains("42"), "{}", msg);
    }

    // --- validate_symbol_type ---

    #[test]
    fn validate_symbol_type_all_valid() {
        for t in 1..=8 {
            assert!(validate_symbol_type(t).is_ok(), "type {} should be valid", t);
        }
    }

    #[test]
    fn validate_symbol_type_zero_is_invalid() {
        assert!(validate_symbol_type(0).is_err());
    }

    #[test]
    fn validate_symbol_type_out_of_range_is_invalid() {
        assert!(validate_symbol_type(9).is_err());
    }

    // --- validate_instance_type ---

    #[test]
    fn validate_instance_type_all_valid() {
        for t in 1..=10 {
            assert!(validate_instance_type(t).is_ok(), "instance type {} should be valid", t);
        }
    }

    #[test]
    fn validate_instance_type_zero_is_invalid() {
        assert!(validate_instance_type(0).is_err());
    }

    // --- resolve_object_id / compute_symbol_id ---

    #[test]
    fn resolve_object_id_found() {
        let map = HashMap::from([(42i64, 99i32)]);
        assert_eq!(resolve_object_id(&map, 42), Ok(99));
    }

    #[test]
    fn resolve_object_id_missing_is_err() {
        assert!(resolve_object_id(&HashMap::new(), 1).is_err());
    }

    #[test]
    fn compute_symbol_id_basic() {
        // project 1, local_id 10 → 1 << 32 | 10
        assert_eq!(compute_symbol_id(1, 10), Ok((1i64 << 32) | 10));
    }

    #[test]
    fn compute_symbol_id_zero_local_id() {
        assert_eq!(compute_symbol_id(5, 0), Ok(5i64 << 32));
    }

    #[test]
    fn compute_symbol_id_max_local_id() {
        let max = (1i64 << 32) - 1;
        assert!(compute_symbol_id(1, max).is_ok());
    }

    #[test]
    fn compute_symbol_id_out_of_range_is_err() {
        assert!(compute_symbol_id(1, 1i64 << 32).is_err());
        assert!(compute_symbol_id(1, -1).is_err());
    }

    // --- build_symbols ---

    fn sym(local_id: i64, name: &str, r#type: i32, scope: i32) -> UploadSymbol {
        UploadSymbol { local_id, name: name.to_string(), r#type, scope }
    }

    #[test]
    fn build_symbols_basic_fields() {
        let result = build_symbols(100, &[sym(1, "foo::bar", 1, 0)]).unwrap();
        assert_eq!(result.len(), 1);
        assert_eq!(result[0].id, (100i64 << 32) | 1);
        assert_eq!(result[0].name, "foo::bar");
        assert_eq!(result[0].project_id, 100);
        assert_eq!(result[0].symbol_type, 1);
    }

    #[test]
    fn build_symbols_zero_scope_maps_to_none() {
        let result = build_symbols(1, &[sym(1, "x", 1, 0)]).unwrap();
        assert_eq!(result[0].symbol_scope, None);
    }

    #[test]
    fn build_symbols_nonzero_scope_maps_to_some() {
        let result = build_symbols(1, &[sym(1, "x", 1, 2)]).unwrap();
        assert_eq!(result[0].symbol_scope, Some(2));
    }

    #[test]
    fn build_symbols_duplicate_local_id_is_err() {
        assert!(build_symbols(1, &[sym(1, "a", 1, 0), sym(1, "b", 1, 0)]).is_err());
    }

    #[test]
    fn build_symbols_invalid_type_is_err() {
        assert!(build_symbols(1, &[sym(1, "a", 99, 0)]).is_err());
    }

    // --- build_objects ---

    fn obj(local_id: i64, path: &str, content: &[u8], hash: &str) -> UploadObject {
        UploadObject {
            local_id,
            module_path: "mod".to_string(),
            filesystem_path: path.to_string(),
            filetype: "c".to_string(),
            content: content.to_vec(),
            content_hash: hash.to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn build_objects_inline_content_computes_hash() {
        let result = build_objects(1, &[obj(1, "/a/b.c", b"hello", "")]).unwrap();
        assert!(result[0].content.is_some());
        assert!(!result[0].row.content_hash.is_empty());
    }

    #[test]
    fn build_objects_hash_only_no_inline_content() {
        let result = build_objects(1, &[obj(1, "/a/b.c", b"", "deadbeef")]).unwrap();
        assert!(result[0].content.is_none());
        assert_eq!(result[0].row.content_hash, "deadbeef");
    }

    #[test]
    fn build_objects_client_hash_must_match_computed() {
        // Providing both content and a wrong hash is an error
        assert!(build_objects(1, &[obj(1, "/a.c", b"real", "wrong")]).is_err());
    }

    #[test]
    fn build_objects_client_hash_matching_computed_is_ok() {
        use super::super::hash_bytes;
        let content = b"data";
        let correct_hash = hash_bytes(content);
        assert!(build_objects(1, &[obj(1, "/a.c", content, &correct_hash)]).is_ok());
    }

    #[test]
    fn build_objects_duplicate_local_id_is_err() {
        assert!(build_objects(1, &[obj(1, "/a.c", b"a", ""), obj(1, "/b.c", b"b", "")]).is_err());
    }

    #[test]
    fn build_objects_relative_path_is_err() {
        assert!(build_objects(1, &[obj(1, "relative/path.c", b"x", "")]).is_err());
    }

    #[test]
    fn build_objects_normalizes_dotdot_in_path() {
        let result = build_objects(1, &[obj(1, "/a/b/../c.h", b"x", "")]).unwrap();
        assert_eq!(result[0].row.filesystem_path, "/a/c.h");
    }

    // --- build_symbol_instances ---

    fn inst(symbol_local_id: i64, instance_type: i32, start: i32, end: i32) -> SymbolInstance {
        SymbolInstance { symbol_local_id, instance_type, start_offset: start, end_offset: end }
    }

    #[test]
    fn build_symbol_instances_basic() {
        let object = UploadObject {
            local_id: 1,
            symbol_instances: vec![inst(10, 1, 5, 10)],
            ..Default::default()
        };
        let obj_map = HashMap::from([(1i64, 100i32)]);
        let rows = build_symbol_instances(7, &[object], &obj_map).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].symbol, (7i64 << 32) | 10);
        assert_eq!(rows[0].object_id, 100);
        assert_eq!(rows[0].offset_range, 5..10);
        assert_eq!(rows[0].instance_type, 1);
    }

    #[test]
    fn build_symbol_instances_zero_type_defaults_to_definition() {
        let object = UploadObject {
            local_id: 1,
            symbol_instances: vec![inst(10, 0, 0, 1)],
            ..Default::default()
        };
        let obj_map = HashMap::from([(1i64, 1i32)]);
        let rows = build_symbol_instances(1, &[object], &obj_map).unwrap();
        assert_eq!(rows[0].instance_type, index::db_diesel::INSTANCE_TYPE_DEFINITION);
    }

    #[test]
    fn build_symbol_instances_missing_object_is_err() {
        let object = UploadObject {
            local_id: 999,
            symbol_instances: vec![inst(1, 1, 0, 1)],
            ..Default::default()
        };
        assert!(build_symbol_instances(1, &[object], &HashMap::new()).is_err());
    }

    #[test]
    fn build_symbol_instances_out_of_range_symbol_is_err() {
        let object = UploadObject {
            local_id: 1,
            symbol_instances: vec![inst(1i64 << 33, 1, 0, 1)],
            ..Default::default()
        };
        let obj_map = HashMap::from([(1i64, 1i32)]);
        assert!(build_symbol_instances(1, &[object], &obj_map).is_err());
    }

    // --- build_symbol_refs ---

    fn sref(to_symbol_local_id: i64, from_start: i32, from_end: i32) -> SymbolRef {
        SymbolRef { to_symbol_local_id, from_offset_start: from_start, from_offset_end: from_end }
    }

    #[test]
    fn build_symbol_refs_basic() {
        let object = UploadObject {
            local_id: 1,
            refs: vec![sref(20, 3, 7)],
            ..Default::default()
        };
        let obj_map = HashMap::from([(1i64, 50i32)]);
        let rows = build_symbol_refs(3, &[object], &obj_map).unwrap();
        assert_eq!(rows.len(), 1);
        assert_eq!(rows[0].to_symbol, (3i64 << 32) | 20);
        assert_eq!(rows[0].from_object, 50);
        assert_eq!(rows[0].from_offset_range, 3..7);
    }

    #[test]
    fn build_symbol_refs_missing_object_is_err() {
        let object = UploadObject {
            local_id: 99,
            refs: vec![sref(1, 0, 1)],
            ..Default::default()
        };
        assert!(build_symbol_refs(1, &[object], &HashMap::new()).is_err());
    }

    #[test]
    fn build_symbol_refs_out_of_range_symbol_is_err() {
        let object = UploadObject {
            local_id: 1,
            refs: vec![sref(1i64 << 33, 0, 1)],
            ..Default::default()
        };
        let obj_map = HashMap::from([(1i64, 1i32)]);
        assert!(build_symbol_refs(1, &[object], &obj_map).is_err());
    }
}

