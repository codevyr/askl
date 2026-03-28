use actix_cors::Cors;
use actix_web::{dev::Service, web, App, HttpServer};
use askld::auth::{self, AuthStore};
use askld::cfg::ControlFlowGraph;
use askld::index_store::IndexStore;
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel_async::pooled_connection::bb8::Pool as AsyncPool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::AsyncPgConnection;

use index::db_diesel::Index;
use log::info;
use tracing_chrome::ChromeLayerBuilder;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::api;
use crate::api::types::AsklData;
use crate::args::ServeArgs;

fn build_cors() -> Cors {
    let mut cors = Cors::default()
        .allowed_methods(vec!["GET", "POST", "PUT", "PATCH", "DELETE", "OPTIONS"])
        .allow_any_header()
        .max_age(3600);

    match std::env::var("ASKL_CORS_ORIGINS") {
        Ok(origins) => {
            let mut added = false;
            for origin in origins.split(',') {
                let origin = origin.trim();
                if origin.is_empty() {
                    continue;
                }
                cors = cors.allowed_origin(origin);
                added = true;
            }
            if !added {
                cors = cors.allow_any_origin();
            }
            cors
        }
        Err(_) => cors.allow_any_origin(),
    }
}

pub async fn run(serve_args: ServeArgs) -> std::io::Result<()> {
    let _guard = if let Some(trace_dir) = &serve_args.trace {
        use chrono::prelude::*;
        let trace_file = format!("trace-{}.json", Local::now().format("%Y%m%d-%H%M%S"),);
        let trace_path = std::path::Path::new(trace_dir).join(trace_file);
        if trace_path.exists() {
            std::fs::remove_file(&trace_path).expect("Failed to remove old trace file");
        }
        let (chrome_layer, _guard) = ChromeLayerBuilder::new()
            .file(trace_path)
            .include_args(true)
            .trace_style(tracing_chrome::TraceStyle::Async)
            .build();
        let filter =
            EnvFilter::new("info,askld=trace,actix_http=off,actix_web=warn,tracing_actix_web=warn");
        tracing_subscriber::registry()
            .with(filter)
            .with(tracing_subscriber::fmt::layer())
            .with(chrome_layer)
            .init();

        diesel::connection::set_default_instrumentation(|| {
            Some(Box::new(askld::tracing_instrumentation::TracingInstrumentation::new()))
        }).expect("Failed to set diesel instrumentation");

        info!("Tracing enabled, writing to {}", trace_dir);
        Some(_guard)
    } else {
        env_logger::init();

        None
    };

    // Sync pool for Index (which uses sync diesel throughout)
    let sync_manager = ConnectionManager::<PgConnection>::new(&serve_args.database_url);
    let sync_pool = Pool::builder()
        .build(sync_manager)
        .expect("Failed to build sync database pool");

    // Async pool for IndexStore and AuthStore
    let async_config =
        AsyncDieselConnectionManager::<AsyncPgConnection>::new(&serve_args.database_url);
    let async_pool: AsyncPool<AsyncPgConnection> = AsyncPool::builder()
        .build(async_config)
        .await
        .expect("Failed to build async database pool");

    let auth_store = AuthStore::from_pool(async_pool.clone(), &serve_args.database_url)
        .expect("Failed to initialize auth store");
    let auth_store = web::Data::new(auth_store);

    let index_store = IndexStore::from_pool(async_pool.clone());
    let index_store = web::Data::new(index_store);

    let index_query = Index::from_pool(sync_pool).expect("Failed to initialize index");
    let askl_data = web::Data::new(AsklData {
        cfg: ControlFlowGraph::from_symbols(index_query),
    });

    info!(
        "Starting server on {}:{}...",
        serve_args.host, serve_args.port
    );

    HttpServer::new(move || {
        App::new()
            .wrap(build_cors())
            .wrap(tracing_actix_web::TracingLogger::default())
            .wrap_fn(|mut req, srv| {
                auth::redact_auth_headers(&mut req);
                let fut = srv.call(req);
                async move { fut.await }
            })
            .app_data(askl_data.clone())
            .app_data(auth_store.clone())
            .app_data(index_store.clone())
            .configure(api::configure)
    })
    .bind((serve_args.host, serve_args.port))?
    .run()
    .await
}
