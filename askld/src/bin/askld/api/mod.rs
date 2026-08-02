use actix_web::{get, guard, post, web, HttpResponse, Responder};
use askld::index_store::IndexStore;

pub mod auth;
pub mod index;
pub mod mcp;
pub mod query;
pub mod render;
pub mod types;

#[get("/version")]
async fn version() -> impl Responder {
    HttpResponse::Ok().body(env!("CARGO_PKG_VERSION"))
}

/// Route guard for the `/admin/local` scope: match only when the peer is
/// loopback (127.0.0.1 / ::1). A remote request doesn't match, so the whole
/// local-admin surface returns 404 — invisible to outsiders.
fn loopback_only(ctx: &guard::GuardContext<'_>) -> bool {
    ctx.head()
        .peer_addr
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
}

/// `POST /admin/local/clear-cache` — clear the RAM SQL cache + purge the DB
/// ephemeral layers so the next query is cold (perf-test harness reset, no
/// restart). Loopback-only via the scope guard.
#[post("/clear-cache")]
async fn clear_cache(store: web::Data<IndexStore>) -> impl Responder {
    match store.clear_caches().await {
        Ok(purged) => HttpResponse::Ok().json(serde_json::json!({ "purged_eph_layers": purged })),
        Err(err) => {
            HttpResponse::InternalServerError().body(format!("clear-cache failed: {:?}\n", err))
        }
    }
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    cfg.service(version)
        // Local-admin surface: everything under /admin/local is loopback-only
        // (the guard makes remote requests 404). Credential ops additionally
        // require ASKL_BOOTSTRAP_MODE, checked in their handlers.
        .service(
            web::scope("/admin/local")
                .guard(guard::fn_guard(loopback_only))
                .service(clear_cache)
                .service(auth::create_api_key)
                .service(auth::revoke_api_key)
                .service(auth::list_api_keys),
        )
        .service(
            web::resource("/v1/index/projects")
                .app_data(web::PayloadConfig::new(index::max_upload_bytes()))
                .route(web::get().to(index::list_index_projects))
                .route(web::post().to(index::upload_index)),
        )
        .service(
            web::resource("/v1/index/contents")
                .app_data(web::PayloadConfig::new(index::max_upload_bytes()))
                .route(web::post().to(index::upload_contents)),
        )
        .service(
            web::resource("/v1/index/contents/check").route(web::post().to(index::check_contents)),
        )
        .service(
            web::resource("/v1/index/projects/{project_id}/symbols")
                .app_data(web::PayloadConfig::new(index::max_upload_bytes()))
                .route(web::post().to(index::upload_symbol_chunk)),
        )
        .service(
            web::resource("/v1/index/projects/{project_id}/objects")
                .app_data(web::PayloadConfig::new(index::max_upload_bytes()))
                .route(web::post().to(index::append_project_objects)),
        )
        .service(
            web::resource("/v1/index/projects/{project_id}/finalize")
                .route(web::post().to(index::finalize_project)),
        )
        .service(index::get_index_project)
        .service(index::delete_index_project)
        .service(index::get_project_tree)
        .service(index::get_project_source)
        .service(query::query)
        .service(query::file)
        .service(web::resource("/mcp").route(web::post().to(mcp::mcp_handler)));
}
