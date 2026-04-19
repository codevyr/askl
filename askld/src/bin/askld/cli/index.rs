use crate::args::IndexCommand;
use anyhow::{anyhow, Result};
use askld::proto::askl::index::Project;
use bytes::Bytes;
use futures::TryStreamExt;
use indicatif::{ProgressBar, ProgressStyle};
use prost::Message;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::StatusCode;
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

fn build_client(timeout: u64) -> reqwest::Client {
    let mut builder = reqwest::Client::builder();
    if timeout > 0 {
        builder = builder.timeout(Duration::from_secs(timeout));
    }
    builder.build().expect("failed to build HTTP client")
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
    let endpoint = format!("{}/v1/index/projects", base_url);
    let response = client
        .get(endpoint)
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| anyhow!("Request failed: {}", e))?;

    if !response.status().is_success() {
        let status = response.status();
        let body_bytes = response.bytes().await.map_err(|e| anyhow!("{}", e))?;
        let body = String::from_utf8_lossy(&body_bytes);
        return Err(anyhow!("Request failed ({}): {}", status, body));
    }

    response
        .json()
        .await
        .map_err(|e| anyhow!("Failed to parse response: {}", e))
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

fn human_size(bytes: u64) -> String {
    if bytes >= 1024 * 1024 * 1024 {
        format!("{} GB", bytes / (1024 * 1024 * 1024))
    } else if bytes >= 1024 * 1024 {
        format!("{} MB", bytes / (1024 * 1024))
    } else if bytes >= 1024 {
        format!("{} KB", bytes / 1024)
    } else {
        format!("{} B", bytes)
    }
}

async fn stream_file_to_endpoint(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    file_path: &str,
    msg: &str,
    show_progress: bool,
) -> Result<(StatusCode, Vec<u8>)> {
    let file = tokio::fs::File::open(file_path)
        .await
        .map_err(|err| anyhow!("Failed to open {}: {}", file_path, err))?;
    let metadata = file
        .metadata()
        .await
        .map_err(|err| anyhow!("Failed to read {} metadata: {}", file_path, err))?;
    let total = metadata.len();

    let progress = build_progress_bar(total, show_progress);
    let progress_handle = progress.clone();
    if let Some(ref pb) = progress {
        pb.set_message(msg.to_string());
    }

    let stream = FramedRead::new(file, BytesCodec::new()).map_ok(move |bytes| {
        if let Some(ref pb) = progress_handle {
            pb.inc(bytes.len() as u64);
        }
        bytes.freeze()
    });

    let response = client
        .post(endpoint)
        .header(CONTENT_TYPE, "application/x-protobuf")
        .bearer_auth(token)
        .header(CONTENT_LENGTH, total.to_string())
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|e| anyhow!("Request failed: {}", e))?;

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }
    if show_progress {
        eprintln!("{}... {} done", msg, human_size(total));
    }

    let status = response.status();
    let body = response.bytes().await.map_err(|e| anyhow!("{}", e))?.to_vec();
    Ok((status, body))
}

fn check_upload_response(status: StatusCode, body: &[u8]) -> Result<()> {
    if status.is_success() {
        return Ok(());
    }
    let body_str = String::from_utf8_lossy(body);
    if status == StatusCode::CONFLICT {
        let message = if body_str.trim().is_empty() {
            "Project already exists".to_string()
        } else {
            body_str.to_string()
        };
        return Err(anyhow!("Request failed ({}): {}", status, message));
    }
    Err(anyhow!("Request failed ({}): {}", status, body_str))
}

fn print_upload_result(body: &[u8], json: bool) -> Result<()> {
    let result: IndexUploadResponse = serde_json::from_slice(body)
        .map_err(|e| anyhow!("Failed to parse response: {}", e))?;
    if json {
        let output = serde_json::to_string_pretty(&result)?;
        println!("{}", output);
    } else {
        println!("Uploaded index; project id: {}", result.project_id);
    }
    Ok(())
}

async fn upload_project_pb(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    file_path: &str,
    project_name: Option<String>,
    show_progress: bool,
) -> Result<(StatusCode, Vec<u8>)> {
    if let Some(project_name) = project_name {
        // Decode, override name, re-encode, stream with progress
        let payload = tokio::fs::read(file_path)
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
        let progress = build_progress_bar(total, show_progress);
        let progress_handle = progress.clone();
        if let Some(ref pb) = progress {
            pb.set_message("Uploading project".to_string());
        }
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

        let response = client
            .post(endpoint)
            .header(CONTENT_TYPE, "application/x-protobuf")
            .bearer_auth(token)
            .header(CONTENT_LENGTH, total.to_string())
            .body(reqwest::Body::wrap_stream(stream))
            .send()
            .await
            .map_err(|e| anyhow!("Request failed: {}", e))?;
        if let Some(pb) = progress {
            pb.finish_and_clear();
        }
        if show_progress {
            eprintln!("Uploading project... {} done", human_size(total));
        }

        let status = response.status();
        let body = response.bytes().await.map_err(|e| anyhow!("{}", e))?.to_vec();
        Ok((status, body))
    } else {
        stream_file_to_endpoint(client, endpoint, token, file_path, "Uploading project", show_progress).await
    }
}

