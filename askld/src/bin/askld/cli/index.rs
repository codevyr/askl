use std::borrow::Cow;

use crate::args::IndexCommand;
use anyhow::{anyhow, Result};
use askld::index_store::UploadStatus;
use askld::proto::askl::index::{ContentBatch, Project};
use bytes::Bytes;
use indicatif::{ProgressBar, ProgressStyle};
use prost::Message;
use reqwest::header::{CONTENT_LENGTH, CONTENT_TYPE};
use reqwest::StatusCode;
use serde::{Deserialize, Serialize};
use tokio::time::Duration;

#[derive(Debug, Deserialize, Serialize)]
struct ProjectInfo {
    id: i32,
    project_name: String,
    root_path: String,
    upload_status: UploadStatus,
}

#[derive(Debug, Deserialize, Serialize)]
struct ProjectDetails {
    id: i32,
    project_name: String,
    root_path: String,
    upload_status: UploadStatus,
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
const CONTENTS_UPLOAD_MAX_BYTES: usize = 128 * 1024 * 1024;
const OBJECTS_UPLOAD_MAX_BYTES: usize = 200 * 1024 * 1024;

/// Typed API endpoint builder — eliminates repeated raw-string URL construction.
struct Endpoints<'a> {
    base_url: &'a str,
}

impl<'a> Endpoints<'a> {
    fn new(base_url: &'a str) -> Self {
        Self { base_url }
    }
    fn projects(&self) -> String {
        format!("{}/v1/index/projects", self.base_url)
    }
    fn project(&self, id: i32) -> String {
        format!("{}/v1/index/projects/{}", self.base_url, id)
    }
    fn project_objects(&self, id: i32) -> String {
        format!("{}/v1/index/projects/{}/objects", self.base_url, id)
    }
    fn project_finalize(&self, id: i32) -> String {
        format!("{}/v1/index/projects/{}/finalize", self.base_url, id)
    }
    fn contents(&self) -> String {
        format!("{}/v1/index/contents", self.base_url)
    }
}

fn set_progress_msg(progress: &Option<ProgressBar>, msg: impl Into<Cow<'static, str>>) {
    if let Some(pb) = progress {
        pb.set_message(msg);
    }
}

/// Best-effort DELETE on upload failure.
///
/// Returns `Ok(())` if the server accepted the delete, or `Err(msg)` with a
/// human-readable description so callers can distinguish "cleaned up" from
/// "project orphaned and needs manual removal".
async fn try_delete_project(
    client: &reqwest::Client,
    url: &str,
    token: &str,
) -> Result<(), String> {
    match client.delete(url).bearer_auth(token).send().await {
        Ok(r) if r.status().is_success() => Ok(()),
        Ok(r) => Err(format!("delete failed ({})", r.status())),
        Err(e) => Err(format!("delete failed: {}", e)),
    }
}

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
    let endpoint = Endpoints::new(base_url).projects();
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
            match matches.len() {
                0 => Err(anyhow!("Project not found: {}", name)),
                1 => {
                    let p = matches.remove(0);
                    Ok((p.id, Some(p.project_name)))
                }
                n => Err(anyhow!(
                    "{} projects named {:?} — use --id to disambiguate",
                    n, name
                )),
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

async fn upload_single_file(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    file_path: &str,
    project: Option<String>,
    json: bool,
) -> Result<()> {
    let show_progress = !json;
    let total = tokio::fs::metadata(file_path)
        .await
        .map_err(|e| anyhow!("Failed to stat {}: {}", file_path, e))?
        .len();
    let progress = build_progress_bar(total, show_progress);
    let result = upload_batched_project(client, base_url, token, file_path, project, &progress).await?;
    if let Some(pb) = progress {
        pb.finish_and_clear();
    }
    if show_progress {
        eprintln!("Uploading project... {} done", human_size(total));
    }
    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Uploaded index; project id: {}", result.project_id);
    }
    Ok(())
}

/// Stream a pre-encoded buffer in UPLOAD_CHUNK_SIZE chunks, updating a shared progress bar.
async fn stream_bytes(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    buf: Vec<u8>,
    progress: &Option<ProgressBar>,
) -> Result<(StatusCode, Vec<u8>)> {
    let buf_len = buf.len() as u64;
    let progress_handle = progress.clone();
    let chunks: Vec<Bytes> = buf
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
        .header(CONTENT_LENGTH, buf_len.to_string())
        .body(reqwest::Body::wrap_stream(stream))
        .send()
        .await
        .map_err(|e| anyhow!("Request failed: {}", e))?;

    let status = response.status();
    let body = response.bytes().await.map_err(|e| anyhow!("{}", e))?.to_vec();
    Ok((status, body))
}

