use std::fs;
use std::str;
use std::process::{Command, Child, Stdio};
use std::io::{BufReader, BufRead, Read, Write};
use std::marker::PhantomData;

use log;
use log::{debug, trace};

use serde::{Serialize, Deserialize};
use serde_json::json;
use serde_json;

use lsp_types::*;
use lsp_types::notification::{DidOpenTextDocument, Initialized, Exit};
use lsp_types::notification::Notification as LspNotification;

use lsp_types::request::{Initialize, Shutdown, DocumentSymbolRequest};
use lsp_types::request::Request as LspRequest;

use crate::Error;

pub trait LanguageServer {
    fn initialize(&mut self) -> Result<InitializeResult, Error>;
    fn initialized(&mut self) -> Result<(), Error>;
    fn shutdown(&mut self) -> Result<(), Error>;
    fn exit(&mut self) -> Result<(), Error>;
    fn document_open(&mut self, path: &str, lang: &str) -> Result<TextDocumentItem, Error>;
    fn document_symbol(&mut self, document: &TextDocumentItem) -> Result<Option<DocumentSymbolResponse>, Error>;
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

#[serde(untagged)]
#[derive(Serialize, Deserialize, Debug)]
enum ServerMessage {
    Response(Response),
    Notification(Notification),
}

pub struct ClangdLanguageServer {
    cmd: Child,
    next_id: u32,
    project: String,
}

impl ClangdLanguageServer {
    fn new(launcher: LanguageServerLauncher) -> Result<Box<dyn LanguageServer>, Error> {
        Ok(Box::new(ClangdLanguageServer{
            cmd: Command::new(launcher.server_path)
                .args(ClangdLanguageServer::compose_args(launcher.project_path.clone()))
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
            next_id: 0,
            project: launcher.project_path.to_string(),
        }))
    }

    fn compose_args(project_path: String) -> Vec<String> {
        vec![
            "--background-index".to_owned(),
            "--compile-commands-dir".to_owned(),
            project_path,
        ]
    }

    fn uri(&mut self, path: &str) -> Url {
        Url::from_file_path(self.project.clone()).unwrap().join(path).unwrap()
    }

    fn full_path(&mut self, path: &str) -> String {
        format!("{}/{}", self.project, path)
    }

    fn read_message(&mut self) -> Result<String, Error> {
        let mut stdout = self.cmd.stdout.as_mut().expect("Failed to get stdout");

        let mut content_length : usize = 0;
        let mut reader = BufReader::new(&mut stdout);
        loop {
            let mut buffer = String::new();
            match reader.read_line(&mut buffer) {
                Ok(0) => {
                    println!("Done");
                    break;
                },
                Ok(_) => {
                    let kv = buffer.split(':').collect::<Vec<_>>();
                    if let ["Content-Length", val] = kv.as_slice() {
                        content_length = val.trim().parse().unwrap();
                    } else if buffer == "\r\n" {
                        break;
                    }
                },
                Err(_) => {
                    println!("Err");
                    break;
                },
            }
        }

        let mut content = vec![0u8; content_length];
        reader.read_exact(&mut content)?;
        Ok(String::from_utf8(content)?)
    }

    fn receive(&mut self) -> Result<Response, Error>
    {
        loop {
            let content_str = self.read_message()?;
            match serde_json::from_str(&content_str)? {
                ServerMessage::Response(resp) => return Ok(resp),
                ServerMessage::Notification(notification) => debug!("received notification: {}", notification.method),
            }
        }
    }

    fn request<T: LspRequest>(&mut self, body: Request<T>) -> Result<T::Result, Error>
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

    fn notify(&mut self, body: Notification) -> Result<(), Error> {
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
    fn initialize(&mut self) -> Result<InitializeResult, Error> {
        self.request(Request::<Initialize>::new(InitializeParams {
            process_id: Some(std::process::id() as u64),
            root_path: None,
            root_uri: Url::from_file_path(self.project.clone()).ok(),
            initialization_options: None,
            capabilities: ClientCapabilities {
                workspace: Some(WorkspaceClientCapabilities {
                    apply_edit: Some(false),
                    ..Default::default()
                }),
                text_document: Some(TextDocumentClientCapabilities {
                    document_symbol: Some(DocumentSymbolCapability {
                        hierarchical_document_symbol_support: Some(true),
                        symbol_kind: Some(SymbolKindCapability{
                            value_set: Some(vec![
                                SymbolKind::File,
                                SymbolKind::Module,
                                SymbolKind::Namespace,
                                SymbolKind::Package,
                                SymbolKind::Class,
                                SymbolKind::Method,
                                SymbolKind::Property,
                                SymbolKind::Field,
                                SymbolKind::Constructor,
                                SymbolKind::Enum,
                                SymbolKind::Interface,
                                SymbolKind::Function,
                                SymbolKind::Variable,
                                SymbolKind::Constant,
                                SymbolKind::String,
                                SymbolKind::Number,
                                SymbolKind::Boolean,
                                SymbolKind::Array,
                                SymbolKind::Object,
                                SymbolKind::Key,
                                SymbolKind::Null,
                                SymbolKind::EnumMember,
                                SymbolKind::Struct,
                                SymbolKind::Event,
                                SymbolKind::Operator,
                                SymbolKind::TypeParameter,
                                SymbolKind::Unknown,
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

    fn initialized(&mut self) -> Result<(), Error> {
        self.notify(Notification::new::<Initialized>(InitializedParams {}))
    }

    fn shutdown(&mut self) -> Result<(), Error> {
        let params = Request::<Shutdown>::new(());
        self.request(params)
    }

    fn exit(&mut self) -> Result<(), Error> {
        self.notify(Notification::new::<Exit>(()))
    }

    fn document_open(&mut self, path: &str, lang: &str) -> Result<TextDocumentItem, Error> {
        let uri = self.uri(path);
        let contents = fs::read_to_string(self.full_path(path))?;
        let document = TextDocumentItem {
            uri: uri,
            language_id: lang.to_string(),
            version: 1,
            text: contents,
        };

        let notification = Notification::new::<DidOpenTextDocument>(DidOpenTextDocumentParams{
            text_document: document.clone(),
        });
        self.notify(notification)?;

        Ok(document)
    }

    fn document_symbol(&mut self, document: &TextDocumentItem) -> Result<Option<DocumentSymbolResponse>, Error> {
        let params = Request::<DocumentSymbolRequest>::new(DocumentSymbolParams {
            text_document: TextDocumentIdentifier{
                uri: document.uri.clone(),
            },
        });
        self.request(params)
    }
}

pub struct LanguageServerLauncher {
    server_path: String,
    project_path: String,
}

impl LanguageServerLauncher {
    pub fn new() -> LanguageServerLauncher {
        LanguageServerLauncher{
            server_path: "".to_owned(),
            project_path: "".to_owned(),
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

    pub fn launch(self) -> Result<Box<dyn LanguageServer>, Error> {
        ClangdLanguageServer::new(self)
    }
}
