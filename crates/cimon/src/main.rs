//! `cimon` — service and admin commands in one executable (design §4.2).

use cimon::{init_tracing, serve};

mod lsa;
mod ops;
mod recipe_cli;
mod service;

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
    /// Admin, once: directories, ACLs, accounts, rights, service, bootstrap tools.
    Setup {
        /// Print what would be done without changing anything.
        #[arg(long)]
        dry_run: bool,
    },
    /// Check the machine (design §16.4); non-zero exit when something fails.
    Doctor {
        /// Also check GitHub access to every project.
        #[arg(long)]
        network: bool,
    },
    /// Refuse new jobs and wait for running ones (before an upgrade).
    Drain {
        #[arg(long)]
        off: bool,
        #[arg(long, default_value_t = 3600)]
        wait: u64,
    },
    /// Store the GitHub read token (read from stdin) DPAPI-encrypted.
    GithubToken,
    /// A token build steps send to one repository (stdin), e.g. for a private cargo git
    /// dependency (design §11.6). Without a URL, lists the URLs that have one.
    BuildGitToken {
        url: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Entry point used by the Windows service control manager.
    #[command(hide = true)]
    Service,
    /// Install toolchains that auto-approve does not (design §10.4).
    Provision {
        /// Every toolchain of this project (including optional ones).
        project: Option<String>,
        #[arg(long)]
        kind: Option<String>,
        #[arg(long)]
        version: Option<String>,
        /// Install from this trial branch's catalog entry, after reviewing it.
        #[arg(long = "recipe-ref")]
        recipe_ref: Option<String>,
        /// Install --kind at --version from this local copy instead of the catalog's
        /// source (when that is down). No catalog checksum is checked — you vouch for the
        /// copy, and the audit log records the path.
        #[arg(long)]
        from: Option<PathBuf>,
        /// Recorded in the audit log.
        #[arg(long, default_value_t = whoami())]
        admin: String,
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

fn whoami() -> String {
    std::env::var("USERNAME").unwrap_or_else(|_| "admin".into())
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
            let _guard = init_service_logging(&cimon_core::Layout::new(&cfg.paths.root).service_logs());
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
        Command::Setup { dry_run } => {
            let path = config_path(&cli.config);
            let cfg = load_config(&cli.config)?;
            ops::setup(&path, &cfg, dry_run)
        }
        Command::Doctor { network } => {
            let cfg = load_config(&cli.config)?;
            let failed = ops::doctor(&cfg, network)?;
            std::process::exit(if failed == 0 { 0 } else { 1 });
        }
        Command::Drain { off, wait } => {
            let cfg = load_config(&cli.config)?;
            ops::drain(&cfg, off, wait)
        }
        Command::GithubToken => {
            let cfg = load_config(&cli.config)?;
            ops::github_token(&cfg)
        }
        Command::BuildGitToken { url, remove } => {
            let cfg = load_config(&cli.config)?;
            ops::build_git_token(&cfg, url, remove)
        }
        Command::Service => service::run(cli.config.clone()),
        Command::Provision { project, kind, version, recipe_ref, from, admin } => {
            let cfg = load_config(&cli.config)?;
            let app = cimon_engine::App::open(cfg)?;
            // install scripts and probes run as the build account, as in the service
            app.logon_build_account()?;
            let lines = cimon_engine::toolchains::provision(&app, &cimon_engine::toolchains::ProvisionArgs { project, kind, version, recipe_ref, from, admin })
                .map_err(|e| anyhow::anyhow!(e))?;
            let mut failed = false;
            for l in &lines {
                println!("{}	{}	{}", l.kind, l.version, l.outcome);
                failed |= !l.ok;
            }
            std::process::exit(if failed { 1 } else { 0 });
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


/// Daily-rolled service log under `logs\cimon\` (design §16.4: kept 30 days).
pub fn init_service_logging(dir: &std::path::Path) -> Option<tracing_appender::non_blocking::WorkerGuard> {
    let _ = std::fs::create_dir_all(dir);
    let appender = tracing_appender::rolling::daily(dir, "cimon.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    use tracing_subscriber::layer::SubscriberExt;
    let subscriber = tracing_subscriber::registry()
        .with(tracing_subscriber::EnvFilter::try_from_default_env().unwrap_or_else(|_| "info,rmcp=warn".into()))
        .with(tracing_subscriber::fmt::layer().with_writer(writer).with_ansi(false))
        .with(tracing_subscriber::fmt::layer().with_writer(std::io::stderr));
    tracing::subscriber::set_global_default(subscriber).ok().map(|_| guard)
}
