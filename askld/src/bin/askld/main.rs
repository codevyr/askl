use std::collections::{HashMap, HashSet};

use actix_web::{
    delete, dev::Service, get,
    http::header,
    post, web, App, HttpRequest, HttpResponse, HttpServer, Responder,
};
use anyhow::{anyhow, Result};
use askld::auth::{
    ApiKeyInfo, AuthIdentity, AuthStore, CreateApiKeyRequest, CreateApiKeyResponse,
    ListApiKeysRequest, ListApiKeysResponse, RevokeApiKeyRequest, RevokeApiKeyResponse,
};
use askld::execution_context::ExecutionContext;
use askld::index_store::{IndexStore, StoreError, UploadError};
use askld::proto::askl::index::IndexUpload;
use askld::parser::Rule;
use askld::{auth, cfg::ControlFlowGraph, parser::parse};
use clap::{Args as ClapArgs, Parser, Subcommand};
use diesel::pg::PgConnection;
use diesel::r2d2::{ConnectionManager, Pool};
use index::db::{self};
use index::symbols::SymbolId;
use index::symbols::{DeclarationId, FileId, Occurrence, SymbolType};
use log::{debug, error, info};
use prost::Message;
use serde::{Deserialize, Serialize, Serializer};
use tokio::time::{timeout, Duration};
use tracing_chrome::ChromeLayerBuilder;
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
struct Args {
    #[clap(subcommand)]
    command: Command,
}

#[derive(Subcommand, Debug)]
enum Command {
    Serve(ServeArgs),
    Auth(AuthArgs),
}

#[derive(ClapArgs, Debug)]
struct ServeArgs {
    /// Postgres connection string for the auth and index DB
    #[clap(long, env = "ASKL_DATABASE_URL")]
    database_url: String,

    /// Port to listen on
    #[clap(short, long, default_value = "80")]
    port: u16,

    /// Host to bind to
    #[clap(short = 'H', long, default_value = "127.0.0.1")]
    host: String,

    /// Enable tracing. Provide a file path to write the trace to.
    #[clap(short, long, action)]
    trace: Option<String>,
}

#[derive(ClapArgs, Debug)]
struct AuthArgs {
    /// Port to call on localhost
    #[clap(short, long, default_value = "80")]
    port: u16,

    #[clap(subcommand)]
    command: AuthCommand,
}

#[derive(Subcommand, Debug)]
enum AuthCommand {
    CreateApiKey {
        #[clap(long)]
        email: String,
        #[clap(long)]
        name: Option<String>,
        #[clap(long, action)]
        json: bool,
        /// RFC3339 timestamp, e.g. 2026-01-01T00:00:00Z
        #[clap(long)]
        expires_at: Option<String>,
    },
    RevokeApiKey {
        #[clap(long)]
        token_id: String,
        #[clap(long, action)]
        json: bool,
    },
    ListApiKeys {
        #[clap(long)]
        email: String,
        #[clap(long, action)]
        json: bool,
    },
}

struct AsklData {
    cfg: ControlFlowGraph,
}

const QUERY_TIMEOUT: Duration = Duration::from_secs(1);

fn symbolid_as_string<S>(x: &SymbolId, s: S) -> Result<S::Ok, S::Error>
where
    S: Serializer,
{
    s.serialize_str(&format!("{}", x))
}

fn print_key(key: &ApiKeyInfo) {
    println!("ID: {}", key.id);
    if let Some(name) = &key.name {
        println!("Name: {}", name);
    }
    println!("Created: {}", key.created_at);
    if let Some(last_used) = &key.last_used_at {
        println!("Last used: {}", last_used);
    }
    if let Some(revoked_at) = &key.revoked_at {
        println!("Revoked: {}", revoked_at);
    }
    if let Some(expires_at) = &key.expires_at {
        println!("Expires: {}", expires_at);
    }
    println!();
}

#[derive(Debug, Serialize, Deserialize)]
struct Node {
    #[serde(serialize_with = "symbolid_as_string")]
    id: SymbolId,
    label: String,
    declarations: Vec<db::Declaration>,
}