async fn upload_content_file(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    file_path: &str,
    progress: &Option<ProgressBar>,
) -> Result<()> {
    let data = tokio::fs::read(file_path)
        .await
        .map_err(|e| anyhow!("Failed to read {}: {}", file_path, e))?;
    let batch = ContentBatch::decode(data.as_slice())
        .map_err(|e| anyhow!("Failed to decode {}: {}", file_path, e))?;

    // Partition into sub-batches by raw content size
    let mut sub_batches: Vec<ContentBatch> = Vec::new();
    let mut current = ContentBatch::default();
    let mut current_size: usize = 0;

    for entry in batch.contents {
        let entry_size = entry.content.len();
        if current_size + entry_size > CONTENTS_UPLOAD_MAX_BYTES && !current.contents.is_empty() {
            sub_batches.push(std::mem::take(&mut current));
            current_size = 0;
        }
        current_size += entry_size;
        current.contents.push(entry);
    }
    if !current.contents.is_empty() {
        sub_batches.push(current);
    }

    for sub_batch in sub_batches {
        let mut buf = Vec::with_capacity(sub_batch.encoded_len());
        sub_batch.encode(&mut buf)?;
        let (status, body) = stream_bytes(client, endpoint, token, buf, progress).await?;
        if !status.is_success() {
            let body_str = String::from_utf8_lossy(&body);
            return Err(anyhow!("Content upload failed ({}): {}", status, body_str));
        }
    }

    Ok(())
}

