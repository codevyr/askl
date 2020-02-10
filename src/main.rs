use std::process::{Command, Child, Stdio};
use std::io::{BufReader, BufRead, Write};

use serde::{Serialize, Deserialize};
use serde_json;

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

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
enum LspRequestParams {
    InitializeParams(InitializeParams),
}

impl LspRequestParams {
    pub fn method(&self) -> String {
        match self {
            LspRequestParams::InitializeParams(_) => "initialize",
        }.to_string()
    }
}

#[derive(Serialize, Deserialize, Debug)]
#[serde(untagged)]
enum LspNotificationParams {
    InitializedParams(InitializedParams),
    ShutdownParams,
    ExitParams,
}

impl LspNotificationParams {
    pub fn method(&self) -> String {
        match self {
            LspNotificationParams::InitializedParams(_) => "initialized",
            LspNotificationParams::ShutdownParams => "shutdown",
            LspNotificationParams::ExitParams => "exit",
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

#[derive(Serialize, Deserialize, Clone, Debug)]
struct LspResponse<T: Clone> {
    id: u32,
    jsonrpc: String,
    result: T,
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

        let resp: LspResponse<InitializeResult> = self.request(params)?;
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

    fn request<'a, T>(&mut self, params: LspRequestParams) -> Result<LspResponse<T>, Error>
        where T: Deserialize<'a> + Clone
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

        let mut de = serde_json::Deserializer::from_reader(reader);
        let resp = LspResponse::<T>::deserialize(&mut de)?;

        self.next_id = self.next_id + 1;
        Ok(resp)
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
    project_path: &'a str,
}

impl<'a> LanguageServerLauncher<'a> {
    fn new() -> LanguageServerLauncher<'a> {
        LanguageServerLauncher{
            server_path: "",
            project_path: "",
        }
    }

    pub fn server(&'a mut self, path: &'a str) -> &'a mut LanguageServerLauncher {
        self.server_path = path;
        self
    }

    pub fn project(&'a mut self, path: &'a str) -> &'a mut LanguageServerLauncher {
        self.project_path = path;
        self
    }

    pub fn launch(&'a self) -> Result<LanguageServer, Error> {
        Ok(LanguageServer {
            cmd: Command::new(self.server_path)
                .stdin(Stdio::piped())
                .stdout(Stdio::piped())
                .spawn()?,
            next_id: 0,
            project: self.project_path.to_string(),
        })
    }
}

fn main() -> Result<(), Error> {
    let mut lang_server = LanguageServerLauncher::new()
        .server("/usr/bin/clangd")
        .project("/home/desertfox/research/projects/ffmk/criu/")
        .launch()
        .expect("Failed to spawn clangd");


    lang_server.initialize()?;
    lang_server.initialized()?;
    lang_server.shutdown()?;
    lang_server.exit()?;

    println!("Hello, world!");

    Ok(())
}
