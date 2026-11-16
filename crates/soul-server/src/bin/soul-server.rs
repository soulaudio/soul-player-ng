//! `soul-server` CLI: config-driven remote source server + user management.

use std::path::PathBuf;
use std::sync::Arc;

use clap::{Parser, Subcommand};

use soul_server::config;
use soul_server::domain::ports::UserStore;
use soul_server::domain::service::Services;
use soul_server::index::CoreIndex;
use soul_server::media::FsMedia;
use soul_server::proto::native;
use soul_server::store::SqliteUsers;

#[derive(Parser)]
#[command(name = "soul-server", about = "Soul Audio remote source server")]
struct Cli {
    /// Path to the server's TOML config file.
    #[arg(long, global = true, default_value = "soul-server.toml")]
    config: PathBuf,

    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the HTTP server.
    Serve,
    /// Manage server users.
    User {
        #[command(subcommand)]
        command: UserCommand,
    },
}

#[derive(Subcommand)]
enum UserCommand {
    /// Add a new user, prompting for a password unless `--password` is given.
    Add {
        name: String,
        #[arg(long)]
        password: Option<String>,
    },
}

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let cli = Cli::parse();

    match cli.command {
        Command::Serve => run_serve(&cli.config).await,
        Command::User { command } => run_user_command(&cli.config, command),
    }
}

async fn run_serve(config_path: &std::path::Path) -> anyhow::Result<()> {
    let cfg = config::load(config_path)?;
    let index = CoreIndex::scan(cfg.roots.clone())?;
    let media = FsMedia;
    let users = SqliteUsers::open(&cfg.db)?;
    let services = Arc::new(Services {
        index,
        media,
        users,
    });
    native::serve(&cfg.bind, services).await
}

fn run_user_command(config_path: &std::path::Path, command: UserCommand) -> anyhow::Result<()> {
    match command {
        UserCommand::Add { name, password } => {
            let cfg = config::load(config_path)?;
            let store = SqliteUsers::open(&cfg.db)?;
            let password = match password {
                Some(password) => password,
                None => rpassword::prompt_password(format!("Password for {name}: "))?,
            };
            let id = store.add_user(&name, &password)?;
            println!("created user '{name}' (id {id})");
            Ok(())
        }
    }
}
