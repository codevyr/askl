use actix_web::{delete, get, http::header, web, HttpRequest, HttpResponse, Responder};
use tracing::Instrument;
use askld::auth::AuthIdentity;
use askld::index_store::{normalize_full_path, IndexStore, MultiTreeResult, StoreError, UploadError};
use askld::proto::askl::index::{ContentBatch, Project};
use log::{error, warn};
use prost::Message;
use serde::{Deserialize, Serialize};

use super::types::{IndexDeleteResponse, IndexUploadResponse};

pub const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;
const MAX_UPLOAD_ENV: &str = "ASKL_MAX_UPLOAD_BYTES";

pub fn max_upload_bytes() -> usize {
    match std::env::var(MAX_UPLOAD_ENV) {
        Ok(raw) => match raw.parse::<u64>() {
            Ok(value) if value > 0 => match usize::try_from(value) {
                Ok(value) => value,
                Err(_) => {
                    warn!(
                        "{} is too large for this platform; using default {}",
                        MAX_UPLOAD_ENV, MAX_UPLOAD_BYTES
                    );
                    MAX_UPLOAD_BYTES
                }
            },
            _ => {
                warn!(
                    "Invalid {} value '{}'; using default {}",
                    MAX_UPLOAD_ENV, raw, MAX_UPLOAD_BYTES
                );
                MAX_UPLOAD_BYTES
            }
        },
        Err(_) => MAX_UPLOAD_BYTES,
    }
}

fn require_protobuf(req: &HttpRequest) -> Result<(), HttpResponse> {
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if content_type.starts_with("application/x-protobuf") {
        Ok(())
    } else {
        Err(HttpResponse::UnsupportedMediaType().body("Expected application/x-protobuf"))
    }
}

#[derive(Debug, Deserialize)]
pub struct UploadIndexQuery {
    pub symbol_chunks: Option<i32>,
    pub object_chunks: Option<i32>,
}

#[derive(Debug, Deserialize)]
pub struct ChunkSeqQuery {
    pub seq: i32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CheckHashesRequest {
    pub hashes: Vec<String>,
}

#[derive(Debug, Serialize)]
pub struct CheckHashesResponse {
    pub present: Vec<String>,
}

pub async fn upload_index(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    query: web::Query<UploadIndexQuery>,
    req: HttpRequest,
    body: web::Bytes,
) -> impl Responder {
    let _upload_span: tracing::span::EnteredSpan =
        tracing::info_span!("index_upload", bytes = body.len()).entered();
    if let Err(resp) = require_protobuf(&req) {
        return resp;
    }

    let upload = match Project::decode(body.as_ref()) {
        Ok(upload) => upload,
        Err(err) => {
            return HttpResponse::BadRequest()
                .body(format!("Failed to decode protobuf payload: {}", err));
        }
    };

    match store
        .upload_index(upload, query.symbol_chunks, query.object_chunks)
        .await
    {
        Ok((project_id, resumed)) => {
            let body = IndexUploadResponse { project_id, resumed };
            if resumed {
                HttpResponse::Ok()
                    .append_header((header::LOCATION, format!("/v1/index/projects/{}", project_id)))
                    .json(body)
            } else {
                HttpResponse::Created()
                    .append_header((header::LOCATION, format!("/v1/index/projects/{}", project_id)))
                    .json(body)
            }
        }
        Err(UploadError::Conflict) => HttpResponse::Conflict().body("Project already exists"),
        Err(UploadError::Invalid(message)) => HttpResponse::BadRequest().body(message),
        Err(UploadError::Storage(message)) => {
            error!("Index upload failed: {}", message);
            HttpResponse::InternalServerError().body("Failed to upload index")
        }
    }
}

pub async fn upload_symbol_chunk(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
    query: web::Query<ChunkSeqQuery>,
    req: HttpRequest,
    body: web::Bytes,
) -> impl Responder {
    if let Err(resp) = require_protobuf(&req) {
        return resp;
    }
    let upload = match Project::decode(body.as_ref()) {
        Ok(u) => u,
        Err(err) => {
            return HttpResponse::BadRequest()
                .body(format!("Failed to decode protobuf payload: {}", err));
        }
    };
    if !upload.objects.is_empty() {
        return HttpResponse::BadRequest()
            .body("objects must be uploaded via POST /v1/index/projects/{id}/objects");
    }
    let seq = query.seq;
    match store
        .upload_symbol_chunk(*project_id, seq, upload.symbols)
        .await
    {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({ "seq": seq })),
        Err(UploadError::Invalid(msg)) => HttpResponse::BadRequest().body(msg),
        Err(UploadError::Storage(msg)) => {
            error!("Symbol chunk upload failed (project={} seq={}): {}", project_id, seq, msg);
            HttpResponse::InternalServerError().body("Failed to upload symbol chunk")
        }
        Err(UploadError::Conflict) => HttpResponse::Conflict().finish(),
    }
}

