//! Operations (design §16): `setup`, `doctor`, `drain`, `github-token`.

use anyhow::{bail, Context, Result};
use cimon_core::{Config, Layout};
use std::path::Path;

/// The service runs as this virtual account (no password to manage).
pub const SERVICE_ACCOUNT: &str = r"NT SERVICE\cimon";
pub const SERVICE_NAME: &str = "cimon";

/// One planned or executed setup action.
struct Step {
    what: String,
    run: Box<dyn FnOnce() -> Result<()>>,
}

fn step(what: impl Into<String>, run: impl FnOnce() -> Result<()> + 'static) -> Step {
    Step { what: what.into(), run: Box::new(run) }
}

fn cmd(program: &str, args: &[&str]) -> Result<()> {
    let out = std::process::Command::new(program).args(args).output().with_context(|| format!("running {program}"))?;
    if !out.status.success() {
        bail!("{program} {}: {}{}", args.join(" "), String::from_utf8_lossy(&out.stdout).trim(), String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

/// Creates or updates the local build account with PowerShell's LocalAccounts module.
/// The password goes in on stdin (never on a command line), and "password never
/// expires" is set in the same call — a failure fails the step.
fn ensure_build_user(name: &str, password: &str) -> Result<()> {
    use std::io::Write;
    if !name.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-')) || name.is_empty() || name.len() > 20 {
        bail!("[accounts].build {name:?} must be 1–20 characters of [A-Za-z0-9._-]");
    }
    let script = format!(
        "$ErrorActionPreference = 'Stop'\n\
         $pw = ConvertTo-SecureString ([Console]::In.ReadLine()) -AsPlainText -Force\n\
         if (Get-LocalUser -Name '{name}' -ErrorAction SilentlyContinue) {{\n\
           Set-LocalUser -Name '{name}' -Password $pw -PasswordNeverExpires $true -AccountNeverExpires -UserMayChangePassword $false\n\
         }} else {{\n\
           New-LocalUser -Name '{name}' -Password $pw -PasswordNeverExpires -AccountNeverExpires -UserMayNotChangePassword -Description 'cimon build steps' | Out-Null\n\
         }}"
    );
    let ps = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let mut child = std::process::Command::new(ps)
        .args(["-NoProfile", "-NonInteractive", "-ExecutionPolicy", "Bypass", "-Command", &script])
        .stdin(std::process::Stdio::piped())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::piped())
        .spawn()
        .context("starting powershell")?;
    child.stdin.take().unwrap().write_all(format!("{password}\n").as_bytes())?;
    let out = child.wait_with_output()?;
    if !out.status.success() {
        bail!("creating {name}: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    Ok(())
}

fn new_password() -> String {
    // 32 hex chars plus one of each class Windows' complexity rule wants
    format!("{}Aa1!", &cimon_core::random_secret_hex()[..32])
}

/// `cimon setup` (admin, once): directories and ACLs, the build account and its stored
/// password, account rights, link key, the Windows service, bootstrap tools.
pub fn setup(config_path: &Path, cfg: &Config, dry_run: bool) -> Result<()> {
    let layout = Layout::new(&cfg.paths.root);
    let build = cfg.accounts.build.clone();
    let mut steps: Vec<Step> = vec![];
    for d in layout.all_dirs() {
        let d2 = d.clone();
        steps.push(step(format!("create {}", d.display()), move || Ok(std::fs::create_dir_all(&d2)?)));
    }
    // the service first: NT SERVICE\cimon only resolves once the service exists, and
    // the rights and ACL steps below name it. The service runs from System32, so both
    // paths in its command line are absolute.
    let exe = std::path::absolute(std::env::current_exe()?)?;
    let cfgp = std::path::absolute(config_path)?;
    steps.push(step(format!("Windows service {SERVICE_NAME} as {SERVICE_ACCOUNT}, automatic start, restart on failure"), move || {
        let bin = format!("\"{}\" service --config \"{}\"", exe.display(), cfgp.display());
        let exists = std::process::Command::new("sc.exe").args(["query", SERVICE_NAME]).output().map(|o| o.status.success()).unwrap_or(false);
        if exists {
            cmd("sc.exe", &["config", SERVICE_NAME, "binPath=", &bin, "obj=", SERVICE_ACCOUNT, "start=", "auto"])?;
        } else {
            cmd("sc.exe", &["create", SERVICE_NAME, "binPath=", &bin, "obj=", SERVICE_ACCOUNT, "start=", "auto", "DisplayName=", "cimon build service"])?;
        }
        cmd("sc.exe", &["failure", SERVICE_NAME, "reset=", "86400", "actions=", "restart/10000/restart/60000/restart/300000"])?;
        cmd("sc.exe", &["description", SERVICE_NAME, "Cimon says: build. Remote build and test service (MCP)."])?;
        Ok(())
    }));
    let key = layout.link_key();
    steps.push(step(format!("link key {}", key.display()), move || {
        if !key.exists() {
            std::fs::write(&key, cimon_core::random_secret_hex())?;
        }
        Ok(())
    }));
    if !cfg.accounts.single_account() {
        let pwfile = layout.config_dir().join("build-account.dpapi");
        let b = build.clone();
        steps.push(step(format!("build account {build} (local user, password never expires, stored DPAPI-encrypted in {})", pwfile.display()), move || {
            let pw = new_password();
            ensure_build_user(&b, &pw)?;
            #[cfg(windows)]
            {
                let blob = cimon_runner::win::protect(pw.as_bytes())?;
                std::fs::write(&pwfile, blob)?;
            }
            Ok(())
        }));
        let b = build.clone();
        steps.push(step(format!("rights: {build} — log on as a batch job"), move || crate::lsa::add_rights(&b, &["SeBatchLogonRight"])));
        // the User Profile Service creates a profile only for an administrator's call;
        // the service later loads the existing profile's hive itself
        #[cfg(windows)]
        {
            let b = build.clone();
            let pwfile = layout.config_dir().join("build-account.dpapi");
            let helper = std::env::current_exe()?;
            steps.push(step(format!("profile of {build} (created by this administrator run)"), move || {
                cimon_runner::BuildAccount::logon(&b, &pwfile, &helper).map(drop).map_err(|e| anyhow::anyhow!(e))
            }));
        }
    }
    // backup/restore: LoadUserProfile requires them in the caller (the build account's
    // HKCU). They let the service read and write any file — it is the trusted part of
    // the machine already (it holds the build account's password); design §15.1.
    steps.push(step(
        format!("rights: {SERVICE_ACCOUNT} — log on as a service, replace a process level token, adjust memory quotas, back up and restore files"),
        || crate::lsa::add_rights(SERVICE_ACCOUNT, &["SeServiceLogonRight", "SeAssignPrimaryTokenPrivilege", "SeIncreaseQuotaPrivilege", "SeBackupPrivilege", "SeRestorePrivilege"]),
    ));
    // ACLs (design §5): the service owns everything; the build account only where it must
    let root = layout.root.clone();
    let b = build.clone();
    let single = cfg.accounts.single_account();
    steps.push(step(format!("ACLs under {}", root.display()), move || {
        let r = root.to_string_lossy().to_string();
        cmd("icacls", &[&r, "/inheritance:r", "/grant:r", "*S-1-5-32-544:(OI)(CI)F", "/grant:r", "*S-1-5-18:(OI)(CI)F", "/grant:r", &format!("{SERVICE_ACCOUNT}:(OI)(CI)F")])?;
        if !single {
            let layout = Layout::new(&root);
            // not logs\ (the runner grants each step's out\ dir) and not toolchains\_staging
            // (a script installer's dir is granted for the install only): a step must not
            // rewrite another job's logs or a download between its check and extraction
            for d in [layout.ws(), layout.root.join("caches"), layout.root.join("homes"), layout.root.join("tmp")] {
                cmd("icacls", &[&d.to_string_lossy(), "/grant", &format!("{b}:(OI)(CI)M")])?;
            }
            // an older setup granted these; take the grant back on upgrade
            for d in [layout.logs(), layout.toolchain_staging()] {
                cmd("icacls", &[&d.to_string_lossy(), "/remove:g", &b])?;
            }
            for d in [layout.mirrors(), layout.toolchains(), layout.recipe_checkouts(), layout.artifacts()] {
                cmd("icacls", &[&d.to_string_lossy(), "/grant", &format!("{b}:(OI)(CI)RX")])?;
            }
            // state, config, recipes.git, backup: no grant — the build account cannot read them
        }
        Ok(())
    }));
    let boot = layout.bootstrap();
    let (rv, uv) = (cfg.bootstrap.rustup.clone(), cfg.bootstrap.uv.clone());
    steps.push(step(
        format!("bootstrap rustup-init {} and uv {} into {}", rv.as_deref().unwrap_or("<unset>"), uv.as_deref().unwrap_or("<unset>"), boot.display()),
        move || bootstrap(&boot, rv.as_deref(), uv.as_deref()),
    ));
    println!("{} setup step(s){}:", steps.len(), if dry_run { " (dry run — nothing is changed)" } else { "" });
    let mut failed = 0;
    for s in steps {
        if dry_run {
            println!("  · {}", s.what);
            continue;
        }
        match (s.run)() {
            Ok(()) => println!("  ✓ {}", s.what),
            Err(e) => {
                failed += 1;
                println!("  ✗ {} — {e}", s.what);
            }
        }
    }
    println!(
        "\nManual steps (docs/setup.md):\n  - tailscale up --unattended=true --advertise-tags=tag:cimon ; tailscale serve --bg --https=443 http://127.0.0.1:8770\n  - restrict the build account's outbound traffic (Windows Defender Firewall / WFP) to the registries in the project notes\n  - store the GitHub read token: cimon github-token (reads one line from stdin)\n  - first admin: cimon token create <name> --admin ; recipes: cimon recipe import --init <dir> --author <name>\n  - then: cimon doctor --network ; sc.exe start {SERVICE_NAME}"
    );
    if failed > 0 {
        bail!("{failed} setup step(s) failed");
    }
    Ok(())
}

/// A version pin from `[bootstrap]`: required, and only characters a release name has
/// (it goes into a download URL).
fn bootstrap_version<'a>(v: Option<&'a str>, key: &str, latest_at: &str) -> Result<&'a str> {
    match v.map(str::trim) {
        Some(v) if !v.is_empty() && v.chars().all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '+')) => Ok(v),
        Some(v) if !v.is_empty() => bail!("[bootstrap].{key} = {v:?} is not a version"),
        _ => bail!("[bootstrap].{key} is not set — pin the version to install (the current one is listed at {latest_at})"),
    }
}

