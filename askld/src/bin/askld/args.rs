use clap::{ArgEnum, Args as ClapArgs, Parser, Subcommand};

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

    /// Query timeout in seconds (PG statement_timeout + tokio timeout)
    #[clap(long, default_value = "5", env = "ASKL_QUERY_TIMEOUT")]
    pub query_timeout: u64,

    /// In-RAM SQL result cache budget in bytes (0 disables the cache)
    #[clap(long, default_value = "268435456", env = "ASKL_SQL_CACHE_BYTES")]
    pub sql_cache_bytes: usize,

    /// Max distinct symbols per query result (0 = unlimited); per-request `?limit=` overrides
    #[clap(long, default_value = "100", env = "ASKL_MAX_RESULT_SYMBOLS")]
    pub max_result_symbols: usize,

    /// Cardinality-probe cap: statements whose probe returns at most this
    /// many instance ids are resolved exactly and read by id
    #[clap(long, default_value = "1000", env = "ASKL_PROBE_CAP")]
    pub probe_cap: usize,

    /// Boot-time planner-statistics refresh.
    ///
    /// `force` (the default) always runs ANALYZE — a couple of seconds, and
    /// the only setting that covers statistics which are the right size and
    /// the wrong distribution, the failure that motivated this.  `auto` only
    /// refreshes tables whose catalog entries look implausible; `off`
    /// disables the pass.  Never fatal in any mode.
    #[clap(long, arg_enum, default_value = "force", env = "ASKL_BOOT_ANALYZE")]
    pub boot_analyze: BootAnalyzeArg,

    /// Wall-clock budget for the boot-time refresh, in seconds (0 = no bound).
    /// On expiry the server logs and starts anyway: a maintenance pass must
    /// never turn into a health-check restart loop.
    ///
    /// ANALYZE samples a fixed 30k rows per table, so this is not a function
    /// of corpus size -- but on a cold cache those samples are 30k random
    /// reads per table, which is tens of seconds for a large deployment.  The
    /// budget is generous because expiring it means the boot pass silently
    /// stops happening on exactly the deployments that need it most.
    #[clap(long, default_value = "300", env = "ASKL_BOOT_ANALYZE_TIMEOUT")]
    pub boot_analyze_timeout: u64,
}

/// CLI mirror of [`index::db_diesel::BootAnalyze`].  Kept here so the `index`
/// crate does not grow a clap dependency for one enum.
#[derive(ArgEnum, Debug, Clone, Copy, PartialEq, Eq)]
pub enum BootAnalyzeArg {
    Off,
    Auto,
    Force,
}

impl From<BootAnalyzeArg> for index::db_diesel::BootAnalyze {
    fn from(arg: BootAnalyzeArg) -> Self {
        match arg {
            BootAnalyzeArg::Off => index::db_diesel::BootAnalyze::Off,
            BootAnalyzeArg::Auto => index::db_diesel::BootAnalyze::Auto,
            BootAnalyzeArg::Force => index::db_diesel::BootAnalyze::Force,
        }
    }
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
        /// Path to index: a file (single Project .pb) or directory (multi-file output)
        index: String,
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
        #[clap(long, default_value = "180")]
        timeout: u64,
        /// Print JSON response only
        #[clap(long, action)]
        json: bool,
        /// Max concurrent in-flight chunk uploads (1 = sequential)
        #[clap(long, default_value = "1")]
        window: usize,
        /// Delete any existing project with the same name and start fresh
        #[clap(long, action)]
        force: bool,
    },
    ListProjects {
        /// askld base URL
        #[clap(long, default_value = "http://127.0.0.1:80")]
        url: String,
        /// Bearer token (falls back to ASKL_TOKEN)
        #[clap(long)]
        token: Option<String>,
        /// Request timeout in seconds (0 disables timeout)
        #[clap(long, default_value = "180")]
        timeout: u64,
        /// Print JSON response only
        #[clap(long, action)]
        json: bool,
    },
    GetProject {
        /// Project id to fetch
        #[clap(long)]
        id: Option<i32>,
        /// Project name to fetch
        #[clap(long)]
        name: Option<String>,
        /// askld base URL
        #[clap(long, default_value = "http://127.0.0.1:80")]
        url: String,
        /// Bearer token (falls back to ASKL_TOKEN)
        #[clap(long)]
        token: Option<String>,
        /// Request timeout in seconds (0 disables timeout)
        #[clap(long, default_value = "180")]
        timeout: u64,
        /// Print JSON response only
        #[clap(long, action)]
        json: bool,
    },
    DeleteProject {
        /// Project id to delete
        #[clap(long)]
        id: Option<i32>,
        /// Project name to delete
        #[clap(long)]
        name: Option<String>,
        /// askld base URL
        #[clap(long, default_value = "http://127.0.0.1:80")]
        url: String,
        /// Bearer token (falls back to ASKL_TOKEN)
        #[clap(long)]
        token: Option<String>,
        /// Request timeout in seconds (0 disables timeout)
        #[clap(long, default_value = "180")]
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
            IndexCommand::GetProject { .. } => "Failed to get project",
            IndexCommand::DeleteProject { .. } => "Failed to delete project",
        }
    }
}
