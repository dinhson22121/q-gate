use std::process::ExitCode;

use clap::Parser;
use qgate::cli::{Cli, EXIT_ERROR, run};

#[tokio::main]
async fn main() -> ExitCode {
    match run(Cli::parse()).await {
        Ok(code) => code,
        Err(error) => {
            eprintln!("qgate: error: {error:#}");
            ExitCode::from(EXIT_ERROR)
        }
    }
}
