use std::fs;
use std::str;
use std::process::{Command, Child, Stdio};
use std::io::{BufReader, BufRead, Read, Write};
use std::marker::PhantomData;
use anyhow::{Result, anyhow};
use log::error;
use log::warn;
use lsp_types::notification::DidCloseTextDocument;
use lsp_types::request::WorkspaceSymbolRequest;

use log;
use log::{debug, trace};

use lsp_types::*;
use serde::{Serialize, Deserialize};
use serde_json::json;
use serde_json;

use lsp_types::notification::{DidOpenTextDocument, Initialized, Exit};
use lsp_types::notification::Notification as LspNotification;

use lsp_types::request::{Initialize, Shutdown, DocumentSymbolRequest};
use lsp_types::request::Request as LspRequest;

pub trait LanguageServer : Send {
    fn initialize(&mut self) -> Result<InitializeResult>;
    fn initialized(&mut self) -> Result<()>;
    fn shutdown(&mut self) -> Result<()>;
    fn exit(&mut self) -> Result<()>;
    fn document_open(&mut self, path: &str) -> Result<Url>;
    fn document_close(&mut self, uri: &Url) -> Result<()>;
    fn document_symbol(&mut self, document: &TextDocumentItem) -> Result<Option<DocumentSymbolResponse>>;

    fn workspace_symbols(&mut self, query: &str) -> Result<Vec<WorkspaceSymbol>>;
}

#[derive(Serialize, Deserialize, Debug)]
struct Request<T: LspRequest> {
    id: u32,
    jsonrpc: String,
    params: T::Params,
    _action: PhantomData<T>,
}

