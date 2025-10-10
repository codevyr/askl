use anyhow::bail;
use clap::Parser;
use sqlx::sqlite::{SqliteConnectOptions, SqlitePool};
use std::{fs, path::Path};

/// Indexer for askl
#[derive(Parser, Debug, Clone)]
#[clap(author, version, about, long_about = None)]
struct Args {
    /// Force recreation of an index
    #[clap(short, long, action)]
    force: bool,

    /// Output file to store the resulting symbol map
    #[clap(short, long, default_value = "askli.db")]
    output: String,
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    env_logger::init();
    let args = Args::parse();

    let output = Path::new(&args.output);
    if args.force && output.exists() {
        // Delete old database
        fs::remove_file(output)?
    } else if output.exists() {
        bail!("File exists");
    }

    let options = SqliteConnectOptions::new()
        .filename(&args.output)
        .create_if_missing(true);

    let pool = SqlitePool::connect_with(options).await?;

    let sql = include_str!("../sql/create_tables.sql");
    let res = sqlx::query(sql).execute(&pool).await?;

    println!("Finished {:?}", res);

    Ok(())
}
