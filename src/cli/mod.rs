mod agents;
mod args;
mod caching;
mod commands;
mod doctor;
mod login;
mod pipeline;
mod policy;
mod render;
#[cfg(unix)]
mod runner;
mod serve;
mod tickets;

use std::process::ExitCode;
use std::sync::Arc;

pub use args::Cli;
use args::Command;
use pipeline::AppContext;

pub const EXIT_OK: u8 = 0;
pub const EXIT_GATE_FAILED: u8 = 1;
pub const EXIT_ERROR: u8 = 2;

pub async fn run(cli: Cli) -> anyhow::Result<ExitCode> {
    if let Command::Init { repo } = &cli.command {
        return commands::init(repo).await;
    }
    if let Command::Runner(args) = &cli.command {
        #[cfg(unix)]
        return runner::runner(args).await;
        #[cfg(not(unix))]
        anyhow::bail!("qgate runner needs Unix sockets (socket {})", args.socket.display());
    }
    let context = Arc::new(AppContext::load()?);
    match cli.command {
        Command::Serve => serve::serve(context).await.map(|()| ExitCode::from(EXIT_OK)),
        Command::Review(args) => commands::review(context, args).await,
        Command::Doctor => Ok(doctor::doctor(&context).await),
        Command::Repos { action } => commands::repos(context, action).await,
        Command::Auth { action } => commands::auth(&context, action),
        Command::LoginUrl => login::print_login_url(&context),
        Command::Ticket { key } => commands::fetch_ticket(context, key).await,
        Command::Init { .. } | Command::Runner(_) => unreachable!("handled above"),
    }
}