impl<T: LspRequest> Request<T> {
    fn new(params: T::Params) -> Request<T> {
        Request {
            jsonrpc: "2.0".to_string(),
            id: 0,
            params: params,
            _action: PhantomData,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct Response {
    id: u32,
    jsonrpc: String,
    result: serde_json::Value,
}

#[derive(Serialize, Deserialize, Debug)]
struct Notification {
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

pub struct ClangdLanguageServer {
    cmd: Child,
    next_id: u32,
    project: String,
    lang: String,
}

impl ClangdLanguageServer {
    fn new(launcher: LanguageServerLauncher) -> Result<Box<dyn LanguageServer>> {
        let args = ClangdLanguageServer::compose_args(launcher.project_path.clone());
        let cmd = Command::new(launcher.server_path)
                .args(args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::inherit())
                .spawn()?;

        Ok(Box::new(ClangdLanguageServer{
            cmd: cmd,
            next_id: 0,
            project: launcher.project_path.to_string(),
            lang: "c".to_owned(),
        }))
    }

    fn compose_args(project_path: String) -> Vec<String> {
        vec![
            "--background-index".to_owned(),
            "--limit-results=0".to_owned(),
            "--limit-references=0".to_owned(),
            "--compile_args_from=filesystem".to_owned(),
            "--log=verbose".to_owned(),
            "--sync".to_owned(),
            "--compile-commands-dir".to_owned(),
            project_path,
        ]
    }

    fn languages_supported(languages: Vec<String>) -> bool {
        for lang in languages {
            match lang.as_str() {
                "cc" | "cpp" => (),
                _ => return false,
            }
        }

        true
    }

    fn uri(&mut self, path: &str) -> Url {
        Url::from_file_path(self.project.clone()).unwrap().join(path).unwrap()
    }

    fn full_path(&mut self, path: &str) -> String {
        format!("{}/{}", self.project, path)
    }

    fn read_message(&mut self) -> Result<String> {
        let mut stdout = self.cmd.stdout.as_mut().expect("Failed to get stdout");

        let mut content_length : usize = 0;
        let mut reader = BufReader::new(&mut stdout);
        loop {
            let mut buffer = String::new();
            match reader.read_line(&mut buffer) {
                Ok(0) => {
                    warn!("Done");
                    break;
                },
                Ok(l) => {
                    warn!("Buffer ({}): {}", l, buffer);
                    let kv = buffer.split(':').collect::<Vec<_>>();
                    if let ["Content-Length", val] = kv.as_slice() {
                        content_length = val.trim().parse().unwrap();
                    } else if buffer == "\r\n" {
                        break;
                    }
                },
                Err(_) => {
                    error!("Err");
                    break;
                },
            }
        }

        let mut content = vec![0u8; content_length];
        reader.read_exact(&mut content)?;
        warn!("Content: {}", str::from_utf8(&content).unwrap());
        Ok(String::from_utf8(content)?)
    }

    fn receive(&mut self) -> Result<Response>
    {
        loop {
            let content_str = self.read_message()?;
            match serde_json::from_str(&content_str)? {
                ServerMessage::Response(resp) => return Ok(resp),
                ServerMessage::Notification(notification) => {
                    error!("received notification: {}", notification.method);
                    panic!("Unexpected notification");
                },
            }
        }
    }

    fn request<T: LspRequest>(&mut self, body: Request<T>) -> Result<T::Result>
    {
        let raw_json = json!({
            "jsonrpc": body.jsonrpc,
            "id": body.id,
            "params": body.params,
            "method": T::METHOD,
        }).to_string();
        let stdin = self.cmd.stdin.as_mut().expect("Failed to get stdin");
        let content_length = format!("Content-Length: {}\r\n\r\n", raw_json.len());
        trace!("Writing header: {:#?}", content_length);
        stdin.write(content_length.as_bytes())?;
        trace!("Making a request: {:#?}", raw_json);
        stdin.write(raw_json.as_bytes())?;

        let res: Response = self.receive()?;

        self.next_id = self.next_id + 1;


        Ok(T::Result::deserialize(res.result)?)
    }

    fn notify(&mut self, body: Notification) -> Result<()> {
        let json = serde_json::to_string(&body).unwrap();
        let stdin = self.cmd.stdin.as_mut().expect("Failed to get stdin");
        let content_length = format!("Content-Length: {}\r\n\r\n", json.len());
        stdin.write(content_length.as_bytes())?;
        trace!("Sending notification: {}", json);
        stdin.write(json.as_bytes())?;

        Ok(())
    }
}

impl LanguageServer for ClangdLanguageServer {
    #[allow(deprecated)]
    fn initialize(&mut self) -> Result<InitializeResult> {
        self.request(Request::<Initialize>::new(InitializeParams {
            process_id: Some(std::process::id()),
            root_path: None,
            root_uri: Url::from_file_path(self.project.clone()).ok(),
            initialization_options: None,
            locale: None,
            work_done_progress_params: WorkDoneProgressParams { work_done_token: None },
            capabilities: ClientCapabilities {
                general: Some(GeneralClientCapabilities {
                    ..Default::default()
                }),
                workspace: Some(WorkspaceClientCapabilities {
                    apply_edit: Some(false),
                    ..Default::default()
                }),
                text_document: Some(TextDocumentClientCapabilities {
                    document_symbol: Some(DocumentSymbolClientCapabilities {
                        hierarchical_document_symbol_support: Some(true),
                        symbol_kind: Some(SymbolKindCapability{
                            value_set: Some(vec![
                                SymbolKind::FILE,
                                SymbolKind::MODULE,
                                SymbolKind::NAMESPACE,
                                SymbolKind::PACKAGE,
                                SymbolKind::CLASS,
                                SymbolKind::METHOD,
                                SymbolKind::PROPERTY,
                                SymbolKind::FIELD,
                                SymbolKind::CONSTRUCTOR,
                                SymbolKind::ENUM,
                                SymbolKind::INTERFACE,
                                SymbolKind::FUNCTION,
                                SymbolKind::VARIABLE,
                                SymbolKind::CONSTANT,
                                SymbolKind::STRING,
                                SymbolKind::NUMBER,
                                SymbolKind::BOOLEAN,
                                SymbolKind::ARRAY,
                                SymbolKind::OBJECT,
                                SymbolKind::KEY,
                                SymbolKind::NULL,
                                SymbolKind::ENUM_MEMBER,
                                SymbolKind::STRUCT,
                                SymbolKind::EVENT,
                                SymbolKind::OPERATOR,
                                SymbolKind::TYPE_PARAMETER,
                            ]
                            )
                        }),
                        ..Default::default()
                    }),
                    ..Default::default()
                }),
                window: None,
                experimental: None,
            },
            trace: None,
            workspace_folders: None,
            client_info: None,
        }))
    }

    fn initialized(&mut self) -> Result<()> {
        self.notify(Notification::new::<Initialized>(InitializedParams {}))
    }

    fn shutdown(&mut self) -> Result<()> {
        let params = Request::<Shutdown>::new(());
        self.request(params)
    }

    fn exit(&mut self) -> Result<()> {
        self.notify(Notification::new::<Exit>(()))
    }

    fn document_open(&mut self, path: &str) -> Result<Url> {
        let uri = self.uri(path);
        debug!("Opening document: {:?}", uri);
        let contents = fs::read_to_string(path)?;
        let document = TextDocumentItem {
            uri: uri.clone(),
            language_id: self.lang.clone(),
            version: 1,
            text: contents,
        };

        let notification = Notification::new::<DidOpenTextDocument>(DidOpenTextDocumentParams{
            text_document: document.clone(),
        });
        self.notify(notification)?;

        Ok(uri)
    }

    fn document_close(&mut self, uri: &Url) -> Result<()> {
        let notification = Notification::new::<DidCloseTextDocument>(DidCloseTextDocumentParams{
            text_document: TextDocumentIdentifier{
                uri: uri.clone(),
            }
        });
        self.notify(notification)
    }

    fn document_symbol(&mut self, document: &TextDocumentItem) -> Result<Option<DocumentSymbolResponse>> {
        let params = Request::<DocumentSymbolRequest>::new(DocumentSymbolParams {
            text_document: TextDocumentIdentifier{
                uri: document.uri.clone(),
            },
            work_done_progress_params: Default::default(),
            partial_result_params: Default::default(),
        });
        self.request(params)
    }

    fn workspace_symbols(&mut self, query: &str) -> Result<Vec<WorkspaceSymbol>> {
        // let mut rng = rand::thread_rng();
        // let partial_result_token = rng.gen();
        // let work_done_token = rng.gen();

        loop {
            let params = Request::<WorkspaceSymbolRequest>::new(WorkspaceSymbolParams {
                query: query.to_owned(),
                partial_result_params: Default::default(),
                work_done_progress_params: Default::default(),
                // partial_result_params: PartialResultParams { partial_result_token: Some(NumberOrString::Number(partial_result_token)) },
                // work_done_progress_params: WorkDoneProgressParams { work_done_token: Some(NumberOrString::Number(work_done_token))}
            });

            let resp = self.request(params)?.unwrap();
            debug!("Workspace symbols request sent");
            debug!("{:?}", resp);

            // let res: Response = self.receive()?;

            // debug!("{:?}", res);

            match resp {
                WorkspaceSymbolResponse::Flat(symbols) => {
                    if symbols.len() > 0 {
                        debug!("Flat symbols {:?}", symbols);
                        return Ok(vec![]);
                    }
                },
                WorkspaceSymbolResponse::Nested(symbols) => {
                    if symbols.len() > 0 {
                        return Ok(symbols);
                    }
                },
            }
        }
    }
}

impl Drop for ClangdLanguageServer {
    fn drop(&mut self) {
        self.shutdown().expect("Shutdown message failed");
        self.exit().expect("Exit failed");
    }
}

pub struct LanguageServerLauncher {
    server_path: String,
    project_path: String,
    languages: Vec<String>,
}

impl LanguageServerLauncher {
    pub fn new() -> LanguageServerLauncher {
        LanguageServerLauncher{
            server_path: "".to_owned(),
            project_path: "".to_owned(),
            languages: Vec::new(),
        }
    }

    pub fn server(mut self, path: String) -> LanguageServerLauncher {
        self.server_path = path;
        self
    }

    pub fn project(mut self, path: String) -> LanguageServerLauncher {
        self.project_path = path;
        self
    }

    pub fn languages(mut self, languages: Vec<String>) -> LanguageServerLauncher {
        self.languages = languages;
        self
    }

    pub fn launch(self) -> Result<Box<dyn LanguageServer>> {
        if ClangdLanguageServer::languages_supported(self.languages.clone()) {
            ClangdLanguageServer::new(self)
        } else {
            Err(anyhow!("Unsupported languages"))
        }
    }
}