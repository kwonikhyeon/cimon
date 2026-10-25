//! `cimon` — service and admin commands in one executable (design §4.2).

use cimon::{init_tracing, serve};

mod recipe_cli;

use anyhow::{bail, Context, Result};
use cimon_core::{Config, Role};
use clap::{Parser, Subcommand};
use std::path::PathBuf;

#[derive(Parser)]
#[command(name = "cimon", version, about = "Cimon says: build.")]
struct Cli {
    /// cimon.toml. Defaults to $CIMON_CONFIG, then D:\cimon\config\cimon.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    command: Command,
}

#[derive(Subcommand)]
enum Command {
    /// Run the service in the foreground.
    Serve,
    /// User tokens.
    Token {
        #[command(subcommand)]
        action: TokenCmd,
    },
    /// User roles.
    User {
        #[command(subcommand)]
        action: UserCmd,
    },
    #[command(name = "__exec", hide = true)]
    Exec { spec: PathBuf },
    #[command(name = "__sweep", hide = true)]
    Sweep,
    #[command(name = "__account-env", hide = true)]
    AccountEnv { args: Vec<String> },
    /// Recipe repository: check, show, import, export, restore.
    Recipe {
        #[command(subcommand)]
        action: recipe_cli::RecipeCmd,
    },
}

#[derive(Subcommand)]
enum TokenCmd {
    /// Create a user and print its token once.
    Create {
        name: String,
        #[arg(long)]
        admin: bool,
    },
    /// Disable a user's token (and end its review-screen sessions).
    Disable { name: String },
    /// Issue a new token for a user, re-enabling it.
    Rotate { name: String },
    /// List users.
    List,
}

#[derive(Subcommand)]
enum UserCmd {
    /// Set a user's role.
    Role {
        name: String,
        #[arg(value_parser = ["admin", "member"])]
        role: String,
    },
}

pub fn config_path(cli: &Option<PathBuf>) -> PathBuf {
    if let Some(p) = cli {
        return p.clone();
    }
    if let Ok(p) = std::env::var("CIMON_CONFIG") {
        return PathBuf::from(p);
    }
    PathBuf::from(r"D:\cimon\config\cimon.toml")
}

pub fn load_config(cli: &Option<PathBuf>) -> Result<Config> {
    let path = config_path(cli);
    Config::load(&path).with_context(|| format!("loading {}", path.display()))
}

fn main() -> Result<()> {
    let cli = Cli::parse();
    match cli.command {
        Command::Serve => {
            init_tracing();
            let cfg = load_config(&cli.config)?;
            let rt = tokio::runtime::Runtime::new()?;
            rt.block_on(serve::run(cfg, None))
        }
        Command::Token { action } => {
            let cfg = load_config(&cli.config)?;
            let store = cimon_store::Store::open(&cimon_core::Layout::new(&cfg.paths.root).db())?;
            match action {
                TokenCmd::Create { name, admin } => {
                    let token = store.create_user(&name, if admin { Role::Admin } else { Role::Member })?;
                    println!("{token}");
                    eprintln!("user {name} created{} — the token is shown once; hand it over without a chat message", if admin { " (admin)" } else { "" });
                }
                TokenCmd::Disable { name } => {
                    store.disable_user(&name)?;
                    eprintln!("user {name} disabled");
                }
                TokenCmd::Rotate { name } => {
                    let token = store.rotate_token(&name)?;
                    println!("{token}");
                    eprintln!("new token for {name}; the old one no longer works");
                }
                TokenCmd::List => {
                    for (name, role, disabled) in store.list_users()? {
                        println!("{name}\t{role}\t{}", disabled.map(|d| format!("disabled {d}")).unwrap_or_else(|| "active".into()));
                    }
                }
            }
            Ok(())
        }
        Command::Exec { spec } => std::process::exit(cimon_runner::exec::child_exec_main(&spec)),
        Command::Sweep => std::process::exit(cimon_runner::sweep_main()),
        Command::AccountEnv { args } => std::process::exit(cimon_runner::account_env_main(&args)),
        Command::Recipe { action } => {
            let code = recipe_cli::run(action, &cli.config)?;
            std::process::exit(code);
        }
        Command::User { action: UserCmd::Role { name, role } } => {
            let cfg = load_config(&cli.config)?;
            let store = cimon_store::Store::open(&cimon_core::Layout::new(&cfg.paths.root).db())?;
            let role = Role::parse(&role).expect("clap restricts values");
            if role == Role::Member && store.user(&name)?.is_some_and(|u| u.is_admin()) && store.count_admins()? <= 1 {
                bail!("{name} is the last admin — make someone else admin first");
            }
            store.set_role(&name, role)?;
            eprintln!("{name} is now {}", role.as_str());
            Ok(())
        }
    }
}