async fn upload_single_file(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    file_path: &str,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    let endpoint = format!("{}/v1/index/projects", base_url);
    let (status, body) = upload_project_pb(client, &endpoint, token, file_path, project, !json).await?;
    check_upload_response(status, &body)?;
    print_upload_result(&body, json)
}

async fn upload_directory(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    dir_path: &str,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    // Discover files
    let project_pb = format!("{}/project.pb", dir_path);
    if !tokio::fs::try_exists(&project_pb)
        .await
        .unwrap_or(false)
    {
        return Err(anyhow!(
            "project.pb not found in directory: {}",
            dir_path
        ));
    }

    // Find content batch files sorted by name
    let mut content_files: Vec<String> = Vec::new();
    let mut entries = tokio::fs::read_dir(dir_path)
        .await
        .map_err(|e| anyhow!("Failed to read directory {}: {}", dir_path, e))?;
    while let Some(entry) = entries
        .next_entry()
        .await
        .map_err(|e| anyhow!("Failed to read directory entry: {}", e))?
    {
        let name = entry.file_name().to_string_lossy().to_string();
        if name.starts_with("contents-") && name.ends_with(".pb") {
            content_files.push(name);
        }
    }
    content_files.sort();

    let contents_endpoint = format!("{}/v1/index/contents", base_url);
    let show_progress = !json;

    // Upload content parts sequentially
    let total_parts = content_files.len();
    for (i, name) in content_files.iter().enumerate() {
        let file_path = format!("{}/{}", dir_path, name);
        let msg = format!("Uploading contents ({}/{})", i + 1, total_parts);
        let (status, body) = stream_file_to_endpoint(
            client,
            &contents_endpoint,
            token,
            &file_path,
            &msg,
            show_progress,
        )
        .await?;

        if !status.is_success() {
            let body_str = String::from_utf8_lossy(&body);
            return Err(anyhow!(
                "Content upload failed ({}): {}",
                status,
                body_str
            ));
        }
    }

    // Upload project
    let projects_endpoint = format!("{}/v1/index/projects", base_url);
    let (status, body) = upload_project_pb(client, &projects_endpoint, token, &project_pb, project, show_progress).await?;
    check_upload_response(status, &body)?;
    print_upload_result(&body, json)
}

pub async fn run_index_command(command: IndexCommand) -> Result<()> {
    match command {
        IndexCommand::Upload {
            index,
            url,
            token,
            project,
            timeout,
            json,
        } => {
            let token = resolve_token(token)?;
            let base_url = normalize_base_url(&url);
            let client = build_client(timeout);

            let path = std::path::Path::new(&index);
            if path.is_dir() {
                upload_directory(&client, &base_url, &token, &index, project, json).await?;
            } else if path.is_file() {
                upload_single_file(&client, &base_url, &token, &index, project, json).await?;
            } else {
                return Err(anyhow!("Index path does not exist: {}", index));
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
            let client = build_client(timeout);

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
            let selector = resolve_project_selector(id, name)?;
            let token = resolve_token(token)?;
            let base_url = normalize_base_url(&url);
            let client = build_client(timeout);

            let (project_id, _) = resolve_project_id(&client, &base_url, &token, selector).await?;
            let endpoint = format!("{}/v1/index/projects/{}", base_url, project_id);

            let response = client
                .get(endpoint)
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| anyhow!("Request failed: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let body_bytes = response.bytes().await.map_err(|e| anyhow!("{}", e))?;
                let body = String::from_utf8_lossy(&body_bytes);
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let details: ProjectDetails =
                response.json().await.map_err(|e| anyhow!("{}", e))?;
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
            let selector = resolve_project_selector(id, name)?;
            let token = resolve_token(token)?;
            let base_url = normalize_base_url(&url);
            let client = build_client(timeout);

            let (project_id, project_name) =
                resolve_project_id(&client, &base_url, &token, selector).await?;
            let endpoint = format!("{}/v1/index/projects/{}", base_url, project_id);

            let response = client
                .delete(endpoint)
                .bearer_auth(&token)
                .send()
                .await
                .map_err(|e| anyhow!("Request failed: {}", e))?;

            if !response.status().is_success() {
                let status = response.status();
                let body_bytes = response.bytes().await.map_err(|e| anyhow!("{}", e))?;
                let body = String::from_utf8_lossy(&body_bytes);
                return Err(anyhow!("Request failed ({}): {}", status, body));
            }

            let result: IndexDeleteResponse =
                response.json().await.map_err(|e| anyhow!("{}", e))?;
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
