use std::borrow::Cow;
use std::collections::HashSet;
use std::sync::Arc;

use crate::args::IndexCommand;
use anyhow::{anyhow, Result};
use askld::index_store::UploadStatus;
use askld::proto::askl::index::{ContentBatch, Project};
use bytes::Bytes;
use futures::stream::{FuturesUnordered, StreamExt};
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
    symbol_chunks_total: Option<i32>,
    object_chunks_total: Option<i32>,
    #[serde(default)]
    committed_symbol_chunks: Vec<i32>,
    #[serde(default)]
    committed_object_chunks: Vec<i32>,
}

#[derive(Debug, Deserialize, Serialize)]
struct IndexUploadResponse {
    project_id: i32,
    #[serde(default)]
    resumed: bool,
}

#[derive(Debug, Deserialize, Serialize)]
struct IndexDeleteResponse {
    project_id: i32,
    deleted: bool,
}

struct ProjectState {
    project_id: i32,
    committed_symbol_seqs: HashSet<i32>,
    committed_object_seqs: HashSet<i32>,
}

enum ProjectSelector {
    Id(i32),
    Name(String),
}

const UPLOAD_CHUNK_SIZE: usize = 64 * 1024;
const CONTENTS_UPLOAD_MAX_BYTES: usize = 128 * 1024 * 1024;
const CONTENTS_HASH_BATCH_SIZE: usize = 10_000;
const SYMBOLS_PER_CHUNK: usize = 1_000;
const OBJECTS_PER_CHUNK: usize = 100;
const CHUNK_MAX_RETRIES: u32 = 3;

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
    fn project_symbols(&self, id: i32) -> String {
        format!("{}/v1/index/projects/{}/symbols", self.base_url, id)
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
    fn contents_check(&self) -> String {
        format!("{}/v1/index/contents/check", self.base_url)
    }
}

