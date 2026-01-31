use std::fmt;

use actix_web::{
    dev::{Payload, ServiceRequest},
    error::{ErrorForbidden, ErrorInternalServerError, ErrorUnauthorized},
    http::header,
    web, FromRequest, HttpMessage, HttpRequest,
};
use argon2::{
    password_hash::{
        rand_core::{OsRng, RngCore},
        PasswordHash, PasswordHasher, PasswordVerifier, SaltString,
    },
    Argon2,
};
use base64::{engine::general_purpose::URL_SAFE_NO_PAD, Engine as _};
use chrono::{DateTime, Utc};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::r2d2::{ConnectionManager, Pool};
use diesel_migrations::{embed_migrations, EmbeddedMigrations, MigrationHarness};
use futures::future::LocalBoxFuture;
use serde::{Deserialize, Serialize};
use tokio::task;
use uuid::Uuid;

const MIGRATIONS: EmbeddedMigrations = embed_migrations!("migrations_auth");

mod schema {
    diesel::table! {
        users (id) {
            id -> Uuid,
            email -> Text,
            created_at -> Timestamptz,
        }
    }

    diesel::table! {
        api_keys (id) {
            id -> Uuid,
            user_id -> Uuid,
            hashed_secret -> Text,
            name -> Nullable<Text>,
            created_at -> Timestamptz,
            last_used_at -> Nullable<Timestamptz>,
            revoked_at -> Nullable<Timestamptz>,
            expires_at -> Nullable<Timestamptz>,
        }
    }

    diesel::joinable!(api_keys -> users (user_id));
    diesel::allow_tables_to_appear_in_same_query!(users, api_keys);
}

use schema::{api_keys, users};

#[derive(Clone)]
pub struct AuthStore {
    pool: Pool<ConnectionManager<PgConnection>>,
}

