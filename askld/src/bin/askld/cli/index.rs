use crate::api::types::IndexUploadResponse;
use crate::args::IndexCommand;
use anyhow::{anyhow, Result};
use askld::proto::askl::index::IndexUpload;
use prost::Message;
use serde::Deserialize;
use tokio::time::Duration;

#[derive(Debug, Deserialize, serde::Serialize)]
struct ProjectInfo {
    id: i32,
    project_name: String,
}

fn normalize_base_url(url: &str) -> String {
    let mut base_url = url.trim().to_string();
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        base_url = format!("http://{}", base_url);
    }
    base_url.trim_end_matches('/').to_string()
}

fn build_client(timeout: u64) -> Result<reqwest::Client> {
    let mut client_builder = reqwest::Client::builder();
    if timeout > 0 {
        client_builder = client_builder.timeout(Duration::from_secs(timeout));
    }
    Ok(client_builder.build()?)
}

pub async fn run_index_command(command: IndexCommand) -> Result<()> {
    match command {
        IndexCommand::Upload {
            file_path,
            url,
            token,
            project,
            timeout,
            json,
        } => {
            use reqwest::header as reqwest_header;

            let token = token.or_else(|| std::env::var("ASKL_TOKEN").ok());
            let token = match token {
                Some(token) if !token.trim().is_empty() => token,
                _ => return Err(anyhow!("Missing token; pass --token or set ASKL_TOKEN")),
            };

            let payload = std::fs::read(&file_path)
                .map_err(|err| anyhow!("Failed to read {}: {}", file_path, err))?;
            if payload.is_empty() {
                return Err(anyhow!("Payload file is empty: {}", file_path));
            }

            let payload = if let Some(project_name) = project {
                let mut upload = IndexUpload::decode(payload.as_slice())
                    .map_err(|err| anyhow!("Failed to decode protobuf payload: {}", err))?;
                upload.project_name = project_name;
                let mut buffer = Vec::with_capacity(upload.encoded_len());
                upload.encode(&mut buffer)?;
                buffer
            } else {
                payload
            };

            let base_url = normalize_base_url(&url);
            let endpoint = format!("{}/v1/index/upload", base_url);

            let client = build_client(timeout)?;
            let response = client
                .post(endpoint)
                .header(reqwest_header::CONTENT_TYPE, "application/x-protobuf")
                .header(reqwest_header::AUTHORIZATION, format!("Bearer {}", token))
                .body(payload)
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                if status == reqwest::StatusCode::CONFLICT {
                    let message = if body.trim().is_empty() {
                        "Project already exists".to_string()
                    } else {
                        body
                    };
                    return Err(anyhow!("Request failed ({}): {}", status, message));
                }
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let result: IndexUploadResponse = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&result)?;
                println!("{}", output);
            } else {
                println!("Uploaded index; project id: {}", result.project_id);
            }
        }
        IndexCommand::ListProjects {
            url,
            token,
            timeout,
            json,
        } => {
            use reqwest::header as reqwest_header;

            let token = token.or_else(|| std::env::var("ASKL_TOKEN").ok());
            let token = match token {
                Some(token) if !token.trim().is_empty() => token,
                _ => return Err(anyhow!("Missing token; pass --token or set ASKL_TOKEN")),
            };

            let base_url = normalize_base_url(&url);
            let endpoint = format!("{}/v1/index/projects", base_url);

            let client = build_client(timeout)?;
            let response = client
                .get(endpoint)
                .header(reqwest_header::AUTHORIZATION, format!("Bearer {}", token))
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let projects: Vec<ProjectInfo> = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&projects)?;
                println!("{}", output);
            } else if projects.is_empty() {
                println!("No projects found.");
            } else {
                for project in projects {
                    println!("{} {}", project.id, project.project_name);
                }
            }
        }
    }

    Ok(())
}