pub async fn finalize_project(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
) -> impl Responder {
    match store.finalize_project(*project_id).await {
        Ok(true) => HttpResponse::Ok().finish(),
        Ok(false) => HttpResponse::NotFound().body("Project not found"),
        Err(UploadError::Conflict) => {
            HttpResponse::Conflict().body("Project is not in uploading state")
        }
        Err(UploadError::Storage(msg)) => {
            error!("Failed to finalize project {}: {}", project_id, msg);
            HttpResponse::InternalServerError().body("Failed to finalize project")
        }
        Err(e) => {
            error!("Unexpected error finalizing project {}: {:?}", project_id, e);
            HttpResponse::InternalServerError().body("Unexpected error")
        }
    }
}

pub async fn append_project_objects(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
    query: web::Query<ChunkSeqQuery>,
    req: HttpRequest,
    body: web::Bytes,
) -> impl Responder {
    if let Err(resp) = require_protobuf(&req) {
        return resp;
    }
    let upload = match Project::decode(body.as_ref()) {
        Ok(u) => u,
        Err(err) => {
            return HttpResponse::BadRequest()
                .body(format!("Failed to decode protobuf payload: {}", err));
        }
    };
    let seq = query.seq;
    match store.upload_object_chunk(*project_id, seq, upload).await {
        Ok(()) => HttpResponse::Ok().json(serde_json::json!({ "seq": seq })),
        Err(UploadError::Invalid(msg)) => HttpResponse::BadRequest().body(msg),
        Err(UploadError::Storage(msg)) => {
            error!("Object chunk upload failed (project={} seq={}): {}", project_id, seq, msg);
            HttpResponse::InternalServerError().body("Failed to upload objects")
        }
        Err(UploadError::Conflict) => HttpResponse::Conflict().finish(),
    }
}

pub async fn upload_contents(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    req: HttpRequest,
    body: web::Bytes,
) -> impl Responder {
    if let Err(resp) = require_protobuf(&req) {
        return resp;
    }

    let batch = match ContentBatch::decode(body.as_ref()) {
        Ok(batch) => batch,
        Err(err) => {
            return HttpResponse::BadRequest()
                .body(format!("Failed to decode protobuf payload: {}", err));
        }
    };

    match store.upload_contents(batch).await {
        Ok(new_count) => HttpResponse::Ok().json(serde_json::json!({ "new_entries": new_count })),
        Err(UploadError::Invalid(message)) => HttpResponse::BadRequest().body(message),
        Err(UploadError::Storage(message)) => {
            error!("Content upload failed: {}", message);
            HttpResponse::InternalServerError().body("Failed to upload contents")
        }
        Err(UploadError::Conflict) => {
            HttpResponse::InternalServerError().body("Unexpected conflict")
        }
    }
}

pub async fn check_contents(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    body: web::Json<CheckHashesRequest>,
) -> impl Responder {
    match store.check_content_hashes(body.into_inner().hashes).await {
        Ok(present) => HttpResponse::Ok().json(CheckHashesResponse { present }),
        Err(StoreError::Storage(message)) => {
            error!("Content hash check failed: {}", message);
            HttpResponse::InternalServerError().body("Failed to check content hashes")
        }
    }
}

pub async fn list_index_projects(
    store: web::Data<IndexStore>,
) -> impl Responder {
    match store.list_projects().await {
        Ok(projects) => HttpResponse::Ok().json(projects),
        Err(StoreError::Storage(message)) => {
            error!("Failed to list projects: {}", message);
            HttpResponse::InternalServerError().body("Failed to list projects")
        }
    }
}

#[get("/v1/index/projects/{project_id}")]
pub async fn get_index_project(
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
) -> impl Responder {
    match store.get_project_details(*project_id).await {
        Ok(Some(details)) => HttpResponse::Ok().json(details),
        Ok(None) => HttpResponse::NotFound().body("Project not found"),
        Err(StoreError::Storage(message)) => {
            error!("Failed to load project {}: {}", project_id, message);
            HttpResponse::InternalServerError().body("Failed to load project")
        }
    }
}

#[delete("/v1/index/projects/{project_id}")]
pub async fn delete_index_project(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
) -> impl Responder {
    match store.delete_project(*project_id).await {
        Ok(true) => HttpResponse::Ok().json(IndexDeleteResponse {
            project_id: *project_id,
            deleted: true,
        }),
        Ok(false) => HttpResponse::NotFound().body("Project not found"),
        Err(StoreError::Storage(message)) => {
            error!("Failed to delete project {}: {}", project_id, message);
            HttpResponse::InternalServerError().body("Failed to delete project")
        }
    }
}

#[derive(Debug, Deserialize)]
pub struct TreeQuery {
    path: Option<String>,
    #[serde(default)]
    expand: Vec<String>,
    compact: Option<u8>,
}