#[derive(Debug, Clone)]
pub struct AuthIdentity {
    pub user_id: Uuid,
    pub email: String,
    pub key_id: Uuid,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateApiKeyRequest {
    pub email: String,
    pub name: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CreateApiKeyResponse {
    pub token: String,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RevokeApiKeyRequest {
    pub token_id: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct RevokeApiKeyResponse {
    pub revoked: bool,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListApiKeysRequest {
    pub email: String,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ApiKeyInfo {
    pub id: String,
    pub name: Option<String>,
    pub created_at: String,
    pub last_used_at: Option<String>,
    pub revoked_at: Option<String>,
    pub expires_at: Option<String>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct ListApiKeysResponse {
    pub keys: Vec<ApiKeyInfo>,
}

#[derive(Debug)]
pub enum AuthError {
    InvalidToken,
    RevokedToken,
    ExpiredToken,
    Storage(String),
}

impl fmt::Display for AuthError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            AuthError::InvalidToken => write!(f, "Invalid API token"),
            AuthError::RevokedToken => write!(f, "Revoked API token"),
            AuthError::ExpiredToken => write!(f, "Expired API token"),
            AuthError::Storage(message) => write!(f, "Auth storage error: {}", message),
        }
    }
}

impl std::error::Error for AuthError {}

#[derive(Debug)]
struct AuthRow {
    user_id: Uuid,
    email: String,
    hashed_secret: String,
    revoked_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = users)]
struct NewUser {
    id: Uuid,
    email: String,
    created_at: DateTime<Utc>,
}

#[derive(Debug, Insertable)]
#[diesel(table_name = api_keys)]
struct NewApiKey {
    id: Uuid,
    user_id: Uuid,
    hashed_secret: String,
    name: Option<String>,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

#[derive(Debug, Queryable)]
struct ApiKeyRow {
    id: Uuid,
    name: Option<String>,
    created_at: DateTime<Utc>,
    last_used_at: Option<DateTime<Utc>>,
    revoked_at: Option<DateTime<Utc>>,
    expires_at: Option<DateTime<Utc>>,
}

impl AuthStore {
    pub fn connect(database_url: &str) -> anyhow::Result<Self> {
        let manager = ConnectionManager::<PgConnection>::new(database_url);
        let pool = Pool::builder().build(manager)?;

        let mut connection = pool.get()?;
        connection
            .run_pending_migrations(MIGRATIONS)
            .map_err(|err| anyhow::anyhow!(err.to_string()))?;

        Ok(Self { pool })
    }

    pub async fn create_api_key(
        &self,
        email: &str,
        name: Option<&str>,
        expires_at: Option<DateTime<Utc>>,
    ) -> Result<String, AuthError> {
        let email = email.trim().to_string();
        let name = name.map(str::to_string);
        let secret = generate_secret();
        let hashed_secret = hash_secret(&secret)?;
        let key_id = Uuid::new_v4();
        let now = Utc::now();
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| AuthError::Storage(err.to_string()))?;

            conn.transaction::<_, diesel::result::Error, _>(|conn| {
                let existing_user_id = users::table
                    .filter(users::email.eq(&email))
                    .select(users::id)
                    .first::<Uuid>(conn)
                    .optional()?;

                let user_id = if let Some(id) = existing_user_id {
                    id
                } else {
                    let new_user = NewUser {
                        id: Uuid::new_v4(),
                        email: email.clone(),
                        created_at: now,
                    };
                    diesel::insert_into(users::table)
                        .values(&new_user)
                        .execute(conn)?;
                    new_user.id
                };

                let new_key = NewApiKey {
                    id: key_id,
                    user_id,
                    hashed_secret: hashed_secret.clone(),
                    name,
                    created_at: now,
                    last_used_at: None,
                    revoked_at: None,
                    expires_at,
                };
                diesel::insert_into(api_keys::table)
                    .values(new_key)
                    .execute(conn)?;
                Ok(())
            })
            .map_err(|err| AuthError::Storage(err.to_string()))?;

            Ok(())
        })
        .await
        .map_err(|err| AuthError::Storage(err.to_string()))??;

        Ok(format!("askl_{}.{}", key_id, secret))
    }

    pub async fn authenticate_token(&self, token: &str) -> Result<AuthIdentity, AuthError> {
        let (key_id, secret) = parse_token(token)?;
        let pool = self.pool.clone();

        task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| AuthError::Storage(err.to_string()))?;

            let record = api_keys::table
                .inner_join(users::table)
                .select((
                    users::id,
                    users::email,
                    api_keys::hashed_secret,
                    api_keys::revoked_at,
                    api_keys::expires_at,
                ))
                .filter(api_keys::id.eq(key_id))
                .first::<(
                    Uuid,
                    String,
                    String,
                    Option<DateTime<Utc>>,
                    Option<DateTime<Utc>>,
                )>(
                    &mut conn,
                )
                .optional()
                .map_err(|err| AuthError::Storage(err.to_string()))?
                .ok_or(AuthError::InvalidToken)?;

            let auth_row = AuthRow {
                user_id: record.0,
                email: record.1,
                hashed_secret: record.2,
                revoked_at: record.3,
                expires_at: record.4,
            };

            if auth_row.revoked_at.is_some() {
                return Err(AuthError::RevokedToken);
            }

            verify_secret(&auth_row.hashed_secret, &secret)?;

            if let Some(expires_at) = auth_row.expires_at {
                if expires_at <= Utc::now() {
                    return Err(AuthError::ExpiredToken);
                }
            }

            diesel::update(api_keys::table.filter(api_keys::id.eq(key_id)))
                .set(api_keys::last_used_at.eq(Utc::now()))
                .execute(&mut conn)
                .map_err(|err| AuthError::Storage(err.to_string()))?;

            Ok(AuthIdentity {
                user_id: auth_row.user_id,
                email: auth_row.email,
                key_id,
            })
        })
        .await
        .map_err(|err| AuthError::Storage(err.to_string()))?
    }

    pub async fn revoke_api_key(&self, token_id: Uuid) -> Result<bool, AuthError> {
        let pool = self.pool.clone();
        let revoked = task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| AuthError::Storage(err.to_string()))?;

            let updated = diesel::update(
                api_keys::table
                    .filter(api_keys::id.eq(token_id))
                    .filter(api_keys::revoked_at.is_null()),
            )
            .set(api_keys::revoked_at.eq(Utc::now()))
            .execute(&mut conn)
            .map_err(|err| AuthError::Storage(err.to_string()))?;

            Ok(updated > 0)
        })
        .await
        .map_err(|err| AuthError::Storage(err.to_string()))??;

        Ok(revoked)
    }

    pub async fn list_api_keys(&self, email: &str) -> Result<Vec<ApiKeyInfo>, AuthError> {
        let email = email.trim().to_string();
        let pool = self.pool.clone();
        let rows = task::spawn_blocking(move || {
            let mut conn = pool
                .get()
                .map_err(|err| AuthError::Storage(err.to_string()))?;

            let rows = api_keys::table
                .inner_join(users::table)
                .select((
                    api_keys::id,
                    api_keys::name,
                    api_keys::created_at,
                    api_keys::last_used_at,
                    api_keys::revoked_at,
                    api_keys::expires_at,
                ))
                .filter(users::email.eq(email))
                .order(api_keys::created_at.desc())
                .load::<ApiKeyRow>(&mut conn)
                .map_err(|err| AuthError::Storage(err.to_string()))?;

            Ok(rows)
        })
        .await
        .map_err(|err| AuthError::Storage(err.to_string()))??;

        Ok(rows
            .into_iter()
            .map(|row| ApiKeyInfo {
                id: row.id.to_string(),
                name: row.name,
                created_at: row.created_at.to_rfc3339(),
                last_used_at: row.last_used_at.map(|value| value.to_rfc3339()),
                revoked_at: row.revoked_at.map(|value| value.to_rfc3339()),
                expires_at: row.expires_at.map(|value| value.to_rfc3339()),
            })
            .collect())
    }
}

