use actix_web::{delete, get, http::header, web, HttpRequest, HttpResponse, Responder};
use askld::auth::AuthIdentity;
use askld::index_store::{IndexStore, ProjectTreeResult, StoreError, UploadError};
use askld::proto::askl::index::Project;
use log::error;
use prost::Message;
use serde::{Deserialize, Serialize};

use super::types::{IndexDeleteResponse, IndexUploadResponse};

pub const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

pub async fn upload_index(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    req: HttpRequest,
    body: web::Bytes,
) -> impl Responder {
    let _upload_span: tracing::span::EnteredSpan =
        tracing::info_span!("index_upload", bytes = body.len()).entered();
    let content_type = req
        .headers()
        .get(header::CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .unwrap_or("");
    if !content_type.starts_with("application/x-protobuf") {
        return HttpResponse::UnsupportedMediaType().body("Expected application/x-protobuf");
    }

    let upload = match Project::decode(body.as_ref()) {
        Ok(upload) => upload,
        Err(err) => {
            return HttpResponse::BadRequest()
                .body(format!("Failed to decode protobuf payload: {}", err));
        }
    };

    match store.upload_index(upload).await {
        Ok(project_id) => HttpResponse::Created()
            .append_header((header::LOCATION, format!("/v1/index/projects/{}", project_id)))
            .json(IndexUploadResponse { project_id }),
        Err(UploadError::Conflict) => HttpResponse::Conflict().body("Project already exists"),
        Err(UploadError::Invalid(message)) => HttpResponse::BadRequest().body(message),
        Err(UploadError::Storage(message)) => {
            error!("Index upload failed: {}", message);
            HttpResponse::InternalServerError().body("Failed to upload index")
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

    let mut path = query.path.clone().unwrap_or_else(|| "/".to_string());
    if path.is_empty() {
        path = "/".to_string();
    }
    if !path.starts_with('/') {
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

    let base_nodes = match store.list_project_tree(*project_id, &path, compact).await {
        Ok(ProjectTreeResult::Nodes(nodes)) => nodes,
        Ok(ProjectTreeResult::ProjectNotFound) => {
            return HttpResponse::NotFound().body("Project not found");
        }
        Ok(ProjectTreeResult::NotDirectory) => {
            return HttpResponse::BadRequest().body("path is not a directory");
        }
        Err(StoreError::Storage(message)) => {
            error!(
                "Failed to load project tree {}: {}",
                project_id, message
            );
            return HttpResponse::InternalServerError().body("Failed to load project tree");
        }
    };

    let mut expanded = std::collections::HashMap::new();
    for expand_path in &query.expand {
        let nodes = match store
            .list_project_tree(*project_id, expand_path, compact)
            .await
        {
            Ok(ProjectTreeResult::Nodes(nodes)) => nodes,
            Ok(ProjectTreeResult::ProjectNotFound) => {
                return HttpResponse::NotFound().body("Project not found");
            }
            Ok(ProjectTreeResult::NotDirectory) => {
                return HttpResponse::BadRequest()
                    .body(format!("expand path is not a directory: {}", expand_path));
            }
            Err(StoreError::Storage(message)) => {
                error!(
                    "Failed to load project tree {}: {}",
                    project_id, message
                );
                return HttpResponse::InternalServerError().body("Failed to load project tree");
            }
        };
        expanded.insert(expand_path.clone(), nodes);
    }

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