fn set_progress_msg(progress: &Option<ProgressBar>, msg: impl Into<Cow<'static, str>>) {
    if let Some(pb) = progress {
        pb.set_message(msg);
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

/// Stream a pre-encoded buffer in UPLOAD_CHUNK_SIZE pieces to a protobuf endpoint.
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

/// POST a single chunk to `endpoint?seq=N` with retry on transient failures.
///
/// Idempotent: 409 Conflict means the server already committed this chunk → Ok(()).
async fn upload_chunk_with_retry(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    seq: i32,
    buf: Bytes,
) -> Result<()> {
    let url = format!("{}?seq={}", endpoint, seq);
    let mut last_err = anyhow!("no attempts made");
    for attempt in 0..=CHUNK_MAX_RETRIES {
        if attempt > 0 {
            let delay = Duration::from_secs(1u64 << (attempt - 1)); // 1s, 2s, 4s
            tokio::time::sleep(delay).await;
        }
        let response = match client
            .post(&url)
            .header(CONTENT_TYPE, "application/x-protobuf")
            .bearer_auth(token)
            .body(buf.clone())
            .send()
            .await
        {
            Ok(r) => r,
            Err(e) => {
                last_err = anyhow!("Network error: {}", e);
                continue;
            }
        };
        let status = response.status();
        let body = response.bytes().await.map_err(|e| anyhow!("{}", e))?.to_vec();
        if status.is_success() {
            return Ok(());
        }
        if status == StatusCode::CONFLICT {
            return Ok(()); // already committed — idempotent
        }
        if status.is_server_error() {
            last_err = anyhow!(
                "Server error ({}): {}",
                status,
                String::from_utf8_lossy(&body)
            );
            continue; // retry 5xx
        }
        // 4xx (except 409) → not retriable
        return Err(anyhow!(
            "Chunk seq={} failed ({}): {}",
            seq,
            status,
            String::from_utf8_lossy(&body)
        ));
    }
    Err(anyhow!(
        "Chunk seq={} failed after {} retries: {}",
        seq,
        CHUNK_MAX_RETRIES,
        last_err
    ))
}

/// Upload all chunks with at most `window` in flight at once.
///
/// Chunks whose seq is in `committed` are skipped. Awaiting this function provides
/// a phase barrier — callers must await symbol chunks before starting object chunks
/// to satisfy FK constraints.
async fn upload_chunks_windowed(
    client: reqwest::Client,
    endpoint: String,
    token: String,
    chunks: Vec<(i32, Bytes)>,
    committed: &HashSet<i32>,
    window: usize,
    progress: &Option<ProgressBar>,
) -> Result<()> {
    let sem = Arc::new(tokio::sync::Semaphore::new(window));
    let mut tasks: FuturesUnordered<tokio::task::JoinHandle<Result<()>>> = FuturesUnordered::new();

    for (seq, buf) in chunks {
        if committed.contains(&seq) {
            if let Some(pb) = progress {
                pb.inc(buf.len() as u64);
            }
            continue;
        }
        let permit = sem.clone().acquire_owned().await
            .map_err(|e| anyhow!("Semaphore closed: {}", e))?;
        let client = client.clone();
        let endpoint = endpoint.clone();
        let token = token.clone();
        let pb = progress.clone();
        let buf_len = buf.len() as u64;
        tasks.push(tokio::spawn(async move {
            let _permit = permit;
            let result = upload_chunk_with_retry(&client, &endpoint, &token, seq, buf).await;
            if let Some(pb) = pb {
                pb.inc(buf_len);
            }
            result
        }));
    }

    while let Some(result) = tasks.next().await {
        result.map_err(|e| anyhow!("Task join error: {}", e))??;
    }
    Ok(())
}

/// Create or resume a project, returning its id and committed chunk sets.
async fn ensure_project(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    project_name: &str,
    root_path: &str,
    symbol_chunks: i32,
    object_chunks: i32,
    force: bool,
    progress: &Option<ProgressBar>,
) -> Result<ProjectState> {
    if force {
        set_progress_msg(progress, "Checking for existing project");
        let projects = fetch_projects(client, base_url, token).await?;
        if let Some(existing) = projects.iter().find(|p| p.project_name == project_name) {
            set_progress_msg(progress, "Deleting existing project");
            let del_url = Endpoints::new(base_url).project(existing.id);
            let resp = client
                .delete(&del_url)
                .bearer_auth(token)
                .send()
                .await
                .map_err(|e| anyhow!("Delete request failed: {}", e))?;
            if !resp.status().is_success() && resp.status() != StatusCode::NOT_FOUND {
                let status = resp.status();
                let body = resp.bytes().await.map_err(|e| anyhow!("{}", e))?;
                return Err(anyhow!(
                    "Failed to delete existing project ({}): {}",
                    status,
                    String::from_utf8_lossy(&body)
                ));
            }
        }
    }

    set_progress_msg(progress, "Creating project");
    let create_url = format!(
        "{}/v1/index/projects?symbol_chunks={}&object_chunks={}",
        base_url, symbol_chunks, object_chunks
    );
    let skeleton = Project {
        project_name: project_name.to_string(),
        root_path: root_path.to_string(),
        ..Default::default()
    };
    let mut buf = Vec::with_capacity(skeleton.encoded_len());
    skeleton.encode(&mut buf)?;

    let response = client
        .post(&create_url)
        .header(CONTENT_TYPE, "application/x-protobuf")
        .bearer_auth(token)
        .body(buf)
        .send()
        .await
        .map_err(|e| anyhow!("Request failed: {}", e))?;

    let status = response.status();
    let body = response.bytes().await.map_err(|e| anyhow!("{}", e))?;
    if !status.is_success() {
        return Err(anyhow!(
            "Create project failed ({}): {}",
            status,
            String::from_utf8_lossy(&body)
        ));
    }

    let resp: IndexUploadResponse = serde_json::from_slice(&body)
        .map_err(|e| anyhow!("Failed to parse create response: {}", e))?;
    let project_id = resp.project_id;

    let mut committed_symbol_seqs = HashSet::new();
    let mut committed_object_seqs = HashSet::new();

    if resp.resumed {
        let details_url = Endpoints::new(base_url).project(project_id);
        let det_resp = client
            .get(&details_url)
            .bearer_auth(token)
            .send()
            .await
            .map_err(|e| anyhow!("Request failed: {}", e))?;
        if det_resp.status().is_success() {
            let details: ProjectDetails = det_resp
                .json()
                .await
                .map_err(|e| anyhow!("Failed to parse project details: {}", e))?;
            committed_symbol_seqs = details.committed_symbol_chunks.into_iter().collect();
            committed_object_seqs = details.committed_object_chunks.into_iter().collect();
            eprintln!(
                "Resuming project {} (symbol chunks: {}/{}, object chunks: {}/{})",
                project_id,
                committed_symbol_seqs.len(),
                symbol_chunks,
                committed_object_seqs.len(),
                object_chunks
            );
        }
    }

    Ok(ProjectState {
        project_id,
        committed_symbol_seqs,
        committed_object_seqs,
    })
}

/// Batch-check which content hashes are already in the store.
async fn check_hashes_present(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    hashes: Vec<String>,
) -> Result<HashSet<String>> {
    if hashes.is_empty() {
        return Ok(HashSet::new());
    }
    let endpoint = Endpoints::new(base_url).contents_check();
    let mut present = HashSet::new();
    for batch in hashes.chunks(CONTENTS_HASH_BATCH_SIZE) {
        let resp = client
            .post(&endpoint)
            .bearer_auth(token)
            .json(&serde_json::json!({ "hashes": batch }))
            .send()
            .await
            .map_err(|e| anyhow!("Hash check request failed: {}", e))?;
        if !resp.status().is_success() {
            let status = resp.status();
            let body = resp.bytes().await.map_err(|e| anyhow!("{}", e))?;
            return Err(anyhow!(
                "Hash check failed ({}): {}",
                status,
                String::from_utf8_lossy(&body)
            ));
        }
        #[derive(Deserialize)]
        struct CheckResp {
            present: Vec<String>,
        }
        let cr: CheckResp = resp.json().await.map_err(|e| anyhow!("{}", e))?;
        present.extend(cr.present);
    }
    Ok(present)
}

/// Upload a single content batch file, skipping entries whose hashes are already present.
async fn upload_content_file_filtered(
    client: &reqwest::Client,
    endpoint: &str,
    token: &str,
    file_path: &str,
    present_hashes: &HashSet<String>,
    progress: &Option<ProgressBar>,
) -> Result<()> {
    let data = tokio::fs::read(file_path)
        .await
        .map_err(|e| anyhow!("Failed to read {}: {}", file_path, e))?;
    let batch = ContentBatch::decode(data.as_slice())
        .map_err(|e| anyhow!("Failed to decode {}: {}", file_path, e))?;

    let mut sub_batches: Vec<ContentBatch> = Vec::new();
    let mut current = ContentBatch::default();
    let mut current_size: usize = 0;

    for entry in batch.contents {
        if present_hashes.contains(&entry.content_hash) {
            if let Some(pb) = progress {
                pb.inc(entry.content.len() as u64);
            }
            continue;
        }
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
            return Err(anyhow!(
                "Content upload failed ({}): {}",
                status,
                String::from_utf8_lossy(&body)
            ));
        }
    }

    Ok(())
}

/// Core upload logic: chunked symbol + object upload with content dedup.
///
/// `content_file_paths` is a sorted list of `contents-*.pb` files to upload.
/// On failure prints the project_id for resume and propagates the error.
async fn upload_project_core(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    project_name: String,
    root_path: String,
    all_symbols: Vec<askld::proto::askl::index::Symbol>,
    all_objects: Vec<askld::proto::askl::index::Object>,
    content_file_paths: Vec<String>,
    json: bool,
    window: usize,
    force: bool,
) -> Result<i32> {
    let show_progress = !json;

    // Encode symbol chunks
    let symbol_chunks: Vec<(i32, Bytes)> = all_symbols
        .chunks(SYMBOLS_PER_CHUNK)
        .enumerate()
        .map(|(seq, symbols)| {
            let msg = Project {
                symbols: symbols.to_vec(),
                ..Default::default()
            };
            let mut buf = Vec::with_capacity(msg.encoded_len());
            msg.encode(&mut buf)
                .map_err(|e| anyhow!("Failed to encode symbol chunk {}: {}", seq, e))?;
            Ok((seq as i32, Bytes::from(buf)))
        })
        .collect::<Result<Vec<_>>>()?;

    // Encode object chunks
    let object_chunks: Vec<(i32, Bytes)> = all_objects
        .chunks(OBJECTS_PER_CHUNK)
        .enumerate()
        .map(|(seq, objects)| {
            let msg = Project {
                objects: objects.to_vec(),
                ..Default::default()
            };
            let mut buf = Vec::with_capacity(msg.encoded_len());
            msg.encode(&mut buf)
                .map_err(|e| anyhow!("Failed to encode object chunk {}: {}", seq, e))?;
            Ok((seq as i32, Bytes::from(buf)))
        })
        .collect::<Result<Vec<_>>>()?;

    let n_symbol_chunks = symbol_chunks.len() as i32;
    let n_object_chunks = object_chunks.len() as i32;

    // Estimate total bytes for progress bar
    let sym_bytes: u64 = symbol_chunks.iter().map(|(_, b)| b.len() as u64).sum();
    let obj_bytes: u64 = object_chunks.iter().map(|(_, b)| b.len() as u64).sum();
    let mut content_bytes: u64 = 0;
    for path in &content_file_paths {
        content_bytes += tokio::fs::metadata(path).await.map(|m| m.len()).unwrap_or(0);
    }
    let total_bytes = sym_bytes + obj_bytes + content_bytes;
    let progress = build_progress_bar(total_bytes, show_progress);

    let state = ensure_project(
        client,
        base_url,
        token,
        &project_name,
        &root_path,
        n_symbol_chunks,
        n_object_chunks,
        force,
        &progress,
    )
    .await
    .map_err(|e| {
        if let Some(ref pb) = progress {
            pb.abandon();
        }
        e
    })?;
    let project_id = state.project_id;

    let result = do_upload(
        client,
        base_url,
        token,
        project_id,
        symbol_chunks,
        object_chunks,
        n_symbol_chunks,
        n_object_chunks,
        content_file_paths,
        &state,
        window,
        &progress,
    )
    .await;

    match result {
        Ok(()) => {
            if let Some(pb) = progress {
                pb.finish_and_clear();
            }
            if show_progress {
                eprintln!("Upload complete ({} total)", human_size(total_bytes));
            }
            Ok(project_id)
        }
        Err(e) => {
            if let Some(pb) = progress {
                pb.abandon();
            }
            eprintln!(
                "Upload interrupted (project id: {}). Re-run the same command to resume.",
                project_id
            );
            Err(e)
        }
    }
}

async fn do_upload(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    project_id: i32,
    symbol_chunks: Vec<(i32, Bytes)>,
    object_chunks: Vec<(i32, Bytes)>,
    n_symbol_chunks: i32,
    n_object_chunks: i32,
    content_file_paths: Vec<String>,
    state: &ProjectState,
    window: usize,
    progress: &Option<ProgressBar>,
) -> Result<()> {
    let ep = Endpoints::new(base_url);

    // Content phase: collect all hashes, check which are present, upload missing
    if !content_file_paths.is_empty() {
        set_progress_msg(progress, "Checking content hashes");
        let mut all_hashes = Vec::new();
        for path in &content_file_paths {
            let data = tokio::fs::read(path)
                .await
                .map_err(|e| anyhow!("Failed to read {}: {}", path, e))?;
            let batch = ContentBatch::decode(data.as_slice())
                .map_err(|e| anyhow!("Failed to decode {}: {}", path, e))?;
            for entry in &batch.contents {
                if !entry.content_hash.is_empty() {
                    all_hashes.push(entry.content_hash.clone());
                }
            }
        }
        let present_hashes = check_hashes_present(client, base_url, token, all_hashes).await?;

        let total_parts = content_file_paths.len();
        for (i, path) in content_file_paths.iter().enumerate() {
            set_progress_msg(
                progress,
                format!("Uploading contents ({}/{})", i + 1, total_parts),
            );
            upload_content_file_filtered(
                client,
                &ep.contents(),
                token,
                path,
                &present_hashes,
                progress,
            )
            .await?;
        }
    }

    // Symbol phase
    if !symbol_chunks.is_empty() {
        set_progress_msg(
            progress,
            format!("Uploading symbols (0/{})", n_symbol_chunks),
        );
        upload_chunks_windowed(
            client.clone(),
            ep.project_symbols(project_id),
            token.to_string(),
            symbol_chunks,
            &state.committed_symbol_seqs,
            window,
            progress,
        )
        .await?;
    }

    // Object phase — must wait for all symbol chunks first (FK: symbol_instances → symbols)
    if !object_chunks.is_empty() {
        set_progress_msg(
            progress,
            format!("Uploading objects (0/{})", n_object_chunks),
        );
        upload_chunks_windowed(
            client.clone(),
            ep.project_objects(project_id),
            token.to_string(),
            object_chunks,
            &state.committed_object_seqs,
            window,
            progress,
        )
        .await?;
    }

    // Finalize
    set_progress_msg(progress, "Finalizing");
    let fin_resp = client
        .post(&ep.project_finalize(project_id))
        .bearer_auth(token)
        .send()
        .await
        .map_err(|e| anyhow!("Failed to finalize project {}: {}", project_id, e))?;
    if !fin_resp.status().is_success() {
        let fin_status = fin_resp.status();
        let fin_body = fin_resp.bytes().await.map_err(|e| anyhow!("{}", e))?;
        return Err(anyhow!(
            "Finalize failed ({}): {}",
            fin_status,
            String::from_utf8_lossy(&fin_body)
        ));
    }

    Ok(())
}

async fn upload_single_file(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    file_path: &str,
    project_name_override: Option<String>,
    json: bool,
    window: usize,
    force: bool,
) -> Result<()> {
    let data = tokio::fs::read(file_path)
        .await
        .map_err(|e| anyhow!("Failed to read {}: {}", file_path, e))?;
    let mut upload = Project::decode(data.as_slice())
        .map_err(|e| anyhow!("Failed to decode protobuf: {}", e))?;
    drop(data);

    if let Some(name) = project_name_override {
        upload.project_name = name;
    }
    let project_name = upload.project_name.clone();
    let root_path = upload.root_path.clone();
    let all_symbols = std::mem::take(&mut upload.symbols);
    let all_objects = std::mem::take(&mut upload.objects);

    let project_id = upload_project_core(
        client,
        base_url,
        token,
        project_name,
        root_path,
        all_symbols,
        all_objects,
        Vec::new(), // no content files for single-file upload
        json,
        window,
        force,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "project_id": project_id }))?
        );
    } else {
        println!("Uploaded index; project id: {}", project_id);
    }
    Ok(())
}

