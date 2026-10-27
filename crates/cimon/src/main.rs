//! `cimon` — service and admin commands in one executable (design §4.2).

use cimon::{init_tracing, serve};

mod ctl;
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
    /// cimon.toml. Defaults to $CIMON_CONFIG, then the installed service's --config, then
    /// D:\cimon\config\cimon.toml.
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    /// Wait for Enter before exiting — for a window that closes with the command (what
    /// the tray app opens elevated, docs/tray.md §5.1).
    #[arg(long, global = true)]
    pause: bool,
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
    /// A one-time code that connects a team member's PC (design §16.3): they run the
    /// printed one-liner, type the code, and the script stores a new token and registers
    /// cimon with Claude Code. One user per device.
    Invite {
        /// The new user (one per device). Without it, --ask asks for it.
        name: Option<String>,
        #[arg(long)]
        admin: bool,
        /// Ask for the name (and admin or not) on this console — what the tray opens.
        #[arg(long)]
        ask: bool,
        /// Minutes the code stays valid.
        #[arg(long, default_value_t = 10)]
        minutes: u32,
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
        /// Only write the service DACL (docs/tray.md §4.1).
        #[arg(long)]
        service_acl: bool,
        /// Only let this Windows user start, stop and control cimon without UAC (the
        /// cimon-operators group), and write the service DACL.
        #[arg(long)]
        operator: Option<String>,
        /// With --operator: take the user out of the group instead.
        #[arg(long, requires = "operator")]
        remove: bool,
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
    /// A credential (stdin) one project's recipe hands its build steps as
    /// `${secret.<name>}` in an env value, e.g. a package registry token (design §11.6).
    /// Without a name, lists the secrets.
    BuildSecret {
        project: Option<String>,
        name: Option<String>,
        #[arg(long)]
        remove: bool,
    },
    /// Entry point used by the Windows service control manager.
    #[command(hide = true)]
    Service,
    /// Start, stop or control the service (docs/tray.md §4.3).
    Ctl {
        #[command(subcommand)]
        action: ctl::CtlCmd,
    },
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
        /// Ask for the project on this console — what the tray opens.
        #[arg(long)]
        ask: bool,
    },
    /// Open the config file in Notepad, check it when closed, and offer a restart —
    /// the config is read at start only.
    EditConfig,
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
    // an admin on the build machine does not repeat what `cimon setup` put in the
    // service's command line
    if let Some(p) = service_config() {
        return p;
    }
    PathBuf::from(r"D:\cimon\config\cimon.toml")
}

/// The `--config` of the installed service, from its command line in the registry.
#[cfg(windows)]
fn service_config() -> Option<PathBuf> {
    use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_LOCAL_MACHINE, RRF_NOEXPAND, RRF_RT_REG_EXPAND_SZ, RRF_RT_REG_SZ};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    let key = wide(&format!(r"SYSTEM\CurrentControlSet\Services\{}", ops::SERVICE_NAME));
    let value = wide("ImagePath");
    let mut buf = vec![0u16; 4096];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: the key and value names are NUL-terminated; buf holds `bytes` bytes
    let rc = unsafe {
        RegGetValueW(HKEY_LOCAL_MACHINE, key.as_ptr(), value.as_ptr(), RRF_RT_REG_SZ | RRF_RT_REG_EXPAND_SZ | RRF_NOEXPAND, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut bytes)
    };
    if rc != 0 {
        return None;
    }
    let line = String::from_utf16_lossy(&buf[..bytes as usize / 2]);
    config_arg(line.trim_end_matches('\0'))
}

#[cfg(not(windows))]
fn service_config() -> Option<PathBuf> {
    None
}

/// The value of `--config` (or `--config=`) in a service command line (setup quotes
/// it), with `%VAR%` expanded — an ImagePath is REG_EXPAND_SZ, read unexpanded.
fn config_arg(line: &str) -> Option<PathBuf> {
    let rest = match line.split_once(" --config ") {
        Some((_, r)) => r.trim_start(),
        None => line.split_once(" --config=")?.1,
    };
    let p = match rest.strip_prefix('"') {
        Some(q) => q.split('"').next()?,
        None => rest.split_whitespace().next()?,
    };
    let p = expand_env(p);
    (!p.is_empty()).then(|| PathBuf::from(p))
}

/// `%NAME%` → the variable's value; an unset one stays as written.
fn expand_env(s: &str) -> String {
    let mut out = String::new();
    let mut rest = s;
    while let Some(i) = rest.find('%') {
        out.push_str(&rest[..i]);
        let after = &rest[i + 1..];
        match after.find('%').map(|j| (&after[..j], &after[j + 1..])) {
            Some((name, tail)) if !name.is_empty() => {
                match std::env::var(name) {
                    Ok(v) => out.push_str(&v),
                    Err(_) => out.push_str(&format!("%{name}%")),
                }
                rest = tail;
            }
            _ => {
                out.push('%');
                rest = after;
            }
        }
    }
    out.push_str(rest);
    out
}

