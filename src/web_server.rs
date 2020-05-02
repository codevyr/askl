use std::io;
use std::sync::{Arc, Mutex};

use juniper::http::GraphQLRequest;
use juniper::http::graphiql::graphiql_source;

use actix_web::{web, App, HttpResponse, HttpServer};

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
    })
        .bind("127.0.0.1:8080")?
        .run()
        .await
}
