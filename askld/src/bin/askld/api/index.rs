use actix_web::{delete, get, http::header, web, HttpRequest, HttpResponse, Responder};
use askld::auth::AuthIdentity;
use askld::index_store::{IndexStore, StoreError, UploadError};
use askld::proto::askl::index::Project;
use log::error;
use prost::Message;

use super::types::{IndexDeleteResponse, IndexUploadResponse};

pub const MAX_UPLOAD_BYTES: usize = 256 * 1024 * 1024;

pub async fn upload_index(
    _identity: AuthIdentity,
    store: web::Data<IndexStore>,
    req: HttpRequest,
    body: web::Bytes,
) -> impl Responder {
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
    _identity: AuthIdentity,
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
    _identity: AuthIdentity,
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
