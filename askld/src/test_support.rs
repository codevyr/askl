use anyhow::Result;
use diesel::pg::PgConnection;
use diesel::Connection;
use tokio::time::{sleep, Duration};

#[cfg(test)]
use testcontainers::{core::WaitFor, GenericImage};

#[cfg(test)]
pub fn postgres_test_image() -> GenericImage {
    GenericImage::new("postgres", "15-alpine")
        .with_env_var("POSTGRES_PASSWORD", "postgres")
        .with_env_var("POSTGRES_USER", "postgres")
        .with_env_var("POSTGRES_DB", "askl")
        .with_wait_for(WaitFor::message_on_stdout(
            "database system is ready to accept connections",
        ))
}

pub fn postgres_url(port: u16) -> String {
    format!("postgres://postgres:postgres@127.0.0.1:{}/askl", port)
}

pub async fn wait_for_postgres(url: &str) -> Result<()> {
    let mut delay = Duration::from_millis(50);
    for attempt in 1..=10 {
        match PgConnection::establish(url) {
            Ok(_) => return Ok(()),
            Err(err) => {
                if attempt == 10 {
                    return Err(anyhow::anyhow!(
                        "Postgres not ready after {} attempts: {}",
                        attempt,
                        err
                    ));
                }
            }
        }
        sleep(delay).await;
        delay = std::cmp::min(delay * 2, Duration::from_secs(1));
    }
    Ok(())
}