impl FromRequest for AuthIdentity {
    type Error = actix_web::Error;
    type Future = LocalBoxFuture<'static, Result<Self, Self::Error>>;

    fn from_request(req: &HttpRequest, _: &mut Payload) -> Self::Future {
        let store = req.app_data::<web::Data<AuthStore>>().cloned();
        let token = extract_token(req);
        let secure_request = is_secure_request(req);
        let allow_insecure = insecure_tokens_allowed();

        Box::pin(async move {
            let store = store.ok_or_else(|| ErrorInternalServerError("Auth store missing"))?;
            let token = token.ok_or_else(|| ErrorUnauthorized("Unauthorized"))?;
            if !secure_request && !allow_insecure {
                return Err(ErrorForbidden("API tokens require HTTPS"));
            }
            match store.authenticate_token(&token).await {
                Ok(identity) => Ok(identity),
                Err(AuthError::InvalidToken | AuthError::RevokedToken | AuthError::ExpiredToken) => {
                    Err(ErrorUnauthorized("Unauthorized"))
                }
                Err(AuthError::Storage(message)) => Err(ErrorInternalServerError(message)),
            }
        })
    }
}

pub fn is_loopback(req: &HttpRequest) -> bool {
    req.peer_addr()
        .map(|addr| addr.ip().is_loopback())
        .unwrap_or(false)
}

