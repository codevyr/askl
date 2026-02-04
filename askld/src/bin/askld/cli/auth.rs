use crate::args::AuthCommand;
use anyhow::{anyhow, Result};
use askld::auth::{
    ApiKeyInfo, CreateApiKeyRequest, CreateApiKeyResponse, ListApiKeysRequest,
    ListApiKeysResponse, RevokeApiKeyRequest, RevokeApiKeyResponse,
};

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

pub async fn run_auth_command(port: u16, command: AuthCommand) -> Result<()> {
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
            } else if result.keys.is_empty() {
                println!("No API keys found.");
            } else {
                for key in result.keys {
                    print_key(&key);
                }
            }
        }
    }

    Ok(())
}
