use std::fmt;
use std::fs;
use std::process::{Command, Child, Stdio};
use std::io::{BufReader, BufRead, Write};

use serde::{Serialize, Deserialize};
use serde_json;

#[derive(Debug)]
struct LspError(String);

impl fmt::Display for LspError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "LSP error: {}", self.0)
    }
}

impl std::error::Error for LspError {}

type Error = Box<dyn std::error::Error>;
type DocumentUri = String;

#[derive(Serialize, Deserialize, Debug)]
struct WorkspaceClientCapabilities {
}

#[derive(Serialize, Deserialize, Debug)]
struct TextDocumentClientCapabilities {
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug)]
struct ClientCapabilities {
    workspace: WorkspaceClientCapabilities,
    textDocument: TextDocumentClientCapabilities,
    experimental: Option<String>,
}

#[derive(Serialize, Deserialize, Debug)]
struct WorkspaceFolder {
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug)]
struct InitializeParams {
    processId: u32,
    rootPath: Option<String>,
    rootUri: DocumentUri,
    initializationOptions: Option<String>,
    capabilities: ClientCapabilities,
    trace: String,
    workspaceFolders: Option<Vec<WorkspaceFolder>>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct SaveOptions {
    includeText: Option<bool>,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct TextDocumentSyncOptions {
    openClose: Option<bool>,
    change: Option<u32>,
    willSave: Option<bool>,
    willSaveWaitUntil: Option<bool>,
    save: SaveOptions,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
#[serde(untagged)]
enum TextDocumentSyncOptionsType {
    Struct(TextDocumentSyncOptions),
    Number(u32)
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct ServerCapabilities {
    textDocumentSync: TextDocumentSyncOptionsType,
    hoverProvider: bool,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct InitializeResult {
    capabilities: ServerCapabilities,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct InitializedParams {
}

type LanguageIdString = String;

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct TextDocumentItem {
    uri: DocumentUri,
    languageId: LanguageIdString,
    version: u32,
    text: String,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct DidOpenTextDocumentParams {
    textDocument: TextDocumentItem,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct TextDocumentIdentifier {
    uri: DocumentUri,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct DocumentSymbolParams {
    textDocument: TextDocumentIdentifier,
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct DocumentSymbol {
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
struct SymbolInformation {
}

#[allow(non_snake_case)]
#[derive(Serialize, Deserialize, Debug, Clone)]
enum DocumentSymbolResult {
    DocumentSymbol(Vec<DocumentSymbol>),
    SymbolInformation(Vec<SymbolInformation>),
    None
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
enum LspRequestParams {
    InitializeParams(InitializeParams),
    DocumentSymbolParams(DocumentSymbolParams),
}

impl LspRequestParams {
    pub fn method(&self) -> String {
        match self {
            LspRequestParams::InitializeParams(_) => "initialize",
            LspRequestParams::DocumentSymbolParams(_) => "textDocument/documentSymbol",
        }.to_string()
    }
}

#[derive(Serialize, Deserialize, Clone, Debug)]
#[serde(untagged)]
enum LspNotificationParams {
    InitializedParams(InitializedParams),
    ShutdownParams,
    ExitParams,
    DidOpenTextDocumentParams(DidOpenTextDocumentParams),
}

impl LspNotificationParams {
    pub fn method(&self) -> String {
        match self {
            LspNotificationParams::InitializedParams(_) => "initialized",
            LspNotificationParams::ShutdownParams => "shutdown",
            LspNotificationParams::ExitParams => "exit",
            LspNotificationParams::DidOpenTextDocumentParams(_) => "textDocument/didOpen",
        }.to_string()
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct LspRequest {
    id: u32,
    jsonrpc: String,
    method: String,
    params: LspRequestParams,
}

#[derive(Serialize, Deserialize, Debug)]
struct ResponseError {
    code: i32,
    message: String,
    data: Option<serde_json::Value>,
}

impl fmt::Display for ResponseError {
    fn fmt(&self, f: &mut fmt::Formatter) -> fmt::Result {
        write!(f, "Response error {}: {} data: {:?}",
               self.code, self.message, self.data)
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct LspResponse {
    id: u32,
    jsonrpc: String,
    result: Option<serde_json::Value>,
    error: Option<ResponseError>,
}

impl LspRequest {
    fn new(id: u32, params: LspRequestParams) -> LspRequest {
        LspRequest {
            jsonrpc: "2.0".to_string(),
            id: id,
            method: params.method(),
            params: params,
        }
    }
}

#[derive(Serialize, Deserialize, Debug)]
struct LspNotification {
    jsonrpc: String,
    method: String,
    params: LspNotificationParams,
}

impl LspNotification {
    fn new(params: LspNotificationParams) -> LspNotification {
        LspNotification {
            jsonrpc: "2.0".to_string(),
            method: params.method(),
            params: params,
        }
    }
}

#[serde(untagged)]
#[derive(Serialize, Deserialize, Debug)]
enum LspServerMessage {
    Response(LspResponse),
    Notification(LspNotification),
}

struct LspDocument {
    item: TextDocumentItem,
}

struct LanguageServer {
    cmd: Child,
    next_id: u32,
    project: String,
}

impl LanguageServer {
    pub fn initialize(&mut self) -> Result<(), Error> {
        let params = LspRequestParams::InitializeParams(InitializeParams {
            processId: std::process::id(),
            rootPath: None,
            rootUri: format!("file://{}", self.project),
            initializationOptions: None,
            capabilities: ClientCapabilities {
                workspace: WorkspaceClientCapabilities {
                },
                textDocument: TextDocumentClientCapabilities {
                },
                experimental: None,
            },
            trace: "off".to_string(),
            workspaceFolders: None,
        });

        let resp: InitializeResult = self.request(params)?;
        println!("{:?}", resp);
        Ok(())
    }

    pub fn initialized(&mut self) -> Result<(), Error> {
        let params = LspNotificationParams::InitializedParams(InitializedParams {});
        self.notification(params)
    }

    pub fn shutdown(&mut self) -> Result<(), Error> {
        let params = LspNotificationParams::ShutdownParams;
        self.notification(params)
    }

    pub fn exit(&mut self) -> Result<(), Error> {
        let params = LspNotificationParams::ExitParams;
        self.notification(params)
    }

    pub fn document_open(&mut self, path: &str, lang: &str) -> Result<LspDocument, Error> {
        let uri = self.uri(path);
        let contents = fs::read_to_string(self.full_path(path))?;
        let document = LspDocument {
            item: TextDocumentItem {
                uri: uri,
                languageId: lang.to_string(),
                version: 1,
                text: contents,
            },
        };

        let params = LspNotificationParams::DidOpenTextDocumentParams(DidOpenTextDocumentParams{
            textDocument: document.item.clone(),
        });
        self.notification(params)?;

        Ok(document)
    }

    pub fn document_symbol(&mut self, document: &LspDocument) -> Result<DocumentSymbolResult, Error> {
        let params = LspRequestParams::DocumentSymbolParams(DocumentSymbolParams {
            textDocument: TextDocumentIdentifier{
                uri: document.item.uri.clone(),
            },
        });
        self.request::<DocumentSymbolResult>(params)
    }

    fn uri(&mut self, path: &str) -> String {
        format!("file://{}/{}", self.project, path)
    }

    fn full_path(&mut self, path: &str) -> String {
        format!("{}/{}", self.project, path)
    }

    fn request<'a, T>(&mut self, params: LspRequestParams) -> Result<T, Error>
        where T: serde::de::DeserializeOwned + Clone
    {
        let body = LspRequest::new(self.next_id, params);

        let json = serde_json::to_string(&body).unwrap();
        let stdin = self.cmd.stdin.as_mut().expect("Failed to get stdin");
        let content_length = format!("Content-Length: {}\r\n\r\n", json.len());
        stdin.write(content_length.as_bytes())?;
        println!(">>{}<<", json);
        stdin.write(json.as_bytes())?;

        let mut stdout = self.cmd.stdout.as_mut().expect("Failed to get stdout");

        let mut reader = BufReader::new(&mut stdout);
        loop {
            let mut buffer = String::new();
            match reader.read_line(&mut buffer) {
                Ok(0) => {
                    println!("Done");
                    break;
                },
                Ok(l) => {
                    if buffer == "\r\n" {
                        println!("Header is over");
                        break;
                    }
                    println!("Read {}: {}", l, buffer);
                },
                Err(_) => {
                    println!("Err");
                    break;
                },
            }
        }

        self.next_id = self.next_id + 1;

        let mut de = serde_json::Deserializer::from_reader(reader);
        let mut msg = LspServerMessage::deserialize(&mut de)?;

        match msg {
            LspServerMessage::Response(r) => {
                let res : T = serde_json::from_value(r.result.unwrap())?;
                Ok(res)
            }
            LspServerMessage::Notification(n) => {
                println!("{:?}", n);
                Err(Box::new(LspError("Not a response".to_string())))
            }
        }
    }

    fn notification(&mut self, params: LspNotificationParams) -> Result<(), Error> {
        let body = LspNotification::new(params);

        let json = serde_json::to_string(&body).unwrap();
        let stdin = self.cmd.stdin.as_mut().expect("Failed to get stdin");
        let content_length = format!("Content-Length: {}\r\n\r\n", json.len());
        stdin.write(content_length.as_bytes())?;
        println!("{}: >>{}<<", content_length, json);
        stdin.write(json.as_bytes())?;

        Ok(())
    }
}

struct LanguageServerLauncher<'a> {
    server_path: &'a str,
    server_args: Vec<String>,
    project_path: &'a str,
}

impl<'a> LanguageServerLauncher<'a> {
    fn new() -> LanguageServerLauncher<'a> {
        LanguageServerLauncher{
            server_path: "",
            server_args: vec!(),
            project_path: "",
        }
    }

    pub fn server(&'a mut self, path: &'a str) -> &'a mut LanguageServerLauncher {
        self.server_path = path;
        self
    }

    pub fn server_args<I>(&'a mut self, args: I) -> &'a mut LanguageServerLauncher 
        where I: IntoIterator<Item = &'a &'a str>
    {
        for arg in args {
            self.server_args.push(arg.to_string());
        }
        self
    }

    pub fn project(&'a mut self, path: &'a str) -> &'a mut LanguageServerLauncher {
        self.project_path = path;
        self
    }

    pub fn launch(&'a self) -> Result<LanguageServer, Error> {
        Ok(LanguageServer {
            cmd: Command::new(self.server_path)
                .args(&self.server_args)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .stderr(Stdio::piped())
                .spawn()?,
            next_id: 0,
            project: self.project_path.to_string(),
        })
    }
}

fn main() -> Result<(), Error> {
    let project_home = "/home/desertfox/research/projects/ffmk/criu/";
    let mut lang_server = LanguageServerLauncher::new()
        .server("/usr/bin/clangd-9")
        .server_args(&["--background-index", "--compile-commands-dir", project_home])
        .project(project_home)
        .launch()
        .expect("Failed to spawn clangd");


    lang_server.initialize()?;
    lang_server.initialized()?;

    let document = lang_server.document_open("criu/cr-restore.c", "cpp")?;
    println!("{:?}", lang_server.document_symbol(&document)?);
    lang_server.shutdown()?;
    lang_server.exit()?;

    println!("Hello, world!");

    Ok(())
}