pub fn bootstrap_allowed() -> bool {
    match std::env::var("ASKL_BOOTSTRAP_MODE") {
        Ok(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

pub fn insecure_tokens_allowed() -> bool {
    match std::env::var("ASKL_ALLOW_INSECURE_TOKENS") {
        Ok(value) => matches!(
            value.to_ascii_lowercase().as_str(),
            "true" | "1" | "yes" | "on"
        ),
        Err(_) => false,
    }
}

fn is_secure_request(req: &HttpRequest) -> bool {
    if req.connection_info().scheme().eq_ignore_ascii_case("https") {
        return true;
    }

    if let Some(value) = req.headers().get("x-forwarded-proto") {
        if let Ok(proto) = value.to_str() {
            let proto = proto.split(',').next().unwrap_or("").trim();
            if proto.eq_ignore_ascii_case("https") {
                return true;
            }
        }
    }

    if let Some(value) = req.headers().get("x-forwarded-ssl") {
        if let Ok(flag) = value.to_str() {
            if flag.eq_ignore_ascii_case("on") {
                return true;
            }
        }
    }

    if let Some(value) = req.headers().get("forwarded") {
        if let Ok(forwarded) = value.to_str() {
            if forwarded.to_ascii_lowercase().contains("proto=https") {
                return true;
            }
        }
    }

    false
}

fn extract_token(req: &HttpRequest) -> Option<String> {
    if let Some(token) = req.extensions().get::<RedactedAuthToken>() {
        return Some(token.0.clone());
    }

    extract_token_from_headers(req.headers())
}

fn extract_token_from_headers(headers: &header::HeaderMap) -> Option<String> {
    if let Some(value) = headers.get(header::AUTHORIZATION) {
        if let Ok(header_value) = value.to_str() {
            if let Some(bearer) = header_value.strip_prefix("Bearer ") {
                let trimmed = bearer.trim();
                if !trimmed.is_empty() {
                    return Some(trimmed.to_string());
                }
            }
        }
    }

    let api_key_header = header::HeaderName::from_static("x-api-key");
    if let Some(value) = headers.get(api_key_header) {
        if let Ok(header_value) = value.to_str() {
            let trimmed = header_value.trim();
            if !trimmed.is_empty() {
                return Some(trimmed.to_string());
            }
        }
    }

    None
}

#[derive(Clone)]
struct RedactedAuthToken(String);

pub fn redact_auth_headers(req: &mut ServiceRequest) {
    if let Some(token) = extract_token_from_headers(req.headers()) {
        req.extensions_mut().insert(RedactedAuthToken(token));
    }

    req.headers_mut().remove(header::AUTHORIZATION);
    req.headers_mut()
        .remove(header::HeaderName::from_static("x-api-key"));
}

fn parse_token(token: &str) -> Result<(Uuid, String), AuthError> {
    let token = token.trim();
    let token = token
        .strip_prefix("askl_")
        .ok_or(AuthError::InvalidToken)?;
    let mut parts = token.splitn(2, '.');
    let id_part = parts.next().ok_or(AuthError::InvalidToken)?;
    let secret = parts.next().ok_or(AuthError::InvalidToken)?;
    if secret.is_empty() {
        return Err(AuthError::InvalidToken);
    }
    let key_id = Uuid::parse_str(id_part).map_err(|_| AuthError::InvalidToken)?;
    Ok((key_id, secret.to_string()))
}

fn generate_secret() -> String {
    let mut buffer = [0u8; 32];
    let mut rng = OsRng;
    rng.fill_bytes(&mut buffer);
    URL_SAFE_NO_PAD.encode(buffer)
}

fn hash_secret(secret: &str) -> Result<String, AuthError> {
    let salt = SaltString::generate(&mut OsRng);
    let argon2 = Argon2::default();
    argon2
        .hash_password(secret.as_bytes(), &salt)
        .map(|hash| hash.to_string())
        .map_err(|err| AuthError::Storage(err.to_string()))
}

fn verify_secret(hash: &str, secret: &str) -> Result<(), AuthError> {
    let parsed = PasswordHash::new(hash).map_err(|_| AuthError::InvalidToken)?;
    Argon2::default()
        .verify_password(secret.as_bytes(), &parsed)
        .map_err(|_| AuthError::InvalidToken)
}
