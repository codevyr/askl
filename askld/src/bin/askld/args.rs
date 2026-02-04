use clap::{Args as ClapArgs, Parser, Subcommand};

/// Indexer for askl
#[derive(Parser, Debug)]
#[clap(author, version, about, long_about = None)]
pub struct Args {
    #[clap(subcommand)]
    pub command: Command,
}

#[derive(Subcommand, Debug)]
pub enum Command {
    Serve(ServeArgs),
    Auth(AuthArgs),
    Index(IndexArgs),
}

#[derive(ClapArgs, Debug)]
pub struct ServeArgs {
    /// Postgres connection string for the auth and index DB
    #[clap(long, env = "ASKL_DATABASE_URL")]
    pub database_url: String,

    /// Port to listen on
    #[clap(short, long, default_value = "80")]
    pub port: u16,

    /// Host to bind to
    #[clap(short = 'H', long, default_value = "127.0.0.1")]
    pub host: String,

    /// Enable tracing. Provide a file path to write the trace to.
    #[clap(short, long, action)]
    pub trace: Option<String>,
}

#[derive(ClapArgs, Debug)]
pub struct AuthArgs {
    /// Port to call on localhost
    #[clap(short, long, default_value = "80")]
    pub port: u16,

    #[clap(subcommand)]
    pub command: AuthCommand,
}

#[derive(Subcommand, Debug)]
pub enum AuthCommand {
    CreateApiKey {
        #[clap(long)]
        email: String,
        #[clap(long)]
        name: Option<String>,
        #[clap(long, action)]
        json: bool,
        /// RFC3339 timestamp, e.g. 2026-01-01T00:00:00Z
        #[clap(long)]
        expires_at: Option<String>,
    },
    RevokeApiKey {
        #[clap(long)]
        token_id: String,
        #[clap(long, action)]
        json: bool,
    },
    ListApiKeys {
        #[clap(long)]
        email: String,
        #[clap(long, action)]
        json: bool,
    },
}

#[derive(ClapArgs, Debug)]
pub struct IndexArgs {
    #[clap(subcommand)]
    pub command: IndexCommand,
}

#[derive(Subcommand, Debug)]
pub enum IndexCommand {
    Upload {
        /// Path to protobuf payload file
        #[clap(long = "file")]
        file_path: String,
        /// askld base URL
        #[clap(long, default_value = "http://127.0.0.1:80")]
        url: String,
        /// Bearer token (falls back to ASKL_TOKEN)
        #[clap(long)]
        token: Option<String>,
        /// Override project name from the protobuf payload
        #[clap(long)]
        project: Option<String>,
        /// Request timeout in seconds (0 disables timeout)
        #[clap(long, default_value = "30")]
        timeout: u64,
        /// Print JSON response only
        #[clap(long, action)]
        json: bool,
    },
    ListProjects {
        /// askld base URL
        #[clap(long, default_value = "http://127.0.0.1:80")]
        url: String,
        /// Bearer token (falls back to ASKL_TOKEN)
        #[clap(long)]
        token: Option<String>,
        /// Request timeout in seconds (0 disables timeout)
        #[clap(long, default_value = "30")]
        timeout: u64,
        /// Print JSON response only
        #[clap(long, action)]
        json: bool,
    },
}

impl IndexCommand {
    pub fn error_context(&self) -> &'static str {
        match self {
            IndexCommand::Upload { .. } => "Failed to upload index",
            IndexCommand::ListProjects { .. } => "Failed to list projects",
        }
    }
}
