use actix_cors::Cors;
use actix_web::{dev::Service, web, App, HttpServer};
use askld::auth::{self, AuthStore};
use askld::cfg::ControlFlowGraph;
use askld::index_store::IndexStore;
use diesel::pg::PgConnection;
use diesel_async::pooled_connection::bb8::Pool as AsyncPool;
use diesel_async::pooled_connection::AsyncDieselConnectionManager;
use diesel_async::{AsyncConnection, AsyncPgConnection};
use diesel_migrations::MigrationHarness;
use futures::FutureExt;

use index::db_diesel::Index;
use log::{info, warn};
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
        std::fs::create_dir_all(trace_dir).expect("Failed to create trace directory");
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
            Some(Box::new(
                askld::tracing_instrumentation::TracingInstrumentation::new(),
            ))
        })
        .expect("Failed to set diesel instrumentation");

        info!("Tracing enabled, writing to {}", trace_dir);
        Some(_guard)
    } else {
        env_logger::init();

        None
    };

    // Run migrations with a sync connection
    {
        use diesel::Connection;
        let mut connection = PgConnection::establish(&serve_args.database_url)
            .expect("Failed to establish migration connection");
        connection
            .run_pending_migrations(index::db_diesel::MIGRATIONS)
            .expect("Failed to run migrations");
    }

    // Async pool for IndexStore and AuthStore (no statement_timeout).
    // The shared config carries the recycling ROLLBACK (load-bearing for
    // ephemeral-layer cancellation safety) and the idle-in-transaction
    // session timeout — see eph_pool_manager_config rustdoc.
    let async_config = AsyncDieselConnectionManager::new_with_config(
        &serve_args.database_url,
        index::db_diesel::eph_pool_manager_config(),
    );
    let async_pool: AsyncPool<AsyncPgConnection> = AsyncPool::builder()
        .test_on_check_out(false)
        .build(async_config)
        .await
        .expect("Failed to build async database pool");

    // Async pool for Index queries (with statement_timeout).  Starts from
    // the shared eph config but overrides custom_setup to ALSO set the
    // per-query statement_timeout; the idle-in-transaction timeout must be
    // re-applied here because a replaced custom_setup does not compose.
    let query_timeout_secs = serve_args.query_timeout;
    let query_timeout_ms = query_timeout_secs * 1000;
    let mut index_pool_config = index::db_diesel::eph_pool_manager_config();
    index_pool_config.custom_setup = Box::new(move |url| {
        async move {
            let mut conn = AsyncPgConnection::establish(url).await?;
            diesel_async::RunQueryDsl::<AsyncPgConnection>::execute(
                diesel::sql_query(&format!("SET statement_timeout = {}", query_timeout_ms)),
                &mut conn,
            )
            .await
            .map_err(diesel::ConnectionError::CouldntSetupConfiguration)?;
            diesel_async::RunQueryDsl::<AsyncPgConnection>::execute(
                diesel::sql_query(&format!(
                    "SET idle_in_transaction_session_timeout = '{}'",
                    index::db_diesel::EPH_POOL_IDLE_IN_TXN_TIMEOUT
                )),
                &mut conn,
            )
            .await
            .map_err(diesel::ConnectionError::CouldntSetupConfiguration)?;
            Ok(conn)
        }
        .boxed()
    });
    let index_config =
        AsyncDieselConnectionManager::new_with_config(&serve_args.database_url, index_pool_config);
    let index_pool: AsyncPool<AsyncPgConnection> = AsyncPool::builder()
        .test_on_check_out(false)
        .build(index_config)
        .await
        .expect("Failed to build index database pool");

    let auth_store = AuthStore::from_pool(async_pool.clone(), &serve_args.database_url)
        .expect("Failed to initialize auth store");
    let auth_store = web::Data::new(auth_store);

    // One shared SQL result cache: the query side (Index) reads it, the
    // mutation side (IndexStore: finalize/delete project) clears it.
    let sql_cache = index::db_diesel::SqlResultCache::new(serve_args.sql_cache_bytes);
    let index_store = IndexStore::from_pool_with_cache(async_pool.clone(), sql_cache.clone());
    let index_store = web::Data::new(index_store);

    let index_query = Index::from_pool_with_cache(index_pool, sql_cache.clone());
    index_query
        .validate_canary()
        .await
        .expect("ephemeral leak-detection canary missing — re-apply migrations");
    // Planner-statistics refresh, after the canary (a correctness gate) and
    // before we listen: serving with statistics describing a two-project
    // database is how a 0.4 ms query became a 120 s timeout.
    //
    // Unlike the canary this is NEVER fatal — stale statistics cost speed,
    // not correctness, and a deployment whose role cannot ANALYZE must still
    // boot.  `ensure_planner_stats` returns `()` so that is a type-level
    // guarantee rather than a promise made here.
    //
    // Its own connection, not a pooled one: a tokio timeout does not cancel a
    // server-side ANALYZE, so a pooled connection would return to bb8 with a
    // query still in flight.  Dropping a standalone connection takes the
    // query with it.
    //
    // This call site is verified by inspection — `run` binds a port and is
    // not unit-testable, which is why the logic lives in a free function over
    // a connection (index::db_diesel::stats, tested there and in all_tests).
    run_boot_analyze(&serve_args).await;

    let askl_data = web::Data::new(AsklData {
        cfg: ControlFlowGraph::from_symbols(index_query),
        query_timeout: std::time::Duration::from_secs(query_timeout_secs),
        max_result_symbols: serve_args.max_result_symbols,
        probe_cap: serve_args.probe_cap,
    });

    // Background GC: periodically purge ephemeral layers idle past the TTL.
    //
    // Interval tuned so the table is checked frequently enough to bound peak
    // size, but not so often that the DB sees idle DELETEs.  TTL chosen so a
    // single user query session can re-use a cached layer across iterations
    // without the layer being evicted between requests.
    const EPH_GC_INTERVAL_SECS: u64 = 600; // 10 min: how often we scan
    const EPH_GC_TTL_SECS: u64 = 3600; // 1 h: minimum idle age to delete
    let gc_index = askl_data.cfg.index.clone();
    tokio::spawn(async move {
        let interval = std::time::Duration::from_secs(EPH_GC_INTERVAL_SECS);
        let ttl = std::time::Duration::from_secs(EPH_GC_TTL_SECS);
        let mut consecutive_failures: u32 = 0;
        let mut shutdown = Box::pin(async {
            let _ = tokio::signal::ctrl_c().await;
        });
        loop {
            tokio::select! {
                biased;
                _ = &mut shutdown => {
                    info!("GC: shutdown signal received, exiting");
                    break;
                }
                _ = tokio::time::sleep(interval) => {}
            }
            match gc_index.purge_old_eph_layers(ttl).await {
                Ok(n) => {
                    consecutive_failures = 0;
                    info!("GC: purged {} ephemeral layers", n);
                }
                Err(e) => {
                    consecutive_failures = consecutive_failures.saturating_add(1);
                    // Escalate as failures pile up: a single hiccup is noise,
                    // but a stuck GC is a real availability problem.
                    if consecutive_failures >= 3 {
                        tracing::error!(
                            consecutive_failures, error = %e,
                            "GC: ephemeral layer purge persistently failing"
                        );
                    } else {
                        warn!(
                            "GC: ephemeral layer purge failed (attempt {}): {}",
                            consecutive_failures, e
                        );
                    }
                }
            }
        }
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

/// Boot-time planner-statistics pass: connect, refresh, disconnect.
///
/// Bounded by `--boot-analyze-timeout` so a slow refresh cannot stall
/// startup past a container health check — a restart loop would mean more
/// crash recovery, which is what wiped the autovacuum counters in the first
/// place.
async fn run_boot_analyze(serve_args: &ServeArgs) {
    use diesel_async::{AsyncConnection, AsyncPgConnection};

    let mode: index::db_diesel::BootAnalyze = serve_args.boot_analyze.into();
    if mode == index::db_diesel::BootAnalyze::Off {
        return;
    }

    let mut conn = match AsyncPgConnection::establish(&serve_args.database_url).await {
        Ok(conn) => conn,
        Err(e) => {
            warn!("planner statistics: could not connect for the boot refresh: {e}");
            return;
        }
    };

    let pass = index::db_diesel::ensure_planner_stats(&mut conn, mode);
    if serve_args.boot_analyze_timeout == 0 {
        pass.await;
        return;
    }
    let budget = std::time::Duration::from_secs(serve_args.boot_analyze_timeout);
    if tokio::time::timeout(budget, pass).await.is_err() {
        tracing::error!(
            timeout_secs = serve_args.boot_analyze_timeout,
            "planner statistics: boot refresh exceeded its budget, starting anyway",
        );
    }
}
