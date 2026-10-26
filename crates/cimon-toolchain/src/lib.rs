//! Toolchain installers (design §10.3, recipe-spec §16.2). The service account downloads,
//! verifies checksums and moves files; install scripts and probes run as the build
//! account in `toolchains\_staging\`. Managed tools without a checksum source are never
//! installed.

pub mod base;

use cimon_core::{now, Layout};
use cimon_recipe::catalog::KindDef;
use cimon_recipe::model::ToolchainReq;
use cimon_store::ToolchainInstall;
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::Arc;
use std::time::Duration;

pub struct Ctx {
    pub layout: Layout,
    pub github_api: String,
    pub rustup: Option<PathBuf>,
    pub uv: Option<PathBuf>,
    pub account: Option<Arc<cimon_runner::BuildAccount>>,
    pub mirrors: PathBuf,
    pub installed_by: String,
}

#[derive(Debug, Clone)]
pub struct InstallError {
    pub message: String,
    /// A download/network problem (infra) rather than a bad entry (environment).
    pub network: bool,
}

fn err(m: impl Into<String>) -> InstallError {
    InstallError { message: m.into(), network: false }
}
fn net(m: impl Into<String>) -> InstallError {
    InstallError { message: m.into(), network: true }
}

/// Where a kind's PATH entries and home are, given its install root.
pub struct InstLayout {
    pub bins: Vec<PathBuf>,
    pub home: PathBuf,
}

/// Version key stored for an install: python with packages gets a package hash suffix
/// so different package sets do not share a virtual environment.
pub fn version_key(kind_def: &KindDef, version: &str, req: &ToolchainReq) -> String {
    match (&kind_def.installer[..], &req.packages) {
        ("uv-python", Some(p)) if !p.is_empty() => {
            let mut sorted = p.clone();
            sorted.sort();
            format!("{version}+pkgs.{}", &cimon_core::sha256_hex(sorted.join("\n").as_bytes())[..8])
        }
        _ => version.to_string(),
    }
}

pub fn layout_of(ctx: &Ctx, _kind: &str, def: &KindDef, version: &str, root: &Path, _req: &ToolchainReq) -> InstLayout {
    let sub = |p: &str| root.join(p.replace("{version}", version).replace('/', "\\"));
    match def.installer.as_str() {
        "rustup" => InstLayout { bins: vec![ctx.layout.bootstrap().join("cargo").join("bin")], home: root.to_path_buf() },
        "uv-python" => {
            let venv = root.join("venv");
            if venv.exists() {
                InstLayout { bins: vec![venv.join("Scripts")], home: venv }
            } else {
                // UV_PYTHON_INSTALL_DIR = root\python holds one interpreter directory
                let find = |d: &Path| std::fs::read_dir(d).ok().and_then(|rd| rd.flatten().find(|e| e.path().join("python.exe").exists()).map(|e| e.path()));
                let dir = find(&root.join("python")).or_else(|| find(root)).unwrap_or_else(|| root.to_path_buf());
                InstLayout { bins: vec![dir.clone()], home: dir }
            }
        }
        "uv-tool" => InstLayout { bins: vec![root.join("bin")], home: root.to_path_buf() },
        _ => {
            let bins = if def.bin.is_empty() { vec![root.to_path_buf()] } else { def.bin.iter().map(|b| sub(b)).collect() };
            let home = def.home.as_ref().map(|h| sub(h)).unwrap_or_else(|| root.to_path_buf());
            InstLayout { bins, home }
        }
    }
}

