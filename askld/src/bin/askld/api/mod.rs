use actix_web::{get, web, HttpResponse, Responder};
use askld::auth::AuthIdentity;

pub mod auth;
pub mod index;
pub mod query;
pub mod types;

#[get("/version")]
async fn version(_identity: AuthIdentity) -> impl Responder {
    HttpResponse::Ok().body(env!("CARGO_PKG_VERSION"))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(version)
        .service(auth::create_api_key)
        .service(auth::revoke_api_key)
        .service(auth::list_api_keys)
        .service(
            web::resource("/v1/index/upload")
                .app_data(web::PayloadConfig::new(index::MAX_UPLOAD_BYTES))
                .route(web::post().to(index::upload_index)),
        )
        .service(index::list_index_projects)
        .service(index::get_index_project)
        .service(index::delete_index_project)
        .service(query::query)
        .service(query::file);
}
