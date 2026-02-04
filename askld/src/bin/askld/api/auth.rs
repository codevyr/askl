use actix_web::{post, web, HttpRequest, HttpResponse, Responder};
use askld::auth::{
    AuthStore, CreateApiKeyRequest, CreateApiKeyResponse, ListApiKeysRequest,
    ListApiKeysResponse, RevokeApiKeyRequest, RevokeApiKeyResponse,
};
use askld::auth;
use log::error;

#[post("/auth/local/create-api-key")]
pub async fn create_api_key(
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
        .create_api_key(payload.email.trim(), payload.name.as_deref(), expires_at)
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
pub async fn revoke_api_key(
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
pub async fn list_api_keys(
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