#[cfg(test)]
#[test]
fn config_from_a_service_command_line() {
    let p = |s: &str| config_arg(s).map(|p| p.to_string_lossy().into_owned());
    assert_eq!(p(r#""C:\Program Files\cimon\cimon.exe" service --config "C:\cimon\config\cimon.toml""#).as_deref(), Some(r"C:\cimon\config\cimon.toml"));
    assert_eq!(p(r#"C:\cimon\cimon.exe service --config D:\c\cimon.toml"#).as_deref(), Some(r"D:\c\cimon.toml"));
    assert_eq!(p(r#""C:\cimon\cimon.exe" service"#), None);
    assert_eq!(p(r#""C:\cimon\cimon.exe" service --config="C:\c d\cimon.toml""#).as_deref(), Some(r"C:\c d\cimon.toml"));
    std::env::set_var("CIMON_TEST_ROOT", r"E:\svc");
    assert_eq!(p(r#""C:\cimon\cimon.exe" service --config "%CIMON_TEST_ROOT%\cimon.toml""#).as_deref(), Some(r"E:\svc\cimon.toml"));
    assert_eq!(p(r#""C:\cimon\cimon.exe" service --config "%NOT_SET_X%\c.toml""#).as_deref(), Some(r"%NOT_SET_X%\c.toml"));
}

pub fn load_config(cli: &Option<PathBuf>) -> Result<Config> {
    let path = config_path(cli);
    Config::load(&path).with_context(|| format!("loading {}", path.display()))
}

fn main() {
    let cli = Cli::parse();
    let pause = cli.pause;
    let code = match run(cli) {
        Ok(code) => code,
        Err(e) => {
            eprintln!("Error: {e:?}");
            1
        }
    };
    if pause {
        ops::pause_for_enter(code);
    }
    std::process::exit(code);
}

fn run(cli: Cli) -> Result<i32> {
    let r: Result<()> = match cli.command {
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
        Command::Invite { name, admin, ask, minutes } => {
            let (name, admin) = match name {
                Some(n) => (n, admin),
                None if ask => (ops::ask("invite whom — one name per device ([A-Za-z0-9._-])")?, admin || ops::ask_yes("as an admin?")?),
                None => bail!("a name — cimon invite <name> (or --ask)"),
            };
            let cfg = load_config(&cli.config)?;
            let store = cimon_store::Store::open(&cimon_core::Layout::new(&cfg.paths.root).db())?;
            let (code, expires) = store.create_invite(&name, if admin { Role::Admin } else { Role::Member }, &whoami(), minutes)?;
            let url = cfg.server.public_url.trim_end_matches('/');
            println!("invite for {name} ({}) — one use, until {expires} ({minutes} min):\n", if admin { "admin" } else { "member" });
            println!("    {code}\n");
            println!("On their PC (Tailscale on, Claude Code 2.1.304+), run and type the code:");
            println!("  Windows (PowerShell):  irm {url}/connect.ps1 | iex");
            println!("  macOS / Linux:         curl -fsSL {url}/connect.sh | sh");
            Ok(())
        }
        Command::Setup { service_acl, operator, remove, .. } if service_acl || operator.is_some() => ops::service_acl(operator, remove),
        Command::Setup { dry_run, .. } => {
            let path = config_path(&cli.config);
            let cfg = load_config(&cli.config)?;
            ops::setup(&path, &cfg, dry_run)
        }
        Command::Doctor { network } => {
            let cfg = load_config(&cli.config)?;
            let failed = ops::doctor(&cfg, network)?;
            return Ok(if failed == 0 { 0 } else { 1 });
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
        Command::BuildSecret { project, name, remove } => {
            let cfg = load_config(&cli.config)?;
            ops::build_secret(&cfg, project, name, remove)
        }
        Command::Service => service::run(cli.config.clone()),
        Command::Provision { project, kind, version, recipe_ref, from, admin, ask } => {
            let project = match project {
                None if ask => Some(ops::ask("project (list_projects id)")?),
                p => p,
            };
            let cfg = load_config(&cli.config)?;
            let app = cimon_engine::App::open(cfg)?;
            let lines = cimon_engine::toolchains::provision(&app, &cimon_engine::toolchains::ProvisionArgs { project, kind, version, recipe_ref, from, admin })
                .map_err(|e| anyhow::anyhow!(e))?;
            let mut failed = false;
            for l in &lines {
                println!("{}	{}	{}", l.kind, l.version, l.outcome);
                failed |= !l.ok;
            }
            if !app.config.accounts.single_account() {
                // starting a process as another account needs "replace a process level
                // token", which `cimon setup` gives the service account and an admin
                // does not hold
                eprintln!(
                    "note: probes and install scripts ran as {} — an admin cannot start processes as the build account. The service probes every toolchain as the build account at each job",
                    whoami()
                );
            }
            return Ok(if failed { 1 } else { 0 });
        }
        Command::Exec { spec } => return Ok(cimon_runner::exec::child_exec_main(&spec)),
        Command::Sweep => return Ok(cimon_runner::sweep_main()),
        Command::AccountEnv { args } => return Ok(cimon_runner::account_env_main(&args)),
        Command::Recipe { action } => {
            let code = recipe_cli::run(action, &cli.config)?;
            return Ok(code);
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
        Command::Ctl { action } => return ctl::run(action),
        Command::EditConfig => ops::edit_config(&config_path(&cli.config)),
    };
    r.map(|()| 0)
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
