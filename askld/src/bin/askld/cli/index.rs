use crate::args::IndexCommand;
use anyhow::{anyhow, Result};
use askld::proto::askl::index::Project;
use bytes::Bytes;
use futures::TryStreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use prost::Message;
use serde::{Deserialize, Serialize};
use tokio::time::Duration;
use tokio_util::codec::{BytesCodec, FramedRead};

#[derive(Debug, Deserialize, Serialize)]
struct ProjectInfo {
    id: i32,
    project_name: String,
    root_path: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProjectModule {
    id: i32,
    module_name: String,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProjectDetails {
    id: i32,
    project_name: String,
    root_path: String,
    modules: Vec<ProjectModule>,
    file_count: i64,
    symbol_count: i64,
}

#[derive(Debug, Deserialize, Serialize)]
struct IndexUploadResponse {
    project_id: i32,
}

#[derive(Debug, Deserialize, Serialize)]
struct IndexDeleteResponse {
    project_id: i32,
    deleted: bool,
}

enum ProjectSelector {
    Id(i32),
    Name(String),
}

const UPLOAD_CHUNK_SIZE: usize = 64 * 1024;

fn normalize_base_url(url: &str) -> String {
    let mut base_url = url.trim().to_string();
    if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
        base_url = format!("http://{}", base_url);
    }
    base_url.trim_end_matches('/').to_string()
}

fn build_client(timeout: u64) -> Result<reqwest::Client> {
    let mut client_builder = reqwest::Client::builder().no_proxy();
    if timeout > 0 {
        client_builder = client_builder.timeout(Duration::from_secs(timeout));
    }
    Ok(client_builder.build()?)
}

fn build_progress_bar(total: u64, enabled: bool) -> Option<ProgressBar> {
    if !enabled {
        return None;
    }
    let pb = ProgressBar::new(total);
    pb.set_style(
        ProgressStyle::with_template(
            "{msg} {bar:40.cyan/blue} {bytes}/{total_bytes} ({bytes_per_sec}, {eta})",
        )
        .unwrap(),
    );
    pb.set_message("Uploading");
    Some(pb)
}

fn resolve_token(token: Option<String>) -> Result<String> {
    let token = token.or_else(|| std::env::var("ASKL_TOKEN").ok());
    match token {
        Some(token) if !token.trim().is_empty() => Ok(token),
        _ => Err(anyhow!("Missing token; pass --token or set ASKL_TOKEN")),
    }
}

fn resolve_project_selector(id: Option<i32>, name: Option<String>) -> Result<ProjectSelector> {
    match (id, name) {
        (Some(_), Some(_)) => Err(anyhow!("Use either --id or --name")),
        (Some(id), None) => Ok(ProjectSelector::Id(id)),
        (None, Some(name)) => Ok(ProjectSelector::Name(name)),
        (None, None) => Err(anyhow!("Missing project selector; use --id or --name")),
    }
}

async fn fetch_projects(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
) -> Result<Vec<ProjectInfo>> {
    use reqwest::header as reqwest_header;

    let endpoint = format!("{}/v1/index/projects", base_url);
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

    Ok(response.json().await?)
}

async fn resolve_project_id(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    selector: ProjectSelector,
) -> Result<(i32, Option<String>)> {
    match selector {
        ProjectSelector::Id(id) => Ok((id, None)),
        ProjectSelector::Name(name) => {
            let projects = fetch_projects(client, base_url, token).await?;
            let mut matches = projects
                .into_iter()
                .filter(|project| project.project_name == name)
                .collect::<Vec<_>>();
            match matches.pop() {
                Some(project) => Ok((project.id, Some(project.project_name))),
                None => Err(anyhow!("Project not found: {}", name)),
            }
        }
    }
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

            let token = resolve_token(token)?;

            let base_url = normalize_base_url(&url);
            let endpoint = format!("{}/v1/index/projects", base_url);

            let client = build_client(timeout)?;
            let (body, content_len, progress) = if let Some(project_name) = project {
                let payload = tokio::fs::read(&file_path)
                    .await
                    .map_err(|err| anyhow!("Failed to read {}: {}", file_path, err))?;
                if payload.is_empty() {
                    return Err(anyhow!("Payload file is empty: {}", file_path));
                }

                let mut upload = Project::decode(payload.as_slice())
                    .map_err(|err| anyhow!("Failed to decode protobuf payload: {}", err))?;
                upload.project_name = project_name;
                let mut buffer = Vec::with_capacity(upload.encoded_len());
                upload.encode(&mut buffer)?;

                let total = buffer.len() as u64;
                let progress = build_progress_bar(total, !json);
                let progress_handle = progress.clone();
                let chunks: Vec<Bytes> = buffer
                    .chunks(UPLOAD_CHUNK_SIZE)
                    .map(Bytes::copy_from_slice)
                    .collect();
                let stream = futures::stream::iter(chunks.into_iter().map(move |chunk| {
                    if let Some(ref pb) = progress_handle {
                        pb.inc(chunk.len() as u64);
                    }
                    Ok::<Bytes, std::io::Error>(chunk)
                }));
                (reqwest::Body::wrap_stream(stream), total, progress)
            } else {
                let file = tokio::fs::File::open(&file_path)
                    .await
                    .map_err(|err| anyhow!("Failed to open {}: {}", file_path, err))?;
                let metadata = file
                    .metadata()
                    .await
                    .map_err(|err| anyhow!("Failed to read {} metadata: {}", file_path, err))?;
                if metadata.len() == 0 {
                    return Err(anyhow!("Payload file is empty: {}", file_path));
                }
                let total = metadata.len();
                let progress = build_progress_bar(total, !json);
                let progress_handle = progress.clone();
                let stream = FramedRead::new(file, BytesCodec::new()).map_ok(move |bytes| {
                    if let Some(ref pb) = progress_handle {
                        pb.inc(bytes.len() as u64);
                    }
                    bytes.freeze()
                });
                (reqwest::Body::wrap_stream(stream), total, progress)
            };

            let response = client
                .post(endpoint)
                .header(reqwest_header::CONTENT_TYPE, "application/x-protobuf")
                .header(reqwest_header::AUTHORIZATION, format!("Bearer {}", token))
                .header(reqwest_header::CONTENT_LENGTH, content_len)
                .body(body)
                .send()
                .await?;

            if let Some(progress) = progress {
                progress.finish_and_clear();
            }

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
            let token = resolve_token(token)?;

            let base_url = normalize_base_url(&url);
            let client = build_client(timeout)?;

            let projects = fetch_projects(&client, &base_url, &token).await?;
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
        IndexCommand::GetProject {
            id,
            name,
            url,
            token,
            timeout,
            json,
        } => {
            use reqwest::header as reqwest_header;

            let selector = resolve_project_selector(id, name)?;
            let token = resolve_token(token)?;
            let base_url = normalize_base_url(&url);
            let client = build_client(timeout)?;

            let (project_id, _) = resolve_project_id(&client, &base_url, &token, selector).await?;
            let endpoint = format!("{}/v1/index/projects/{}", base_url, project_id);

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

            let details: ProjectDetails = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&details)?;
                println!("{}", output);
            } else {
                println!("ID: {}", details.id);
                println!("Name: {}", details.project_name);
                println!("Files: {}", details.file_count);
                println!("Symbols: {}", details.symbol_count);
                if details.modules.is_empty() {
                    println!("Modules: none");
                } else {
                    println!("Modules:");
                    for module in details.modules {
                        println!("{} {}", module.id, module.module_name);
                    }
                }
            }
        }
        IndexCommand::DeleteProject {
            id,
            name,
            url,
            token,
            timeout,
            json,
        } => {
            use reqwest::header as reqwest_header;

            let selector = resolve_project_selector(id, name)?;
            let token = resolve_token(token)?;
            let base_url = normalize_base_url(&url);
            let client = build_client(timeout)?;

            let (project_id, project_name) =
                resolve_project_id(&client, &base_url, &token, selector).await?;
            let endpoint = format!("{}/v1/index/projects/{}", base_url, project_id);

            let response = client
                .delete(endpoint)
                .header(reqwest_header::AUTHORIZATION, format!("Bearer {}", token))
                .send()
                .await?;

            if !response.status().is_success() {
                let status = response.status();
                let body = response.text().await.unwrap_or_default();
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let result: IndexDeleteResponse = response.json().await?;
            if json {
                let output = serde_json::to_string_pretty(&result)?;
                println!("{}", output);
            } else if let Some(project_name) = project_name {
                println!("Deleted project {} ({})", result.project_id, project_name);
            } else {
                println!("Deleted project {}", result.project_id);
            }
        }
    }

    Ok(())
}