async fn upload_batched_project(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    file_path: &str,
    project_name_override: Option<String>,
    progress: &Option<ProgressBar>,
) -> Result<IndexUploadResponse> {
    let data = tokio::fs::read(file_path)
        .await
        .map_err(|e| anyhow!("Failed to read {}: {}", file_path, e))?;
    let mut upload = Project::decode(data.as_slice())
        .map_err(|e| anyhow!("Failed to decode protobuf: {}", e))?;
    drop(data);

    if let Some(name) = project_name_override {
        upload.project_name = name;
    }
    let all_objects = std::mem::take(&mut upload.objects);
    let total_objects = all_objects.len();

    // Build batches by accumulating objects until the encoded size limit is reached,
    // using each object's actual encoded length rather than guessing from the file size.
    let mut batches = Vec::new();
    let mut current = Vec::new();
    let mut current_size: usize = 0;
    for object in all_objects {
        let object_size = object.encoded_len();
        if current_size + object_size > OBJECTS_UPLOAD_MAX_BYTES && !current.is_empty() {
            batches.push(std::mem::take(&mut current));
            current_size = 0;
        }
        current_size += object_size;
        current.push(object);
    }
    if !current.is_empty() {
        batches.push(current);
    }
    let total_batches = batches.len();

    let ep = Endpoints::new(base_url);

    // Phase 1: POST header (symbols only, no objects)
    set_progress_msg(progress, "Uploading project header");
    let mut header_buf = Vec::with_capacity(upload.encoded_len());
    upload.encode(&mut header_buf)?;
    let (status, body) = stream_bytes(client, &ep.projects(), token, header_buf, progress).await?;
    check_upload_response(status, &body)?;
    let response: IndexUploadResponse = serde_json::from_slice(&body)
        .map_err(|e| anyhow!("Failed to parse response: {}", e))?;
    let project_id = response.project_id;
    let project_url = ep.project(project_id);

    // Phase 2: POST object batches
    let mut uploaded_objects: usize = 0;
    for (i, batch) in batches.into_iter().enumerate() {
        let batch_len = batch.len();
        set_progress_msg(
            progress,
            format!("Uploading objects ({}/{})", uploaded_objects, total_objects),
        );
        let batch_msg = Project { objects: batch, ..Default::default() };
        let mut buf = Vec::with_capacity(batch_msg.encoded_len());
        batch_msg.encode(&mut buf)?;
        let (status, body) =
            stream_bytes(client, &ep.project_objects(project_id), token, buf, progress).await?;
        if !status.is_success() {
            let cleanup = try_delete_project(client, &project_url, token).await;
            let cleanup_note = match cleanup {
                Ok(()) => "project deleted".to_string(),
                Err(msg) => format!("WARNING: {msg} — project may be orphaned at {project_url}"),
            };
            return Err(anyhow!(
                "Object batch {}/{} failed ({}): {} — {}",
                i + 1, total_batches, status,
                String::from_utf8_lossy(&body),
                cleanup_note
            ));
        }
        uploaded_objects += batch_len;
    }

    // Phase 3: finalize so the project becomes visible
    set_progress_msg(progress, "Finalizing");
    let fin_response = client
        .post(&ep.project_finalize(project_id))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| anyhow!("Failed to finalize project {}: {}", project_id, e))?;
    if !fin_response.status().is_success() {
        let fin_status = fin_response.status();
        let fin_body = fin_response.bytes().await.map_err(|e| anyhow!("{}", e))?;
        let cleanup = try_delete_project(client, &project_url, token).await;
        let cleanup_note = match cleanup {
            Ok(()) => "project deleted".to_string(),
            Err(msg) => format!("WARNING: {msg} — project may be orphaned at {project_url}"),
        };
        return Err(anyhow!(
            "Finalize failed ({}): {} — {}",
            fin_status,
            String::from_utf8_lossy(&fin_body),
            cleanup_note
        ));
    }

    Ok(response)
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

    let ep = Endpoints::new(base_url);
    let show_progress = !json;

    // Compute total bytes across all files for the unified progress bar
    let mut total_bytes: u64 = tokio::fs::metadata(&project_pb)
        .await
        .map_err(|e| anyhow!("Failed to stat {}: {}", project_pb, e))?
        .len();
    for name in &content_files {
        total_bytes += tokio::fs::metadata(format!("{}/{}", dir_path, name))
            .await
            .map_err(|e| anyhow!("Failed to stat {}: {}", name, e))?
            .len();
    }

    let progress = build_progress_bar(total_bytes, show_progress);

    // Upload content parts sequentially
    let total_parts = content_files.len();
    for (i, name) in content_files.iter().enumerate() {
        let file_path = format!("{}/{}", dir_path, name);
        set_progress_msg(&progress, format!("Uploading contents ({}/{})", i + 1, total_parts));
        upload_content_file(client, &ep.contents(), token, &file_path, &progress).await?;
    }

    // Upload project (always batched: header POST then object batch POSTs)
    let result = upload_batched_project(client, base_url, token, &project_pb, project, &progress)
        .await
        .map_err(|e| {
            if !content_files.is_empty() {
                anyhow!(
                    "{}\nNote: content blobs were already uploaded and remain on the server.",
                    e
                )
            } else {
                e
            }
        })?;

    if let Some(pb) = progress {
        pb.finish_and_clear();
    }
    if show_progress {
        eprintln!("Uploaded {} total", human_size(total_bytes));
    }

    if json {
        println!("{}", serde_json::to_string_pretty(&result)?);
    } else {
        println!("Uploaded index; project id: {}", result.project_id);
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    // --- normalize_base_url ---

    #[test]
    fn normalize_base_url_adds_http_scheme() {
        assert_eq!(normalize_base_url("example.com"), "http://example.com");
    }

    #[test]
    fn normalize_base_url_preserves_https() {
        assert_eq!(normalize_base_url("https://example.com"), "https://example.com");
    }

    #[test]
    fn normalize_base_url_preserves_http() {
        assert_eq!(normalize_base_url("http://example.com"), "http://example.com");
    }

    #[test]
    fn normalize_base_url_strips_trailing_slash() {
        assert_eq!(normalize_base_url("http://example.com/"), "http://example.com");
    }

    #[test]
    fn normalize_base_url_strips_multiple_trailing_slashes() {
        assert_eq!(normalize_base_url("https://example.com///"), "https://example.com");
    }

    // --- human_size ---

    #[test]
    fn human_size_sub_kilobyte() {
        assert_eq!(human_size(0), "0 B");
        assert_eq!(human_size(1023), "1023 B");
    }

    #[test]
    fn human_size_kilobytes() {
        assert_eq!(human_size(1024), "1 KB");
        assert_eq!(human_size(3 * 1024), "3 KB");
    }

    #[test]
    fn human_size_megabytes() {
        assert_eq!(human_size(1024 * 1024), "1 MB");
        assert_eq!(human_size(5 * 1024 * 1024), "5 MB");
    }

    #[test]
    fn human_size_gigabytes() {
        assert_eq!(human_size(1024 * 1024 * 1024), "1 GB");
        assert_eq!(human_size(2 * 1024 * 1024 * 1024), "2 GB");
    }

    // --- Endpoints ---

    #[test]
    fn endpoints_projects() {
        assert_eq!(
            Endpoints::new("http://api.example.com").projects(),
            "http://api.example.com/v1/index/projects"
        );
    }

    #[test]
    fn endpoints_project() {
        assert_eq!(
            Endpoints::new("http://api.example.com").project(42),
            "http://api.example.com/v1/index/projects/42"
        );
    }

    #[test]
    fn endpoints_project_objects() {
        assert_eq!(
            Endpoints::new("http://api.example.com").project_objects(7),
            "http://api.example.com/v1/index/projects/7/objects"
        );
    }

    #[test]
    fn endpoints_project_finalize() {
        assert_eq!(
            Endpoints::new("http://api.example.com").project_finalize(3),
            "http://api.example.com/v1/index/projects/3/finalize"
        );
    }

    #[test]
    fn endpoints_contents() {
        assert_eq!(
            Endpoints::new("http://api.example.com").contents(),
            "http://api.example.com/v1/index/contents"
        );
    }

    #[test]
    fn endpoints_no_double_slash_when_base_has_no_trailing_slash() {
        // normalize_base_url strips the trailing slash, so Endpoints should never
        // produce double slashes in practice — verify the raw struct holds the contract too
        let ep = Endpoints::new("http://example.com");
        assert!(!ep.projects().contains("//v1"));
        assert!(!ep.project(1).contains("//v1"));
    }
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
                    if project.upload_status == UploadStatus::Complete {
                        println!("{} {}", project.id, project.project_name);
                    } else {
                        println!("{} {} [{}]", project.id, project.project_name, project.upload_status);
                    }
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
            let endpoint = Endpoints::new(&base_url).project(project_id);

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
                println!("Status: {}", details.upload_status);
                println!("Files: {}", details.file_count);
                println!("Symbols: {}", details.symbol_count);
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
            let endpoint = Endpoints::new(&base_url).project(project_id);

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