/// Installs one managed kind at one version from `def` (which must be main's entry, or
/// a reviewed trial entry via `cimon provision --recipe-ref`).
pub fn install(ctx: &Ctx, kind: &str, def: &KindDef, entry: &str, version: &str, req: &ToolchainReq, source_ref: &str) -> Result<ToolchainInstall, InstallError> {
    if def.is_base() {
        return Err(err(format!("{kind} is a base kind — an admin installs it")));
    }
    let vkey = version_key(def, version, req);
    if let Some(pkgs) = &req.packages {
        for p in pkgs {
            if !cimon_recipe::check::valid_package_pin(p) {
                return Err(err(format!("package {p:?} is not name==version — refusing to pass it to uv")));
            }
        }
    }
    let final_dir = ctx.layout.toolchain_dir(kind, &vkey, entry);
    let staging = ctx.layout.toolchain_staging().join(format!("{kind}-{}-{}", cimon_core::layout::sanitize(&vkey), &entry[..entry.len().min(8)]));
    let _ = remove_dir(&staging);
    std::fs::create_dir_all(&staging).map_err(|e| err(format!("{}: {e}", staging.display())))?;
    // uv writes absolute paths into interpreters, venvs and tool launchers: those kinds
    // are built where they will live, the rest in staging and moved
    let in_place = matches!(def.installer.as_str(), "uv-python" | "uv-tool");
    let root = if in_place {
        if final_dir.exists() {
            remove_dir(&final_dir).map_err(|e| err(format!("{}: {e}", final_dir.display())))?;
        }
        final_dir.clone()
    } else {
        staging.join("root")
    };
    std::fs::create_dir_all(&root).map_err(|e| err(e.to_string()))?;
    let build_mode = ctx.account.is_some();
    let mut checksum: Option<String> = None;
    let res = (|| -> Result<(), InstallError> {
        match def.installer.as_str() {
            "archive" => {
                let url = fill(def.url.as_deref().ok_or_else(|| err("archive needs url"))?, version);
                let cs = def.checksum.as_ref().ok_or_else(|| err("no checksum source — not installed"))?;
                let file = staging.join("download");
                let got = download(&url, &file)?;
                let want = expected_checksum(cs, version, &url, &ctx.github_api)?;
                verify(&got, &want, &url)?;
                checksum = Some(got);
                extract(&file, &root)?;
            }
            "github-release" => {
                let repo = def.repo.as_deref().ok_or_else(|| err("github-release needs repo"))?;
                let tag = fill(def.tag.as_deref().ok_or_else(|| err("needs tag"))?, version);
                let asset = fill(def.asset.as_deref().ok_or_else(|| err("needs asset"))?, version);
                let (url, digest) = github_asset(&ctx.github_api, repo, &tag, &asset)?;
                let file = staging.join("download");
                let got = download(&url, &file)?;
                verify(&got, &digest, &url)?;
                checksum = Some(got);
                extract(&file, &root)?;
            }
            "rustup" => {
                let rustup = ctx.rustup.clone().ok_or_else(|| err("[bootstrap].rustup is not set — run `cimon setup`"))?;
                let mut args = vec!["toolchain".to_string(), "install".into(), version.into(), "--profile".into(), "minimal".into(), "--no-self-update".into()];
                for c in def.components.iter() {
                    args.push("-c".into());
                    args.push(c.clone());
                }
                let mut env = BTreeMap::new();
                env.insert("RUSTUP_HOME".into(), ctx.layout.bootstrap().join("rustup").to_string_lossy().into());
                env.insert("CARGO_HOME".into(), ctx.layout.bootstrap().join("cargo").to_string_lossy().into());
                // rustup verifies its own downloads; it runs as the service account
                run_service(&rustup, &args, &staging, env, Duration::from_secs(1800))?;
            }
            "uv-python" => {
                let uv = ctx.uv.clone().ok_or_else(|| err("[bootstrap].uv is not set — run `cimon setup`"))?;
                let mut env = BTreeMap::new();
                env.insert("UV_PYTHON_INSTALL_DIR".into(), root.join("python").to_string_lossy().into());
                env.insert("UV_CACHE_DIR".into(), ctx.layout.bootstrap().join("uv-cache").to_string_lossy().into());
                run_service(&uv, &["python".into(), "install".into(), version.into()], &staging, env.clone(), Duration::from_secs(1800))?;
                if let Some(pkgs) = req.packages.as_ref().filter(|p| !p.is_empty()) {
                    let venv = root.join("venv");
                    run_service(&uv, &["venv".into(), "--python".into(), version.into(), venv.to_string_lossy().into()], &staging, env.clone(), Duration::from_secs(600))?;
                    let mut args = vec!["pip".into(), "install".into(), "--no-build".into(), "--python".into(), venv.join("Scripts").join("python.exe").to_string_lossy().into()];
                    args.extend(pkgs.iter().cloned());
                    run_service(&uv, &args, &staging, env, Duration::from_secs(1800))?;
                }
            }
            "uv-tool" => {
                let uv = ctx.uv.clone().ok_or_else(|| err("[bootstrap].uv is not set — run `cimon setup`"))?;
                let pkg = def.package.as_deref().ok_or_else(|| err("uv-tool needs package"))?;
                let mut env = BTreeMap::new();
                env.insert("UV_TOOL_DIR".into(), root.join("tools").to_string_lossy().into());
                env.insert("UV_TOOL_BIN_DIR".into(), root.join("bin").to_string_lossy().into());
                env.insert("UV_CACHE_DIR".into(), ctx.layout.bootstrap().join("uv-cache").to_string_lossy().into());
                let spec = if pkg.contains("==") { fill(pkg, version) } else { format!("{pkg}=={version}") };
                run_service(&uv, &["tool".into(), "install".into(), "--no-build".into(), spec], &staging, env, Duration::from_secs(1800))?;
            }
            "script" => {
                let s = def.script.as_ref().ok_or_else(|| err("script installer needs script"))?;
                let mirror = ctx.mirrors.join(format!("{}.git", s.from_project));
                if !mirror.join("HEAD").exists() {
                    return Err(err(format!("mirror of {} does not exist yet — run a job of that project first, or `cimon provision` fetches it", s.from_project)));
                }
                // the mirror's own git (--git-dir, the service's options), also when an
                // admin runs `cimon provision` on a mirror the service created
                let Some(body) = cimon_workspace::Mirror { dir: mirror.clone() }.show(&s.ref_, &s.path) else {
                    return Err(err(format!("{}:{} not found in the {} mirror", &s.ref_[..s.ref_.len().min(10)], s.path, s.from_project)));
                };
                let script = staging.join(Path::new(&s.path).file_name().unwrap_or_default());
                std::fs::write(&script, &body).map_err(|e| err(e.to_string()))?;
                let mut args = vec!["-NoProfile".to_string(), "-NonInteractive".into(), "-ExecutionPolicy".into(), "Bypass".into(), "-File".into(), script.to_string_lossy().into()];
                for a in &s.args {
                    args.push(a.replace("{version}", version).replace("{root}", &root.to_string_lossy()));
                }
                let ps = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
                // the script runs as the build account and writes {root}: write access to
                // this install's staging dir only. Its output is never moved into place —
                // the service copies it (below), so the toolchain is owned by the service
                if let Some(a) = &ctx.account {
                    cimon_runner::grant_modify(&staging, &a.name).map_err(err)?;
                }
                run_build(ctx, &ps, &args, &staging, BTreeMap::new(), Duration::from_secs(1800))?;
            }
            other => return Err(err(format!("unknown installer {other}"))),
        }
        Ok(())
    })();
    let fail = |e: InstallError| {
        let _ = remove_dir(&staging);
        if in_place {
            let _ = remove_dir(&final_dir);
        }
        e
    };
    if let Err(e) = res {
        return Err(fail(e));
    }
    // What a script wrote as the build account stays the build account's (an owner can
    // rewrite its ACL), and it may hold links or junctions. The service copies it into
    // the final folder — copies it owns, inheriting the read-only grant — refusing any
    // reparse point, and probes the copy.
    if def.installer == "script" && build_mode {
        if final_dir.exists() {
            remove_dir(&final_dir).map_err(|e| fail(err(format!("{}: {e}", final_dir.display()))))?;
        }
        if let Err(e) = cimon_runner::copy_tree_no_links(&root, &final_dir) {
            let _ = remove_dir(&final_dir);
            return Err(fail(err(format!("{e} — not installed"))));
        }
        let lay = layout_of(ctx, kind, def, version, &final_dir, req);
        if let Err(e) = probe(ctx, def, version, &final_dir, &lay) {
            let _ = remove_dir(&final_dir);
            return Err(fail(e));
        }
        let _ = remove_dir(&staging);
        let t = now();
        return Ok(ToolchainInstall {
            kind: kind.into(),
            version: vkey,
            entry_sha256: entry.into(),
            root: final_dir.to_string_lossy().into(),
            checksum,
            source_ref: Some(source_ref.into()),
            installed_by: ctx.installed_by.clone(),
            installed_at: t.clone(),
            last_used_at: t,
        });
    }
    // probe before it is used (as the build account)
    let lay = layout_of(ctx, kind, def, version, &root, req);
    probe(ctx, def, version, &root, &lay).map_err(fail)?;
    // move into place; rustup installs in the shared RUSTUP_HOME instead
    let installed_root = if def.installer == "rustup" {
        ctx.layout.bootstrap().join("rustup").join("toolchains").join(format!("{version}-x86_64-pc-windows-msvc"))
    } else if in_place {
        final_dir.clone()
    } else {
        if final_dir.exists() {
            remove_dir(&final_dir).map_err(|e| fail(err(format!("{}: {e}", final_dir.display()))))?;
        }
        if let Some(p) = final_dir.parent() {
            std::fs::create_dir_all(p).map_err(|e| fail(err(e.to_string())))?;
        }
        std::fs::rename(&root, &final_dir).map_err(|e| fail(err(format!("moving into {}: {e}", final_dir.display()))))?;
        final_dir.clone()
    };
    // a move keeps the old ACL: re-inherit, so the shared toolchain is read-only for
    // build steps (design §15.1). Only the service wrote this tree; `icacls /reset /T`
    // follows junctions, so a tree holding one is refused rather than reset.
    if build_mode && def.installer != "rustup" {
        let check = cimon_runner::find_reparse_point(&installed_root).map_or(Ok(()), |p| Err(format!("{} is a link or junction", p.display())));
        if let Err(e) = check.and_then(|_| cimon_runner::reset_acl(&installed_root)) {
            let _ = remove_dir(&installed_root);
            let _ = remove_dir(&staging);
            return Err(err(format!("{e} — not installed")));
        }
    }
    let _ = remove_dir(&staging);
    let t = now();
    Ok(ToolchainInstall {
        kind: kind.into(),
        version: vkey,
        entry_sha256: entry.into(),
        root: installed_root.to_string_lossy().into(),
        checksum,
        source_ref: Some(source_ref.into()),
        installed_by: ctx.installed_by.clone(),
        installed_at: t.clone(),
        last_used_at: t,
    })
}