fn bootstrap(dir: &Path, rustup: Option<&str>, uv: Option<&str>) -> Result<()> {
    let rustup = bootstrap_version(rustup, "rustup", "https://static.rust-lang.org/rustup/release-stable.toml")?;
    let uv = bootstrap_version(uv, "uv", "https://github.com/astral-sh/uv/releases")?;
    std::fs::create_dir_all(dir)?;
    let client = reqwest::blocking::Client::builder().user_agent(concat!("cimon/", env!("CARGO_PKG_VERSION"))).build()?;
    // rustup-init of the pinned version, with its published sha256
    let base = format!("https://static.rust-lang.org/rustup/archive/{rustup}/x86_64-pc-windows-msvc/rustup-init.exe");
    let exe = client.get(&base).send()?.error_for_status()?.bytes()?;
    let sum = client.get(format!("{base}.sha256")).send()?.error_for_status()?.text()?;
    let want = sum.split_whitespace().next().unwrap_or_default();
    if cimon_core::sha256_hex(&exe) != want {
        bail!("rustup-init checksum mismatch");
    }
    let init = dir.join("rustup-init.exe");
    std::fs::write(&init, &exe)?;
    let status = std::process::Command::new(&init)
        .args(["-y", "--no-modify-path", "--default-toolchain", "none", "--profile", "minimal"])
        .env("RUSTUP_HOME", dir.join("rustup"))
        .env("CARGO_HOME", dir.join("cargo"))
        .status()?;
    if !status.success() {
        bail!("rustup-init failed");
    }
    // uv from the pinned GitHub release, verified by the asset digest
    let rel: serde_json::Value = client.get(format!("https://api.github.com/repos/astral-sh/uv/releases/tags/{uv}")).send()?.error_for_status()?.json()?;
    let asset = rel["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == "uv-x86_64-pc-windows-msvc.zip")).context("uv release asset")?;
    let digest = asset["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).context("uv asset has no digest")?;
    let zip = client.get(asset["browser_download_url"].as_str().unwrap()).send()?.error_for_status()?.bytes()?;
    if cimon_core::sha256_hex(&zip) != digest {
        bail!("uv checksum mismatch");
    }
    let zpath = dir.join("uv.zip");
    std::fs::write(&zpath, &zip)?;
    cimon_toolchain::extract(&zpath, &dir.join("uv")).map_err(|e| anyhow::anyhow!(e.message))?;
    Ok(())
}

/// `cimon github-token`: stores the GitHub read token DPAPI-encrypted in `config\`.
pub fn github_token(cfg: &Config) -> Result<()> {
    let mut token = String::new();
    std::io::stdin().read_line(&mut token)?;
    let token = token.trim();
    if token.is_empty() {
        bail!("no token on stdin");
    }
    let layout = Layout::new(&cfg.paths.root);
    std::fs::create_dir_all(layout.config_dir())?;
    let p = layout.config_dir().join("github-token.dpapi");
    #[cfg(windows)]
    std::fs::write(&p, cimon_runner::win::protect(token.as_bytes())?)?;
    #[cfg(not(windows))]
    std::fs::write(&p, token)?;
    println!("stored in {} — readable only through this machine's DPAPI and the config ACL", p.display());
    Ok(())
}

/// `cimon drain`: new jobs are refused; returns once nothing runs (design §16.6).
pub fn drain(cfg: &Config, off: bool, wait_secs: u64) -> Result<()> {
    let layout = Layout::new(&cfg.paths.root);
    if off {
        let _ = std::fs::remove_file(layout.draining_flag());
        println!("draining off");
        return Ok(());
    }
    std::fs::create_dir_all(layout.state_dir())?;
    std::fs::write(layout.draining_flag(), cimon_core::now())?;
    println!("draining — new jobs are refused (draining)");
    let store = cimon_store::Store::open(&layout.db())?;
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(wait_secs);
    loop {
        use cimon_core::JobState::*;
        let running = store.jobs_in_states(&[Running, Collecting, Cancelling])?;
        if running.is_empty() {
            println!("nothing is running — safe to stop the service, replace cimon.exe and start it again");
            return Ok(());
        }
        if std::time::Instant::now() >= deadline {
            bail!("still running after {wait_secs}s: {}", running.iter().map(|j| cimon_core::job_label(j.id)).collect::<Vec<_>>().join(", "));
        }
        std::thread::sleep(std::time::Duration::from_secs(5));
    }
}

/// `cimon doctor` (design §16.4). Returns the number of failed checks.
pub fn doctor(cfg: &Config, network: bool) -> Result<usize> {
    let layout = Layout::new(&cfg.paths.root);
    let mut failed = 0usize;
    let mut report = |ok: bool, what: &str, detail: String| {
        if !ok {
            failed += 1;
        }
        println!("{} {what}{}", if ok { "OK  " } else { "FAIL" }, if detail.is_empty() { String::new() } else { format!(" — {detail}") });
    };
    for d in layout.all_dirs() {
        if !d.exists() {
            report(false, &format!("directory {}", d.display()), "missing — run cimon setup".into());
        }
    }
    let app = match cimon_engine::App::open(cfg.clone()) {
        Ok(a) => {
            report(true, "job store and recipe store open", String::new());
            a
        }
        Err(e) => {
            report(false, "job store / recipe store", e.to_string());
            return Ok(failed);
        }
    };
    match app.recipes.main_commit() {
        None => report(false, "recipe repository", "empty — cimon recipe import --init".into()),
        Some(main) => {
            report(app.recipes.fsck().is_ok(), "recipe repository git fsck", String::new());
            match app.recipes.tree(&main) {
                Ok(t) => {
                    let r = cimon_recipe::check_tree(&*t);
                    report(r.ok(), &format!("recipes main {} static check", &main[..7]), format!("{} error(s), {} warning(s)", r.errors.len(), r.warnings.len()));
                    // base toolchains every project declares
                    let cat = cimon_recipe::Catalog::parse(cimon_recipe::Tree::read_str(&*t, "toolchains.toml").unwrap_or("")).ok();
                    for (id, res) in &r.resolved {
                        for (kind, req) in &res.toolchains {
                            if let Some(def) = cat.as_ref().and_then(|c| c.kinds.get(kind)).filter(|d| d.is_base()) {
                                match cimon_toolchain::base::check(&cfg.base, kind, def, req) {
                                    Ok(f) => report(true, &format!("base {kind} ({id})"), f.version),
                                    Err(e) => report(false, &format!("base {kind} ({id})"), format!("{e}{}", def.hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default())),
                                }
                            }
                        }
                        if network {
                            let auth = cimon_engine::git_auth(&app);
                            let mut c = std::process::Command::new("git");
                            c.args(["ls-remote", "--heads", &res.project.repo]).env("GIT_TERMINAL_PROMPT", "0");
                            for (k, v) in &auth {
                                c.env(k, v);
                            }
                            let ok = c.output().map(|o| o.status.success()).unwrap_or(false);
                            report(ok, &format!("GitHub access to {id}"), res.project.repo.clone());
                        }
                    }
                }
                Err(e) => report(false, "recipes main", e.to_string()),
            }
        }
    }
    for w in cimon_engine::backup::warnings(&app) {
        report(false, "backup", w);
    }
    if let Some(free) = cimon_runner::free_bytes(&layout.root) {
        let gb = free / (1 << 30);
        report(gb >= cfg.disk.min_free_gb, "disk free", format!("{gb} GB (min {})", cfg.disk.min_free_gb));
    }
    if cfg.tailscale.require_funnel_off {
        match cimon::tailscale::ensure_funnel_off() {
            Ok(()) => report(true, "Tailscale Funnel off", String::new()),
            Err(e) => report(false, "Tailscale", e.to_string()),
        }
    }
    if !cfg.accounts.single_account() {
        #[cfg(windows)]
        {
            let helper = std::env::current_exe()?;
            match cimon_runner::BuildAccount::logon(&cfg.accounts.build, &layout.config_dir().join("build-account.dpapi"), &helper) {
                Ok(a) => report(true, &format!("build account {} logs on", cfg.accounts.build), a.profile_dir.display().to_string()),
                Err(e) => report(false, &format!("build account {}", cfg.accounts.build), e),
            }
        }
    } else {
        report(false, "accounts", "single-account mode ([accounts].build = \"\") — build steps can write the job store; development only".into());
    }
    let svc = std::process::Command::new("sc.exe").args(["query", SERVICE_NAME]).output().map(|o| o.status.success()).unwrap_or(false);
    report(svc, &format!("Windows service {SERVICE_NAME} registered"), String::new());
    if layout.draining_flag().exists() {
        report(false, "draining", "new jobs are refused — cimon drain --off".into());
    }
    println!("{failed} check(s) failed");
    Ok(failed)
}
