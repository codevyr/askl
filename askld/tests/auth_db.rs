use askld::auth::{AuthError, AuthStore};
use diesel::pg::PgConnection;
use diesel::prelude::*;
use diesel::sql_types::BigInt;
use testcontainers::{clients, core::WaitFor, GenericImage};
use uuid::Uuid;

#[derive(QueryableByName)]
struct CountRow {
    #[diesel(sql_type = BigInt)]
    count: i64,
}

#[tokio::test]
async fn auth_store_round_trip_with_postgres() {
    let docker = clients::Cli::default();
    let image = GenericImage::new("postgres", "15-alpine")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "askl")
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ));
    let node = docker.run(image);
    let port = node.get_host_port_ipv4(5432);
    let url = format!("postgres://postgres:postgres@127.0.0.1:{}/askl", port);

    let store = AuthStore::connect(&url).expect("connect auth store");
    let token = store
        .create_api_key("user@example.com", Some("test key"), None)
        .await
        .expect("create api key");
    let key_id = token
        .strip_prefix("askl_")
        .and_then(|rest| rest.split('.').next())
        .and_then(|id| Uuid::parse_str(id).ok())
        .expect("parse token id");

    let keys = store
        .list_api_keys("user@example.com")
        .await
        .expect("list keys");
    assert_eq!(keys.len(), 1);
    assert!(keys[0].revoked_at.is_none());
    assert!(keys[0].last_used_at.is_none());

    let identity = store
        .authenticate_token(&token)
        .await
        .expect("authenticate token");
    assert_eq!(identity.email, "user@example.com");

    let keys = store
        .list_api_keys("user@example.com")
        .await
        .expect("list keys after use");
    assert_eq!(keys.len(), 1);
    assert!(keys[0].last_used_at.is_some());

    let revoked = store
        .revoke_api_key(key_id)
        .await
        .expect("revoke key");
    assert!(revoked);

    let keys = store
        .list_api_keys("user@example.com")
        .await
        .expect("list keys after revoke");
    assert_eq!(keys.len(), 1);
    assert!(keys[0].revoked_at.is_some());

    let err = store
        .authenticate_token(&token)
        .await
        .expect_err("token should be revoked");
    assert!(matches!(err, AuthError::RevokedToken));

    let mut conn = PgConnection::establish(&url).expect("connect pg");
    let user_count: CountRow = diesel::sql_query("SELECT COUNT(*) as count FROM users")
        .get_result(&mut conn)
        .expect("count users");
    let key_count: CountRow = diesel::sql_query("SELECT COUNT(*) as count FROM api_keys")
        .get_result(&mut conn)
        .expect("count api_keys");

    assert_eq!(user_count.count, 1);
    assert_eq!(key_count.count, 1);
}