fn fill(s: &str, version: &str) -> String {
    let url_enc: String = version.bytes().map(|b| if b.is_ascii_alphanumeric() || matches!(b, b'.' | b'-' | b'_') { (b as char).to_string() } else { format!("%{b:02X}") }).collect();
    s.replace("{version_url}", &url_enc).replace("{version}", version)
}

fn remove_dir(p: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(p) {
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        other => other,
    }
}

fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().user_agent(concat!("cimon/", env!("CARGO_PKG_VERSION"))).timeout(Duration::from_secs(3600)).build().expect("http client")
}

/// Downloads `url` to `file`; returns its sha256.
pub fn download(url: &str, file: &Path) -> Result<String, InstallError> {
    let mut resp = client().get(url).send().map_err(|e| net(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(net(format!("GET {url}: HTTP {}", resp.status())));
    }
    let mut f = std::fs::File::create(file).map_err(|e| err(e.to_string()))?;
    let mut h = Sha256::new();
    let mut buf = vec![0u8; 1 << 20];
    loop {
        let n = resp.read(&mut buf).map_err(|e| net(format!("reading {url}: {e}")))?;
        if n == 0 {
            break;
        }
        h.update(&buf[..n]);
        std::io::Write::write_all(&mut f, &buf[..n]).map_err(|e| err(e.to_string()))?;
    }
    Ok(hex::encode(h.finalize()))
}

fn get_text(url: &str) -> Result<String, InstallError> {
    let resp = client().get(url).send().map_err(|e| net(format!("GET {url}: {e}")))?;
    if !resp.status().is_success() {
        return Err(net(format!("GET {url}: HTTP {}", resp.status())));
    }
    resp.text().map_err(|e| net(e.to_string()))
}

fn verify(got: &str, want: &str, url: &str) -> Result<(), InstallError> {
    if !got.eq_ignore_ascii_case(want.trim()) {
        return Err(err(format!("checksum mismatch for {url}: got {got}, expected {want} — not installed")));
    }
    Ok(())
}

fn expected_checksum(cs: &cimon_recipe::catalog::Checksum, version: &str, url: &str, _github: &str) -> Result<String, InstallError> {
    match cs.source.as_str() {
        "sha256" => cs.values.get(version).cloned().ok_or_else(|| err(format!("no sha256 for {version} in the catalog's checksum.values"))),
        "shasums" => {
            let list_url = fill(cs.url.as_deref().ok_or_else(|| err("shasums needs url"))?, version);
            let text = get_text(&list_url)?;
            let name = url.rsplit('/').next().unwrap_or(url);
            text.lines()
                .find_map(|l| {
                    let mut parts = l.split_whitespace();
                    let sum = parts.next()?;
                    let file = parts.next()?.trim_start_matches('*');
                    (file == name).then(|| sum.to_string())
                })
                .ok_or_else(|| err(format!("{name} is not listed in {list_url}")))
        }
        "adoptium" => {
            // the assets API lists the checksum of each binary package
            let api = format!(
                "https://api.adoptium.net/v3/assets/version/jdk-{}?architecture=x64&heap_size=normal&image_type=jdk&jvm_impl=hotspot&os=windows&project=jdk&vendor=eclipse",
                fill("{version_url}", version)
            );
            let v: serde_json::Value = serde_json::from_str(&get_text(&api)?).map_err(|e| net(e.to_string()))?;
            v.as_array()
                .and_then(|a| a.iter().find_map(|rel| rel["binaries"].as_array()?.iter().find_map(|b| b["package"]["checksum"].as_str().map(str::to_string))))
                .ok_or_else(|| err(format!("no checksum for jdk {version} from the Adoptium API")))
        }
        other => Err(err(format!("unknown checksum source {other}"))),
    }
}

/// Release asset URL and its sha256 `digest` from the GitHub API.
fn github_asset(api: &str, repo: &str, tag: &str, asset: &str) -> Result<(String, String), InstallError> {
    let url = format!("{}/repos/{repo}/releases/tags/{tag}", api.trim_end_matches('/'));
    let v: serde_json::Value = serde_json::from_str(&get_text(&url)?).map_err(|e| net(format!("{url}: {e}")))?;
    let a = v["assets"].as_array().and_then(|a| a.iter().find(|x| x["name"] == asset)).ok_or_else(|| err(format!("{repo} {tag} has no asset {asset}")))?;
    let digest = a["digest"].as_str().and_then(|d| d.strip_prefix("sha256:")).ok_or_else(|| err(format!("{repo} {tag} {asset} has no sha256 digest — not installed")))?;
    let dl = a["browser_download_url"].as_str().ok_or_else(|| err("asset has no download url"))?;
    Ok((dl.to_string(), digest.to_string()))
}

/// Extracts a zip, refusing entries that escape `dest`.
pub fn extract(file: &Path, dest: &Path) -> Result<(), InstallError> {
    let f = std::fs::File::open(file).map_err(|e| err(e.to_string()))?;
    let mut z = zip::ZipArchive::new(f).map_err(|e| err(format!("not a zip archive: {e}")))?;
    for i in 0..z.len() {
        let mut entry = z.by_index(i).map_err(|e| err(e.to_string()))?;
        let Some(rel) = entry.enclosed_name() else {
            return Err(err(format!("archive entry {:?} escapes the install root — refused", entry.name())));
        };
        let out = dest.join(rel);
        if entry.is_dir() {
            std::fs::create_dir_all(&out).map_err(|e| err(e.to_string()))?;
            continue;
        }
        if let Some(p) = out.parent() {
            std::fs::create_dir_all(p).map_err(|e| err(e.to_string()))?;
        }
        let mut w = std::fs::File::create(&out).map_err(|e| err(e.to_string()))?;
        std::io::copy(&mut entry, &mut w).map_err(|e| err(e.to_string()))?;
    }
    Ok(())
}

fn sys_env(extra: BTreeMap<String, String>, bins: &[PathBuf], profile: Option<&Path>, user: Option<&str>) -> BTreeMap<String, String> {
    let mut env = cimon_runner::base_env(profile, user);
    let mut path = bins.to_vec();
    path.extend(cimon_runner::system_path());
    env.insert("PATH".into(), cimon_runner::join_path(&path));
    env.extend(extra);
    env
}

fn exec(account: &cimon_runner::Account, program: &Path, args: &[String], cwd: &Path, env: BTreeMap<String, String>, timeout: Duration) -> cimon_runner::ExecResult {
    let spec = cimon_runner::ExecSpec {
        program: program.to_path_buf(),
        args: args.to_vec(),
        cwd: cwd.to_path_buf(),
        env,
        timeout,
        log_path: cwd.join(format!("{}.log", program.file_stem().unwrap_or_default().to_string_lossy())),
        log_cap: 20 << 20,
        error_patterns: cimon_runner::exec::default_error_patterns(),
        fail_on_output: vec![],
        capture_prefixes: vec![String::new()],
        preamble: vec![],
    };
    cimon_runner::execute(&spec, account, &AtomicBool::new(false))
}

fn check_ok(what: &Path, r: &cimon_runner::ExecResult) -> Result<(), InstallError> {
    if let Some(e) = &r.spawn_error {
        return Err(err(format!("{}: {e}", what.display())));
    }
    if r.exit_code != Some(0) {
        return Err(err(format!("{} exited {:?}: {}", what.display(), r.exit_code, r.tail.iter().rev().take(5).cloned().collect::<Vec<_>>().join(" | "))));
    }
    Ok(())
}

/// Runs a bootstrap tool (rustup, uv) as the service account — they verify their own
/// downloads (design §10.3).
fn run_service(program: &Path, args: &[String], cwd: &Path, extra: BTreeMap<String, String>, timeout: Duration) -> Result<(), InstallError> {
    let r = exec(&cimon_runner::Account::Current, program, args, cwd, sys_env(extra, &[], None, None), timeout);
    check_ok(program, &r)
}

/// Runs an install script as the build account (single-account mode: as the service).
fn run_build(ctx: &Ctx, program: &Path, args: &[String], cwd: &Path, extra: BTreeMap<String, String>, timeout: Duration) -> Result<(), InstallError> {
    let (acct, profile, user) = match &ctx.account {
        Some(a) => (a.account(), Some(a.profile_dir.clone()), Some(a.name.clone())),
        None => (cimon_runner::Account::Current, None, None),
    };
    let r = exec(&acct, program, args, cwd, sys_env(extra, &[], profile.as_deref(), user.as_deref()), timeout);
    check_ok(program, &r)
}

/// Runs the catalog probe (as the build account) and checks its output. A probe that
/// prints the version and then fails or hangs is a failure too.
pub fn probe(ctx: &Ctx, def: &KindDef, version: &str, root: &Path, lay: &InstLayout) -> Result<String, InstallError> {
    let Some(p) = &def.probe else { return Ok(String::new()) };
    let expected = def.expected_probe(version).unwrap_or_default();
    let (acct, profile, user) = match &ctx.account {
        Some(a) => (a.account(), Some(a.profile_dir.clone()), Some(a.name.clone())),
        None => (cimon_runner::Account::Current, None, None),
    };
    let mut extra = BTreeMap::new();
    for (k, v) in &def.exports {
        // the same placeholders steps get (engine toolchains::placeholders)
        extra.insert(
            k.clone(),
            v.replace("{version}", version)
                .replace("{home}", &lay.home.to_string_lossy())
                .replace("{root}", &root.to_string_lossy())
                .replace("{bin}", &lay.bins.first().map(|b| b.to_string_lossy().to_string()).unwrap_or_default()),
        );
    }
    if def.installer == "rustup" {
        extra.insert("RUSTUP_HOME".into(), ctx.layout.bootstrap().join("rustup").to_string_lossy().into());
        extra.insert("RUSTUP_TOOLCHAIN".into(), version.into());
    }
    let env = sys_env(extra, &lay.bins, profile.as_deref(), user.as_deref());
    let path: Vec<PathBuf> = env.get("PATH").map(|s| s.split(';').map(PathBuf::from).collect()).unwrap_or_default();
    let program = cimon_runner::resolve_program(&p.run[0], &lay.home, &path, env.get("PATHEXT").map(|s| s.as_str()).unwrap_or(".EXE;.BAT;.CMD")).map_err(|e| err(format!("probe: {e}")))?;
    let tmp = ctx.layout.toolchain_staging().join(format!("probe-{}", std::process::id()));
    let _ = std::fs::create_dir_all(&tmp);
    let r = exec(&acct, &program, &p.run[1..].to_vec(), &tmp, env, Duration::from_secs(120));
    let out = r.tail.join("\n");
    let _ = std::fs::remove_dir_all(&tmp);
    if let Some(e) = &r.spawn_error {
        return Err(err(format!("probe `{}` could not start: {e} — not installed", p.run.join(" "))));
    }
    if r.timed_out || r.exit_code != Some(0) {
        return Err(err(format!("probe `{}` {} — not installed", p.run.join(" "), if r.timed_out { "timed out".to_string() } else { format!("exited {:?}", r.exit_code) })));
    }
    if !out.contains(&expected) {
        return Err(err(format!("probe `{}` did not print {expected:?} (got: {}) — not installed", p.run.join(" "), out.lines().take(3).collect::<Vec<_>>().join(" | "))));
    }
    Ok(out)
}

#[cfg(test)]
mod tests;
