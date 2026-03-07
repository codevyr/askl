use actix_web::{get, web, HttpResponse, Responder};

pub mod auth;
pub mod index;
pub mod mcp;
pub mod query;
pub mod types;

#[get("/version")]
async fn version() -> impl Responder {
    HttpResponse::Ok().body(env!("CARGO_PKG_VERSION"))
}

pub fn configure(cfg: &mut web::ServiceConfig) {
    // Create SSE session store for MCP
    let sse_sessions = mcp::new_sse_session_store();

    cfg.service(version)
        .service(auth::create_api_key)
        .service(auth::revoke_api_key)
        .service(auth::list_api_keys)
        .service(
            web::resource("/v1/index/projects")
                .app_data(web::PayloadConfig::new(index::max_upload_bytes()))
                .route(web::get().to(index::list_index_projects))
                .route(web::post().to(index::upload_index)),
        )
        .service(index::get_index_project)
        .service(index::delete_index_project)
        .service(index::get_project_tree)
        .service(index::get_project_source)
        .service(query::query)
        .service(query::file)
        // MCP endpoints
        .app_data(web::Data::new(sse_sessions.clone()))
        .service(
            web::resource("/mcp")
                .app_data(web::PayloadConfig::new(mcp::MAX_MCP_REQUEST_BYTES))
                .route(web::post().to(mcp::mcp_handler)),
        )
        .service(web::resource("/mcp/sse").route(web::get().to(mcp::mcp_sse_handler)))
        .service(
            web::resource("/mcp/session/{session_id}")
                .app_data(web::PayloadConfig::new(mcp::MAX_MCP_REQUEST_BYTES))
                .route(web::post().to(mcp::mcp_session_handler)),
        );
}
