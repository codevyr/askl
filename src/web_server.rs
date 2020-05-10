use std::io;
use std::sync::{Arc, Mutex};
use std::path::{Path, PathBuf};

use juniper::http::GraphQLRequest;
use juniper::http::graphiql::graphiql_source;

use actix_web::{web, App, HttpRequest, HttpResponse, HttpServer};
use actix_files as fs;
use actix_web::http::header::{ContentDisposition, DispositionType};

use crate::schema;
use crate::asker;

async fn graphiql() -> HttpResponse {
    let html = graphiql_source("http://127.0.0.1:8080/graphql");
    HttpResponse::Ok()
        .content_type("text/html; charset=utf-8")
        .body(html)
}

#[derive(Clone)]
struct AppData {
    schema: Arc<schema::Schema>,
    asker: Arc<Mutex<asker::Asker>>,
}

async fn graphql(
    st: web::Data<AppData>,
    data: web::Json<GraphQLRequest>,
) -> Result<HttpResponse, actix_web::Error> {
    println!("Hello");
    let user = web::block(move || {
        let ctx = schema::Context{
            asker: st.asker.clone(),
        };
        let res = data.execute(&st.schema, &ctx);
        Ok::<_, serde_json::error::Error>(serde_json::to_string(&res)?)
    })
    .await?;
    Ok(HttpResponse::Ok()
        .content_type("application/json")
        .body(user))
}

async fn index(req: HttpRequest) -> Result<fs::NamedFile, actix_web::Error> {
    let path = {
        let path: PathBuf = req.match_info().query("filename").parse()?;
        if path == Path::new("") {
            Path::new("static/index.html").to_path_buf()
        } else {
            Path::new("static/").join(path)
        }
    };
    println!("Attempt to open {:?}", path);
    let file = fs::NamedFile::open(path)?;
    Ok(file)
}

#[actix_rt::main]
pub async fn server_main(asker: Arc<Mutex<asker::Asker>>) -> io::Result<()> {
    let schema = std::sync::Arc::new(schema::create_schema());
    HttpServer::new(move || {
        App::new()
            .data(AppData{
                schema: schema.clone(),
                asker: asker.clone(),
            })
            .service(web::resource("/graphql").route(web::post().to(graphql)))
            .service(web::resource("/graphiql").route(web::get().to(graphiql)))
            .route("/{filename:.*}", web::get().to(index))
    })
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