#[get("/v1/index/projects/{project_id}/tree")]
pub async fn get_project_tree(
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
    req: HttpRequest,
) -> impl Responder {
    let query = if req.query_string().is_empty() {
        TreeQuery {
            path: None,
            expand: Vec::new(),
            compact: None,
        }
    } else {
        let qs_config = serde_qs::Config::new(5, false);
        match qs_config.deserialize_str::<TreeQuery>(req.query_string()) {
            Ok(query) => query,
            Err(err) => {
                return HttpResponse::BadRequest()
                    .body(format!("Query deserialize error: {}", err));
            }
        }
    };

    let raw_path = query.path.clone().unwrap_or_else(|| "/".to_string());
    if raw_path.is_empty() || !raw_path.starts_with('/') {
        return HttpResponse::BadRequest().body("path must be an absolute path");
    }
    let compact = match query.compact {
        None => true,
        Some(0) => false,
        Some(1) => true,
        Some(_) => return HttpResponse::BadRequest().body("compact must be 0 or 1"),
    };
    for expand_path in &query.expand {
        if !expand_path.starts_with('/') {
            return HttpResponse::BadRequest().body("expand must be absolute paths");
        }
    }

    // Normalize all paths so that HashMap keys match what the store returns.
    let path = normalize_full_path(&raw_path);
    let expand_normalized: Vec<String> = query.expand.iter().map(|p| normalize_full_path(p)).collect();

    let mut all_paths = vec![path.clone()];
    all_paths.extend(expand_normalized.iter().cloned());

    let span = tracing::info_span!(
        "project_tree",
        project_id = *project_id,
        path = %path,
        compact,
        expand_count = query.expand.len(),
    );

    let mut all_nodes = match store
        .list_project_tree_multi(*project_id, &all_paths, compact)
        .instrument(span)
        .await
    {
        Ok(MultiTreeResult::Nodes(map)) => map,
        Ok(MultiTreeResult::ProjectNotFound) => {
            return HttpResponse::NotFound().body("Project not found");
        }
        Ok(MultiTreeResult::NotReady) => {
            return HttpResponse::Conflict().body("Project upload is not complete");
        }
        Ok(MultiTreeResult::NotDirectory(p)) if p == path => {
            return HttpResponse::BadRequest().body("path is not a directory");
        }
        Ok(MultiTreeResult::NotDirectory(p)) => {
            return HttpResponse::BadRequest()
                .body(format!("expand path is not a directory: {}", p));
        }
        Err(StoreError::Storage(message)) => {
            error!("Failed to load project tree {}: {}", project_id, message);
            return HttpResponse::InternalServerError().body("Failed to load project tree");
        }
    };

    let base_nodes = all_nodes.remove(&path).unwrap_or_default();
    let expanded: std::collections::HashMap<String, _> = expand_normalized
        .iter()
        .filter_map(|p| all_nodes.remove(p).map(|nodes| (p.clone(), nodes)))
        .collect();

    tracing::debug!(nodes = base_nodes.len(), expanded = expanded.len(), "project_tree complete");

    let response = TreeResponse {
        base_path: path,
        nodes: base_nodes,
        expanded,
    };
    HttpResponse::Ok().json(response)
}

#[derive(Debug, Serialize)]
struct TreeResponse {
    base_path: String,
    nodes: Vec<askld::index_store::ProjectTreeNode>,
    expanded: std::collections::HashMap<String, Vec<askld::index_store::ProjectTreeNode>>,
}

#[derive(Debug, Deserialize)]
pub struct SourceQuery {
    path: String,
    start_offset: Option<i64>,
    end_offset: Option<i64>,
}

#[get("/v1/index/projects/{project_id}/source")]
pub async fn get_project_source(
    store: web::Data<IndexStore>,
    project_id: web::Path<i32>,
    query: web::Query<SourceQuery>,
) -> impl Responder {
    let path = query.path.trim();
    if path.is_empty() {
        return HttpResponse::BadRequest().body("path is required");
    }
    if !path.starts_with('/') {
        return HttpResponse::BadRequest().body("path must be an absolute path");
    }

    let content = match store
        .get_project_file_contents_by_path(*project_id, path)
        .await
    {
        Ok(Some(content)) => content,
        Ok(None) => return HttpResponse::NotFound().body("File not found"),
        Err(StoreError::Storage(message)) => {
            error!(
                "Failed to load project source {}: {}",
                project_id, message
            );
            return HttpResponse::InternalServerError().body("Failed to load project source");
        }
    };

    match slice_content(content, query.start_offset, query.end_offset) {
        Ok(slice) => HttpResponse::Ok().body(slice),
        Err(response) => response,
    }
}

fn slice_content(
    content: Vec<u8>,
    start_offset: Option<i64>,
    end_offset: Option<i64>,
) -> Result<Vec<u8>, HttpResponse> {
    let len = content.len();
    let start = start_offset.unwrap_or(0);
    let end = end_offset.unwrap_or(len as i64);
    if start < 0 || end < 0 {
        return Err(HttpResponse::BadRequest().body("Offsets must be non-negative"));
    }
    let start = start as usize;
    let end = end as usize;
    if start > end || end > len {
        return Err(HttpResponse::BadRequest().body("Invalid offset range"));
    }
    Ok(content[start..end].to_vec())
}
