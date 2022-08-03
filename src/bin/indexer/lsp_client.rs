use std::fs::{self, File};
use std::io::Write;
use std::marker::PhantomData;
use std::sync::{
    atomic::{AtomicU64, Ordering},
    Arc,
};

use log::debug;

use anyhow::{anyhow, Result};
use lsp_types::{self, NumberOrString};
use serde;
use serde::{Deserialize, Serialize};
use serde_json;
use serde_json::json;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use url::Url;

use lsp_types::notification::Notification as LspNotification;
use lsp_types::notification::{DidCloseTextDocument, DidOpenTextDocument, Exit, Initialized};
use lsp_types::request::{DocumentSymbolRequest, Initialize, Shutdown, WorkspaceSymbol};
use lsp_types::request::{References, Request as LspRequest};

#[derive(Serialize, Deserialize, Debug)]
struct Request<T: LspRequest> {
    jsonrpc: String,
    params: T::Params,
    _action: PhantomData<T>,
}

impl<T: LspRequest> Request<T> {
    fn new(params: T::Params) -> Request<T> {
        Request {
            jsonrpc: "2.0".to_string(),
            params: params,
            _action: PhantomData,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    id: u64,
    jsonrpc: String,
    result: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Notification {
    jsonrpc: String,
    method: String,
    params: serde_json::Value,
}

impl Notification {
    fn new<T: LspNotification>(params: T::Params) -> Notification {
        Notification {
            jsonrpc: "2.0".to_string(),
            method: T::METHOD.to_string(),
            params: serde_json::to_value(params).unwrap(),
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
enum ServerMessage {
    Response(Response),
    Notification(Notification),
}

pub struct LSPClient {
    lsp: tokio::process::Child,
    next_id: Arc<AtomicU64>,
    project_root: String,
}

impl Drop for LSPClient {
    fn drop(&mut self) {
        let _ = self.lsp.kill();
    }
}

impl LSPClient {
    pub fn start(lsp_command: &str, project_root: &str, log: Option<&str>) -> Result<Self> {
        let mut args = lsp_command.split_whitespace();
        let prog = args.next().ok_or(anyhow!("LSP server path not provided"))?;

        let stderr = match log {
            Some("-") => std::process::Stdio::inherit(),
            Some(log_path) => {
                let f = File::create(log_path)?;
                std::process::Stdio::from(f)
            },
            None => std::process::Stdio::null(),
        };

        let lsp = tokio::process::Command::new(prog)
            .args(args)
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(stderr)
            .kill_on_drop(true)
            .spawn()?;

        Ok(Self {
            lsp: lsp,
            next_id: Arc::new(0.into()),
            project_root: project_root.into(),
        })
    }

    fn uri(&self, path: &str) -> Option<Url> {
        Url::from_file_path(&self.project_root)
            .unwrap()
            .join(path)
            .ok()
    }

    #[allow(unused)]
    fn full_path(&mut self, path: &str) -> String {
        format!("{}/{}", self.project_root, path)
    }

    async fn read_message(&mut self) -> Result<String> {
        let mut stdout = self.lsp.stdout.as_mut().unwrap();

        let mut content_length: usize = 0;
        let mut reader = BufReader::new(&mut stdout);
        loop {
            let mut buffer = String::new();
            match reader.read_line(&mut buffer).await {
                Ok(0) => {
                    println!("Done");
                    break;
                }
                Ok(_) => {
                    let kv = buffer.split(':').collect::<Vec<_>>();
                    if let ["Content-Length", val] = kv.as_slice() {
                        content_length = val.trim().parse().unwrap();
                    } else if buffer == "\r\n" {
                        break;
                    }
                }
                Err(_) => {
                    println!("Err");
                    break;
                }
            }
        }

        let mut content = vec![0u8; content_length];
        reader.read_exact(&mut content).await?;
        Ok(String::from_utf8(content)?)
    }

    async fn receive(&mut self) -> Result<Response> {
        loop {
            let content_str = self.read_message().await?;
            match serde_json::from_str(&content_str)? {
                ServerMessage::Response(resp) => return Ok(resp),
                ServerMessage::Notification(notification) => match notification.method.as_str() {
                    lsp_types::notification::PublishDiagnostics::METHOD => (),
                    _ => debug!("received notification: {:#?}", notification),
                },
            }
        }
    }

    async fn request<T: LspRequest>(&mut self, body: Request<T>) -> Result<T::Result> {
        let next_id = self.next_id.fetch_add(1, Ordering::SeqCst);

        let raw_json = json!({
            "jsonrpc": body.jsonrpc,
            "id": next_id,
            "params": body.params,
            "method": T::METHOD,
        })
        .to_string();
        let stdin = self.lsp.stdin.as_mut().unwrap();

        let buffer = new_request_buf(&raw_json)?;
        stdin.write_all(&buffer).await?;

        let res: Response = self.receive().await?;

        assert_eq!(next_id, res.id);

        Ok(T::Result::deserialize(res.result)?)
    }

    pub async fn notify(&mut self, body: Notification) -> Result<()> {
        let raw_json = serde_json::to_string(&body).unwrap();
        let stdin = self.lsp.stdin.as_mut().expect("Failed to get stdin");
        let buffer = new_request_buf(&raw_json)?;

        stdin.write_all(&buffer).await?;

        Ok(())
    }

    pub async fn initialize(&mut self) -> Result<lsp_types::InitializeResult> {
        #[allow(deprecated)]
        self.request(Request::<Initialize>::new(lsp_types::InitializeParams {
            process_id: Some(std::process::id()),
            root_path: None,
            root_uri: self.uri(&self.project_root),
            initialization_options: None,
            capabilities: lsp_types::ClientCapabilities {
                workspace: Some(lsp_types::WorkspaceClientCapabilities {
                    apply_edit: Some(false),
                    ..Default::default()
                }),
                window: None,
                experimental: None,
                text_document: Some(lsp_types::TextDocumentClientCapabilities {
                    document_symbol: Some(lsp_types::DocumentSymbolClientCapabilities {
                        hierarchical_document_symbol_support: Some(true),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                ..Default::default()
            },
            trace: None,
            workspace_folders: None,
            client_info: None,
            locale: None,
        }))
        .await
    }

    pub async fn initialized(&mut self) -> Result<()> {
        self.notify(Notification::new::<Initialized>(
            lsp_types::InitializedParams {},
        ))
        .await
    }

    #[allow(unused)]
    async fn shutdown(&mut self) -> Result<()> {
        let params = Request::<Shutdown>::new(());
        self.request(params).await
    }

    #[allow(unused)]
    async fn exit(&mut self) -> Result<()> {
        self.notify(Notification::new::<Exit>(())).await
    }

    pub async fn open_file(
        &mut self,
        path: &str,
        lang: &str,
    ) -> Result<lsp_types::TextDocumentItem> {
        let contents = fs::read_to_string(path)?;
        let document = lsp_types::TextDocumentItem {
            uri: self
                .uri(path)
                .ok_or(anyhow!("Failed creating dicument path"))?,
            language_id: lang.into(),
            version: 1,
            text: contents,
        };

        let notification =
            Notification::new::<DidOpenTextDocument>(lsp_types::DidOpenTextDocumentParams {
                text_document: document.clone(),
            });
        self.notify(notification).await?;

        Ok(document)
    }

    #[allow(unused)]
    pub async fn close_file(&mut self, document: &lsp_types::TextDocumentItem) -> Result<()> {
        let notification =
            Notification::new::<DidCloseTextDocument>(lsp_types::DidCloseTextDocumentParams {
                text_document: lsp_types::TextDocumentIdentifier {
                    uri: document.uri.clone(),
                },
            });
        self.notify(notification).await?;

        Ok(())
    }

    pub async fn document_symbol(
        &mut self,
        document: &lsp_types::TextDocumentItem,
    ) -> Result<Option<lsp_types::DocumentSymbolResponse>> {
        let params = Request::<DocumentSymbolRequest>::new(lsp_types::DocumentSymbolParams {
            text_document: lsp_types::TextDocumentIdentifier {
                uri: document.uri.clone(),
            },
            work_done_progress_params: lsp_types::WorkDoneProgressParams {
                work_done_token: Some(NumberOrString::String(document.uri.clone().into())),
                ..Default::default()
            },
            partial_result_params: lsp_types::PartialResultParams {
                partial_result_token: Some(NumberOrString::String(document.uri.clone().into())),
                ..Default::default()
            },
        });
        self.request(params).await
    }

    pub async fn find_references(
        &mut self,
        path: &lsp_types::Url,
        position: lsp_types::Position,
    ) -> Result<Vec<lsp_types::Location>> {
        let params = Request::<References>::new(lsp_types::ReferenceParams {
            text_document_position: lsp_types::TextDocumentPositionParams {
                text_document: lsp_types::TextDocumentIdentifier { uri: path.clone() },
                position: position,
            },
            context: lsp_types::ReferenceContext {
                include_declaration: false,
            },
            work_done_progress_params: lsp_types::WorkDoneProgressParams {
                work_done_token: Some(NumberOrString::String(path.to_string())),
                ..Default::default()
            },
            partial_result_params: lsp_types::PartialResultParams {
                partial_result_token: Some(NumberOrString::String(path.to_string())),
                ..Default::default()
            },
        });

        match self.request(params).await {
            Ok(None) => Err(anyhow!("No result has been found")),
            Ok(Some(v)) => Ok(v),
            Err(e) => Err(e),
        }
    }

    #[allow(unused)]
    pub async fn workspace_symbols(
        &mut self,
        query: &str,
    ) -> Result<Option<Vec<lsp_types::SymbolInformation>>> {
        let params = Request::<WorkspaceSymbol>::new(lsp_types::WorkspaceSymbolParams {
            query: query.into(),
            ..Default::default()
        });
        self.request(params).await
    }
}

fn new_request_buf(request: &str) -> std::io::Result<Vec<u8>> {
    let mut buffer: Vec<u8> = Vec::new();
    write!(
        &mut buffer,
        "Content-Length: {}\r\n\r\n{}",
        request.len(),
        request
    )?;
    Ok(buffer)
}