impl Node {
    fn new(id: SymbolId, label: String, declarations: Vec<db::Declaration>) -> Self {
        Self {
            id,
            label,
            declarations,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Edge {
    id: String,
    #[serde(serialize_with = "symbolid_as_string")]
    from: SymbolId,
    #[serde(serialize_with = "symbolid_as_string")]
    to: SymbolId,
    from_file: Option<FileId>,
    from_offset_start: Option<i32>,
    from_offset_end: Option<i32>,
}

impl Edge {
    fn new(from: SymbolId, to: SymbolId, occurence: Option<Occurrence>) -> Self {
        let (filename, start_offset, end_offset) = if let Some(occ) = occurence {
            (Some(occ.file), Some(occ.start_offset), Some(occ.end_offset))
        } else {
            (None, None, None)
        };
        Self {
            id: format!("{}-{}", from, to),
            from: from,
            to: to,
            from_file: filename,
            from_offset_start: start_offset,
            from_offset_end: end_offset,
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct Graph {
    nodes: Vec<Node>,
    edges: Vec<Edge>,
    files: Vec<(FileId, String)>,
    warnings: Vec<ErrorResponse>,
}

impl Graph {
    fn new() -> Self {
        Self {
            nodes: vec![],
            edges: vec![],
            files: vec![],
            warnings: vec![],
        }
    }

    fn add_node(&mut self, node: Node) {
        self.nodes.push(node);
    }

    fn add_edge(&mut self, edge: Edge) {
        self.edges.push(edge);
    }

    fn add_warnings(&mut self, warnings: Vec<pest::error::Error<Rule>>) {
        for warning in warnings {
            let error_response = ErrorResponse {
                message: warning.to_string(),
                location: warning.location.clone().into(),
                line_col: warning.line_col.clone().into(),
                path: warning.path().map(|p| p.to_string()),
                line: warning.line().to_string(),
            };
            self.warnings.push(error_response);
        }
    }
}

/// Where an `Error` has occurred.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum InputLocation {
    /// `Error` was created by `Error::new_from_pos`
    Pos(usize),
    /// `Error` was created by `Error::new_from_span`
    Span((usize, usize)),
}

impl From<pest::error::InputLocation> for InputLocation {
    fn from(loc: pest::error::InputLocation) -> Self {
        match loc {
            pest::error::InputLocation::Pos(pos) => InputLocation::Pos(pos),
            pest::error::InputLocation::Span(span) => InputLocation::Span(span),
        }
    }
}

/// Line/column where an `Error` has occurred.
#[derive(Clone, Debug, Eq, Hash, PartialEq, Serialize, Deserialize)]
pub enum LineColLocation {
    /// Line/column pair if `Error` was created by `Error::new_from_pos`
    Pos((usize, usize)),
    /// Line/column pairs if `Error` was created by `Error::new_from_span`
    Span((usize, usize), (usize, usize)),
}

impl From<pest::error::LineColLocation> for LineColLocation {
    fn from(loc: pest::error::LineColLocation) -> Self {
        match loc {
            pest::error::LineColLocation::Pos(pos) => LineColLocation::Pos(pos),
            pest::error::LineColLocation::Span(start, end) => LineColLocation::Span(start, end),
        }
    }
}

#[derive(Debug, Serialize, Deserialize)]
struct ErrorResponse {
    message: String,
    location: InputLocation,
    line_col: LineColLocation,
    path: Option<String>,
    line: String,
}

#[derive(Debug, Serialize)]
struct IndexUploadResponse {
    project_id: i32,
}

#[derive(Debug, Serialize)]
struct IndexDeleteResponse {
    project_id: i32,
    deleted: bool,
}

#[get("/version")]
async fn version(_identity: AuthIdentity) -> impl Responder {
    HttpResponse::Ok().body(env!("CARGO_PKG_VERSION"))
}

#[post("/auth/local/create-api-key")]
async fn create_api_key(
    req: HttpRequest,
    auth_store: web::Data<AuthStore>,
    payload: web::Json<CreateApiKeyRequest>,
) -> impl Responder {
    if !auth::is_loopback(&req) {
        return HttpResponse::Forbidden().body("Loopback connections only");
    }

    if !auth::bootstrap_allowed() {
        return HttpResponse::Forbidden().body("Bootstrap mode disabled");
    }

    if payload.email.trim().is_empty() {
        return HttpResponse::BadRequest().body("Email is required");
    }

    let expires_at = match payload.expires_at.as_deref() {
        None => None,
        Some(raw) => match chrono::DateTime::parse_from_rfc3339(raw) {
            Ok(value) => Some(value.with_timezone(&chrono::Utc)),
            Err(_) => {
                return HttpResponse::BadRequest()
                    .body("Invalid expires_at; use RFC3339 like 2026-01-01T00:00:00Z");
            }
        },
    };

    if let Some(expires_at) = expires_at.as_ref() {
        if *expires_at <= chrono::Utc::now() {
            return HttpResponse::BadRequest().body("expires_at must be in the future");
        }
    }

    let expires_at_response = expires_at.as_ref().map(|value| value.to_rfc3339());

    match auth_store
        .create_api_key(
            payload.email.trim(),
            payload.name.as_deref(),
            expires_at,
        )
        .await
    {
        Ok(token) => HttpResponse::Ok().json(CreateApiKeyResponse {
            token,
            expires_at: expires_at_response,
        }),
        Err(err) => {
            error!("Failed to create API key: {}", err);
            HttpResponse::InternalServerError().body("Failed to create API key")
        }
    }
}

#[post("/auth/local/revoke-api-key")]
async fn revoke_api_key(
    req: HttpRequest,
    auth_store: web::Data<AuthStore>,
    payload: web::Json<RevokeApiKeyRequest>,
) -> impl Responder {
    if !auth::is_loopback(&req) {
        return HttpResponse::Forbidden().body("Loopback connections only");
    }

    if !auth::bootstrap_allowed() {
        return HttpResponse::Forbidden().body("Bootstrap mode disabled");
    }

    let token_id = match uuid::Uuid::parse_str(payload.token_id.trim()) {
        Ok(token_id) => token_id,
        Err(_) => {
            return HttpResponse::BadRequest().body("Invalid token_id; expected UUID");
        }
    };

    match auth_store.revoke_api_key(token_id).await {
        Ok(revoked) => {
            if revoked {
                HttpResponse::Ok().json(RevokeApiKeyResponse { revoked })
            } else {
                HttpResponse::NotFound().body("API key not found or already revoked")
            }
        }
        Err(err) => {
            error!("Failed to revoke API key: {}", err);
            HttpResponse::InternalServerError().body("Failed to revoke API key")
        }
    }
}

#[post("/auth/local/list-api-keys")]
async fn list_api_keys(
    req: HttpRequest,
    auth_store: web::Data<AuthStore>,
    payload: web::Json<ListApiKeysRequest>,
) -> impl Responder {
    if !auth::is_loopback(&req) {
        return HttpResponse::Forbidden().body("Loopback connections only");
    }

    if !auth::bootstrap_allowed() {
        return HttpResponse::Forbidden().body("Bootstrap mode disabled");
    }

    if payload.email.trim().is_empty() {
        return HttpResponse::BadRequest().body("Email is required");
    }

    match auth_store.list_api_keys(payload.email.trim()).await {
        Ok(keys) => HttpResponse::Ok().json(ListApiKeysResponse { keys }),
        Err(err) => {
            error!("Failed to list API keys: {}", err);
            HttpResponse::InternalServerError().body("Failed to list API keys")
        }
    }
}

#[post("/v1/index/upload")]
async fn upload_index(
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

    let upload = match IndexUpload::decode(body.as_ref()) {
        Ok(upload) => upload,
        Err(err) => {
            return HttpResponse::BadRequest()
                .body(format!("Failed to decode protobuf payload: {}", err));
        }
    };

    match store.upload_index(upload).await {
        Ok(project_id) => HttpResponse::Ok().json(IndexUploadResponse { project_id }),
        Err(UploadError::Conflict) => HttpResponse::Conflict().body("Project already exists"),
        Err(UploadError::Invalid(message)) => HttpResponse::BadRequest().body(message),
        Err(UploadError::Storage(message)) => {
            error!("Index upload failed: {}", message);
            HttpResponse::InternalServerError().body("Failed to upload index")
        }
    }
}

#[get("/v1/index/projects")]
async fn list_index_projects(
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
async fn get_index_project(
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
async fn delete_index_project(
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

#[post("/query")]
async fn query(data: web::Data<AsklData>, req_body: String) -> impl Responder {
    let _query = tracing::info_span!("query").entered();

    println!("Received query: {}", req_body);
    let ast = match parse(&req_body) {
        Ok(ast) => ast,
        Err(err) => {
            println!("Parse error: {}", err);
            let json_err = serde_json::to_string(&ErrorResponse {
                message: err.to_string(),
                location: err.location.clone().into(),
                line_col: err.line_col.clone().into(),
                path: err.path().map(|p| p.to_string()),
                line: err.line().to_string(),
            })
            .unwrap();
            return HttpResponse::BadRequest().body(json_err);
        }
    };
    debug!("Global scope: {:#?}", ast);

    let mut ctx = ExecutionContext::new();

    let res = {
        let _query_execute = tracing::info_span!("query_execute").entered();
        let execute_future = ast.execute(&mut ctx, &data.cfg);
        match timeout(QUERY_TIMEOUT, execute_future).await {
            Ok(Err(err)) => {
                let json_err = serde_json::to_string(&ErrorResponse {
                    message: err.to_string(),
                    location: err.location.clone().into(),
                    line_col: err.line_col.clone().into(),
                    path: err.path().map(|p| p.to_string()),
                    line: err.line().to_string(),
                });
                return HttpResponse::BadRequest().body(json_err.unwrap());
            }
            Ok(Ok(res)) => res,
            Err(_) => {
                return HttpResponse::RequestTimeout().body("Query timed out");
            }
        }
    };

    info!("Symbols: {:#?}", res.nodes.as_vec().len());
    info!("Edges: {:#?}", res.edges.0.len());

    let mut result_graph = Graph::new();

    for (from, to, loc) in res.edges.0 {
        result_graph.add_edge(Edge::new(from.symbol_id, to.symbol_id, loc));
    }

    let mut all_symbols = HashSet::new();
    for declaration in res.nodes.0.iter() {
        all_symbols.insert(declaration.symbol.clone());
    }

    let mut result_files = HashMap::new();
    for symbol in all_symbols {
        for declaration in res.nodes.0.iter() {
            if !result_files.contains_key(&FileId::new(declaration.file.id)) {
                result_files.insert(
                    FileId::new(declaration.file.id),
                    declaration.file.filesystem_path.clone(),
                );
            }
        }

        let declarations: Vec<db::Declaration> = res
            .nodes
            .0
            .iter()
            .filter(|d| d.declaration.symbol == symbol.id)
            .map(|d| db::Declaration {
                id: DeclarationId::new(d.declaration.id),
                symbol: SymbolId(d.declaration.symbol),
                file_id: FileId::new(d.file.id),
                symbol_type: SymbolType::from(d.declaration.symbol_type),
                start_offset: d.declaration.start_offset as i64,
                end_offset: d.declaration.end_offset as i64,
            })
            .collect();

        println!("Declarations for symbol {}: {:?}", symbol.id, declarations);
        result_graph.add_node(Node::new(
            SymbolId(symbol.id),
            symbol.name.clone(),
            declarations,
        ));
    }

    result_graph.files = result_files.into_iter().collect();
    result_graph.add_warnings(res.warnings);

    let json_graph = serde_json::to_string_pretty(&result_graph).unwrap();
    HttpResponse::Ok().body(json_graph)
}

#[get["/source/{file_id}"]]
async fn file(data: web::Data<AsklData>, file_id: web::Path<FileId>) -> impl Responder {
    let _source = tracing::info_span!("source").entered();

    let file_id = *file_id;

    println!("Received request for file: {}", file_id);
    if let Ok(source) = data.cfg.index.get_file_contents(file_id).await {
        HttpResponse::Ok().body(source)
    } else {
        HttpResponse::NotFound().body("File not found")
    }
}

async fn run_auth_command(port: u16, command: AuthCommand) -> Result<()> {
    match command {
        AuthCommand::CreateApiKey {
            email,
            name,
            json,
            expires_at,
        } => {
            let client = reqwest::Client::new();
            let url = format!("http://127.0.0.1:{}/auth/local/create-api-key", port);
            let response = client
                .post(url)
                .json(&CreateApiKeyRequest {
                    email,
                    name,
                    expires_at,
                })
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let token_response: CreateApiKeyResponse = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&token_response)?;
                println!("{}", output);
            } else {
                println!("API key: {}", token_response.token);
                if let Some(expires_at) = token_response.expires_at {
                    println!("Expires: {}", expires_at);
                }
                eprintln!("Store this token securely; it will not be shown again.");
            }
        }
        AuthCommand::RevokeApiKey { token_id, json } => {
            let client = reqwest::Client::new();
            let url = format!("http://127.0.0.1:{}/auth/local/revoke-api-key", port);
            let response = client
                .post(url)
                .json(&RevokeApiKeyRequest { token_id })
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let result: RevokeApiKeyResponse = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&result)?;
                println!("{}", output);
            } else if result.revoked {
                println!("API key revoked.");
            } else {
                println!("API key not revoked.");
            }
        }
        AuthCommand::ListApiKeys { email, json } => {
            let client = reqwest::Client::new();
            let url = format!("http://127.0.0.1:{}/auth/local/list-api-keys", port);
            let response = client
                .post(url)
                .json(&ListApiKeysRequest { email })
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let result: ListApiKeysResponse = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&result)?;
                println!("{}", output);
            } else {
                if result.keys.is_empty() {
                    println!("No API keys found.");
                } else {
                    for key in result.keys {
                        print_key(&key);
                    }
                }
            }
        }
    }

    Ok(())
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    let serve_args = match args.command {
        Command::Auth(auth_args) => {
            if let Err(err) = run_auth_command(auth_args.port, auth_args.command).await {
                eprintln!("Failed to create API key: {}", err);
                std::process::exit(1);
            }
            return Ok(());
        }
        Command::Serve(serve_args) => serve_args,
    };

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

    let index_query =
        index::db_diesel::Index::from_pool(pool.clone()).expect("Failed to initialize index");
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
            .service(version)
            .service(create_api_key)
            .service(revoke_api_key)
            .service(list_api_keys)
            .service(upload_index)
            .service(list_index_projects)
            .service(get_index_project)
            .service(delete_index_project)
            .service(query)
            .service(file)
    })
    .bind((serve_args.host, serve_args.port))?
    .run()
    .await
}