async fn upload_directory(
    client: &reqwest::Client,
    base_url: &str,
    token: &str,
    dir_path: &str,
    project_name_override: Option<String>,
    json: bool,
    window: usize,
    force: bool,
) -> Result<()> {
    let project_pb = format!("{}/project.pb", dir_path);
    if !tokio::fs::try_exists(&project_pb).await.unwrap_or(false) {
        return Err(anyhow!(
            "project.pb not found in directory: {}",
            dir_path
        ));
    }

    // Discover content batch files
    let mut content_file_names: Vec<String> = Vec::new();
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
            content_file_names.push(name);
        }
    }
    content_file_names.sort();
    let content_file_paths: Vec<String> = content_file_names
        .iter()
        .map(|n| format!("{}/{}", dir_path, n))
        .collect();

    // Parse project.pb
    let data = tokio::fs::read(&project_pb)
        .await
        .map_err(|e| anyhow!("Failed to read {}: {}", project_pb, e))?;
    let mut upload = Project::decode(data.as_slice())
        .map_err(|e| anyhow!("Failed to decode protobuf: {}", e))?;
    drop(data);

    if let Some(name) = project_name_override {
        upload.project_name = name;
    }
    let project_name = upload.project_name.clone();
    let root_path = upload.root_path.clone();
    let all_symbols = std::mem::take(&mut upload.symbols);
    let all_objects = std::mem::take(&mut upload.objects);

    let project_id = upload_project_core(
        client,
        base_url,
        token,
        project_name,
        root_path,
        all_symbols,
        all_objects,
        content_file_paths,
        json,
        window,
        force,
    )
    .await?;

    if json {
        println!(
            "{}",
            serde_json::to_string_pretty(&serde_json::json!({ "project_id": project_id }))?
        );
    } else {
        println!("Uploaded index; project id: {}", project_id);
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
    fn endpoints_project_symbols() {
        assert_eq!(
            Endpoints::new("http://api.example.com").project_symbols(7),
            "http://api.example.com/v1/index/projects/7/symbols"
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
    fn endpoints_contents_check() {
        assert_eq!(
            Endpoints::new("http://api.example.com").contents_check(),
            "http://api.example.com/v1/index/contents/check"
        );
    }

    #[test]
    fn endpoints_no_double_slash_when_base_has_no_trailing_slash() {
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
            window,
            force,
        } => {
            let token = resolve_token(token)?;
            let base_url = normalize_base_url(&url);
            let client = build_client(timeout);

            let path = std::path::Path::new(&index);
            if path.is_dir() {
                upload_directory(&client, &base_url, &token, &index, project, json, window, force)
                    .await?;
            } else if path.is_file() {
                upload_single_file(&client, &base_url, &token, &index, project, json, window, force)
                    .await?;
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
                        println!(
                            "{} {} [{}]",
                            project.id, project.project_name, project.upload_status
                        );
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

            let (project_id, _) =
                resolve_project_id(&client, &base_url, &token, selector).await?;
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
                if let (Some(sym_total), Some(obj_total)) =
                    (details.symbol_chunks_total, details.object_chunks_total)
                {
                    println!(
                        "Symbol chunks: {}/{}",
                        details.committed_symbol_chunks.len(),
                        sym_total
                    );
                    println!(
                        "Object chunks: {}/{}",
                        details.committed_object_chunks.len(),
                        obj_total
                    );
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
