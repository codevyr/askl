use actix_web::{dev::Service, web, App, HttpServer};
use askld::auth::{self, AuthStore};
use askld::cfg::ControlFlowGraph;
use askld::index_store::IndexStore;
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use index::db_diesel::Index;
use log::info;
use tracing_chrome::ChromeLayerBuilder;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

use crate::api;
use crate::api::types::AsklData;
use crate::args::ServeArgs;

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
        tracing_subscriber::registry()
            .with(tracing_subscriber::fmt::layer())
            .with(chrome_layer)
            .init();

        info!("Tracing enabled, writing to {}", trace_dir);
        Some(_guard)
    } else {
        env_logger::init();

        None
    };

    let manager = ConnectionManager::<PgConnection>::new(&serve_args.database_url);
    let pool = Pool::builder()
        .build(manager)
        .expect("Failed to build database pool");

    let auth_store = AuthStore::from_pool(pool.clone()).expect("Failed to initialize auth store");
    let auth_store = web::Data::new(auth_store);

    let index_store = IndexStore::from_pool(pool.clone());
    let index_store = web::Data::new(index_store);

    let index_query = Index::from_pool(pool.clone()).expect("Failed to initialize index");
    let askl_data = web::Data::new(AsklData {
        cfg: ControlFlowGraph::from_symbols(index_query),
    });

    info!(
        "Starting server on {}:{}...",
        serve_args.host, serve_args.port
    );

    HttpServer::new(move || {
        App::new()
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
