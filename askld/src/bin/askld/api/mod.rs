use actix_web::{get, web, HttpResponse, Responder};

pub mod auth;
pub mod index;
pub mod query;
pub mod types;

#[get("/version")]
async fn version() -> impl Responder {
    HttpResponse::Ok().body(env!("CARGO_PKG_VERSION"))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(version)
        .service(auth::create_api_key)
        .service(auth::revoke_api_key)
        .service(auth::list_api_keys)
        .service(
            web::resource("/v1/index/projects")
                .app_data(web::PayloadConfig::new(index::MAX_UPLOAD_BYTES))
                .route(web::get().to(index::list_index_projects))
                .route(web::post().to(index::upload_index)),
        )
        .service(index::get_index_project)
        .service(index::delete_index_project)
        .service(index::get_project_tree)
        .service(index::resolve_project_path)
        .service(index::get_project_source)
        .service(query::query)
        .service(query::file);
}
