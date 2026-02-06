mod api;
mod args;
mod cli;
mod server;

use clap::Parser;
use anyhow::Error;

use args::{Args, Command};

fn print_error_chain(context: &str, err: &Error) {
    eprintln!("{}: {}", context, err);
    for cause in err.chain().skip(1) {
        eprintln!("  caused by: {}", cause);
    }
}

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Auth(auth_args) => {
            if let Err(err) = cli::run_auth_command(auth_args.port, auth_args.command).await {
                print_error_chain("Failed to create API key", &err);
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Index(index_args) => {
            let command = index_args.command;
            let error_context = command.error_context();
            if let Err(err) = cli::run_index_command(command).await {
                print_error_chain(error_context, &err);
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Serve(serve_args) => server::run(serve_args).await,
    }
}
