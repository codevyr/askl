mod api;
mod args;
mod cli;
mod server;

use clap::Parser;

use args::{Args, Command};

#[actix_web::main]
async fn main() -> std::io::Result<()> {
    let args = Args::parse();

    match args.command {
        Command::Auth(auth_args) => {
            if let Err(err) = cli::run_auth_command(auth_args.port, auth_args.command).await {
                eprintln!("Failed to create API key: {}", err);
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Index(index_args) => {
            let command = index_args.command;
            let error_context = command.error_context();
            if let Err(err) = cli::run_index_command(command).await {
                eprintln!("{}: {}", error_context, err);
                std::process::exit(1);
            }
            Ok(())
        }
        Command::Serve(serve_args) => server::run(serve_args).await,
    }
}
