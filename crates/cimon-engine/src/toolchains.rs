//! Toolchains for jobs (design §10): which kinds a plan needs, whether they are
//! installed (by `main`'s catalog entry), installing auto-approved kinds, base checks,
//! and the PATH/exports a job runs with.

use crate::jobs::JobPlan;
use crate::recipes::Validated;
use crate::run::JobCtx;
use crate::App;
use cimon_core::{FailureClass, Verdict};
use cimon_recipe::catalog::{Catalog, KindDef};
use cimon_recipe::Resolved;
use cimon_store::JobRow;
use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::path::PathBuf;

#[derive(Debug, Clone, Default)]
pub struct ToolEnv {
    pub path: Vec<PathBuf>,
    pub exports: BTreeMap<String, String>,
    /// kind → (home, first bin, version)
    pub homes: BTreeMap<String, (PathBuf, PathBuf, String)>,
    /// Identity of the toolchain set — a change makes `when = "cold"` setup run again.
    pub hash: String,
    pub rustup_home: Option<PathBuf>,
}

impl ToolEnv {
    pub fn field(&self, kind: &str, field: &str) -> Option<String> {
        let (home, bin, version) = self.homes.get(kind)?;
        Some(match field {
            "home" => home.to_string_lossy().into(),
            "bin" => bin.to_string_lossy().into(),
            "version" => version.clone(),
            _ => return None,
        })
    }
}

pub struct Provision {
    pub verdict: Verdict,
    pub class: FailureClass,
    pub code: &'static str,
    pub message: String,
}

fn env_err(code: &'static str, message: String) -> Provision {
    Provision { verdict: Verdict::Inconclusive, class: FailureClass::Environment, code, message }
}

/// Catalogs for a job: the one at its recipe commit (what the recipe asks for) and
/// `main`'s (the only one installs come from — design §6.2).
pub fn catalogs(app: &App, recipe_commit: &str) -> Result<(Catalog, Catalog), String> {
    let read = |c: &str| -> Result<Catalog, String> {
        let t = app.recipes.tree(c).map_err(|e| e.to_string())?;
        Catalog::parse(t.read_str_or_empty("toolchains.toml"))
    };
    let main = app.recipes.require_main().map_err(|e| e.to_string())?;
    Ok((read(recipe_commit)?, read(&main)?))
}

trait ReadOr {
    fn read_str_or_empty(&self, p: &str) -> &str;
}
impl ReadOr for cimon_recipe::MemTree {
    fn read_str_or_empty(&self, p: &str) -> &str {
        use cimon_recipe::Tree;
        self.read_str(p).unwrap_or("")
    }
}

/// Provisioning phase of intake (design §8.1): base checks, installs of auto-approved
/// managed kinds from `main`'s catalog, and the trial-branch catalog rule.
pub fn provide(app: &App, id: i64, job: &JobRow, r: &Resolved, plan: &JobPlan) -> Result<(), Provision> {
    if plan.plan.toolchains.is_empty() {
        return Ok(());
    }
    let (cat, main_cat) = catalogs(app, job.recipe_commit.as_deref().unwrap_or_default()).map_err(|e| env_err("catalog_invalid", e))?;
    let mut missing_base = vec![];
    for kind in &plan.plan.toolchains {
        let def = cat.kinds.get(kind).ok_or_else(|| env_err("catalog_invalid", format!("{kind} is not in the catalog")))?;
        let req = r.toolchains.get(kind).cloned().unwrap_or_default();
        if def.is_base() {
            match cimon_toolchain::base::check(app_base(app), kind, def, &req) {
                Ok(found) => {
                    let _ = app.store.set_job_toolchain(id, kind, &found.version, cat.hash(kind).unwrap_or_default(), found.probe.as_deref());
                }
                Err(e) => missing_base.push(format!("{kind}: {e}{}", def.hint.as_ref().map(|h| format!(" — {h}")).unwrap_or_default())),
            }
            continue;
        }
        let version = plan.versions.get(kind).cloned().ok_or_else(|| env_err("toolchain_not_provisioned", format!("{kind}: no version resolved")))?;
        // installs are stored under the version key (python + packages gets a suffix)
        let vkey = cimon_toolchain::version_key(def, &version, &req);
        let entry = cat.hash(kind).unwrap_or_default().to_string();
        let main_entry = main_cat.hash(kind).map(str::to_string);
        if let Ok(Some(i)) = app.store.install(kind, &vkey, &entry) {
            if PathBuf::from(&i.root).exists() {
                let _ = app.store.touch_install(kind, &vkey, &entry);
                let _ = app.store.set_job_toolchain(id, kind, &vkey, &entry, None);
                continue;
            }
        }
        if main_entry.as_deref() != Some(entry.as_str()) {
            return Err(env_err(
                "toolchain_not_provisioned",
                format!(
                    "{kind}: the catalog entry on {} differs from main — an admin reviews it and runs `cimon provision --recipe-ref {} --kind {kind}`",
                    job.recipe_ref, job.recipe_ref
                ),
            ));
        }
        if !app.config.toolchains.auto_approve.iter().any(|k| k == kind) {
            return Err(env_err(
                "toolchain_not_provisioned",
                format!("{kind} {version} is not installed and not in [toolchains].auto_approve — an admin runs `cimon provision {}`", job.project),
            ));
        }
        let _ = app.store.set_status(id, &format!("툴체인: {kind} {version} 설치 중"));
        let mdef = main_cat.kinds.get(kind).unwrap();
        // install scripts and probes run as the build account: not while a job sweeps
        let _build_guard = app.build_guard();
        match cimon_toolchain::install(&installer_ctx(app), kind, mdef, &entry, &version, &req, "main") {
            Ok(inst) => {
                let _ = app.store.upsert_install(&inst);
                let _ = app.store.set_job_toolchain(id, kind, &vkey, &entry, None);
            }
            Err(e) => {
                return Err(Provision {
                    verdict: Verdict::Inconclusive,
                    class: if e.network { FailureClass::Infra } else { FailureClass::Environment },
                    code: "toolchain_install_failed",
                    message: format!("{kind} {version}: {}", e.message),
                })
            }
        }
    }
    if !missing_base.is_empty() {
        return Err(env_err("base_missing", missing_base.join("; ")));
    }
    Ok(())
}

pub fn app_base(app: &App) -> &BTreeMap<String, PathBuf> {
    &app.config.base
}

pub fn installer_ctx(app: &App) -> cimon_toolchain::Ctx {
    cimon_toolchain::Ctx {
        layout: app.layout.clone(),
        github_api: app.config.github.api_url.clone(),
        // installed by `cimon setup` at the versions in [bootstrap]
        rustup: Some(app.layout.bootstrap().join("cargo").join("bin").join("rustup.exe")).filter(|p| p.exists()),
        uv: Some(app.layout.bootstrap().join("uv").join("uv.exe")).filter(|p| p.exists()),
        account: app.build_account(),
        mirrors: app.layout.mirrors(),
        installed_by: "cimon".into(),
    }
}

/// PATH and exports for a job, from what provisioning recorded.
pub fn env_for(app: &App, id: i64, r: &Resolved, plan: &JobPlan) -> Result<ToolEnv, String> {
    let mut env = ToolEnv::default();
    if plan.plan.toolchains.is_empty() {
        env.hash = cimon_core::sha256_hex(b"none");
        return Ok(env);
    }
    let job = app.store.require_job(id).map_err(|e| e.to_string())?;
    let (cat, _) = catalogs(app, job.recipe_commit.as_deref().unwrap_or_default())?;
    let used = app.store.job_toolchains(id).map_err(|e| e.to_string())?;
    let mut ident = String::new();
    // declaration order of [toolchains] (BTreeMap: sorted by kind, stable)
    for kind in &plan.plan.toolchains {
        let def = cat.kinds.get(kind).ok_or_else(|| format!("{kind} missing from the catalog"))?;
        let Some((_, vkey, entry, _)) = used.iter().find(|(k, ..)| k == kind) else {
            return Err(format!("{kind} was not provided by intake"));
        };
        ident.push_str(&format!("{kind}={vkey}@{entry};"));
        // the plain version for `{version}` placeholders; the key finds the install
        let version = &vkey.split("+pkgs.").next().unwrap_or(vkey).to_string();
        let req = r.toolchains.get(kind).cloned().unwrap_or_default();
        let (root, bins, home) = if def.is_base() {
            let found = cimon_toolchain::base::check(app_base(app), kind, def, &req).map_err(|e| format!("{kind}: {e}"))?;
            (found.root.clone(), found.bins.clone(), found.root)
        } else {
            let inst = app.store.install(kind, vkey, entry).map_err(|e| e.to_string())?.ok_or_else(|| format!("{kind} {vkey} is not installed"))?;
            let root = PathBuf::from(&inst.root);
            let layout = cimon_toolchain::layout_of(&installer_ctx(app), kind, def, version, &root, &req);
            (root, layout.bins, layout.home)
        };
        for b in &bins {
            env.path.push(b.clone());
        }
        for (k, v) in &def.exports {
            env.exports.insert(k.clone(), placeholders(v, version, &root, &home, bins.first()));
        }
        if def.installer == "rustup" {
            env.rustup_home = Some(app.layout.bootstrap().join("rustup"));
        }
        env.homes.insert(kind.clone(), (home.clone(), bins.first().cloned().unwrap_or_else(|| root.clone()), version.clone()));
    }
    env.hash = cimon_core::sha256_hex(ident.as_bytes());
    Ok(env)
}

pub fn placeholders(s: &str, version: &str, root: &std::path::Path, home: &std::path::Path, bin: Option<&PathBuf>) -> String {
    s.replace("{version}", version)
        .replace("{root}", &root.to_string_lossy())
        .replace("{home}", &home.to_string_lossy())
        .replace("{bin}", &bin.map(|b| b.to_string_lossy().to_string()).unwrap_or_default())
}

/// `plan`'s view of the toolchains: installed, auto-installable, or admin needed.
pub fn plan_view(app: &App, v: &Validated) -> Value {
    let Ok((cat, main_cat)) = catalogs(app, &v.recipe_commit) else { return json!(v.plan.toolchains) };
    v.plan
        .toolchains
        .iter()
        .map(|k| {
            let def: Option<&KindDef> = cat.kinds.get(k);
            let req = v.resolved.toolchains.get(k).cloned().unwrap_or_default();
            let tier = def.map(|d| d.tier.clone()).unwrap_or_default();
            if def.is_some_and(|d| d.is_base()) {
                let state = match cimon_toolchain::base::check(app_base(app), k, def.unwrap(), &req) {
                    Ok(f) => format!("있음 ({})", f.version),
                    Err(e) => format!("없음 — {e}"),
                };
                return json!({"kind": k, "tier": tier, "state": state});
            }
            let version = req.version.clone().unwrap_or_else(|| format!("({} 에서 접수 때 읽음)", req.version_file.clone().unwrap_or_default()));
            let entry = cat.hash(k).unwrap_or_default();
            let state = if req.version.is_none() {
                "알 수 없음 — 접수 때 확인".to_string()
            } else if app.store.install(k, &version, entry).ok().flatten().is_some() {
                "설치됨".into()
            } else if main_cat.hash(k) != Some(entry) {
                format!("시도 브랜치의 카탈로그 항목이 main 과 다르다 — 관리자 provision --recipe-ref 필요")
            } else if app.config.toolchains.auto_approve.contains(k) {
                "접수 때 자동 설치".into()
            } else {
                "관리자 설치 필요(cimon provision)".into()
            };
            json!({"kind": k, "tier": tier, "version": version, "state": state})
        })
        .collect()
}

/// Executables that appeared in the job's caches and homes (design §10.7) — recorded,
/// not trusted.
pub fn new_executables(ctx: &JobCtx<'_>) -> Vec<String> {
    let mut out = vec![];
    let since = ctx.job.started_at.clone().unwrap_or_default();
    let since = cimon_core::parse_time(&since).map(|t| std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(t.timestamp().max(0) as u64));
    let dirs = [ctx.app.layout.caches(&ctx.job.project_key), ctx.app.layout.homes_job_dir(ctx.job.id), ctx.app.layout.root.join("homes").join("p").join(&ctx.job.project_key)];
    for d in dirs {
        for e in walkdir::WalkDir::new(&d).max_depth(8).into_iter().flatten() {
            if out.len() >= 200 {
                return out;
            }
            let p = e.path();
            let is_exe = p.extension().is_some_and(|x| matches!(x.to_string_lossy().to_lowercase().as_str(), "exe" | "dll" | "bat" | "cmd" | "ps1"));
            if !is_exe {
                continue;
            }
            let fresh = match (since, e.metadata().ok().and_then(|m| m.created().ok())) {
                (Some(s), Some(c)) => c >= s,
                _ => false,
            };
            if fresh {
                out.push(p.to_string_lossy().into());
            }
        }
    }
    out
}

/// One line of `cimon provision` output.
#[derive(Debug, Clone, serde::Serialize)]
pub struct ProvisionLine {
    pub kind: String,
    pub version: String,
    pub outcome: String,
    pub ok: bool,
}

#[derive(Debug, Clone, Default)]
pub struct ProvisionArgs {
    pub project: Option<String>,
    pub kind: Option<String>,
    pub version: Option<String>,
    /// Install from a trial branch's catalog entry after review (design §10.4).
    pub recipe_ref: Option<String>,
    /// Install `kind` at `version` from this local copy instead of the catalog's source
    /// (the admin vouches for it; recorded as `local:<path>`).
    pub from: Option<std::path::PathBuf>,
    pub admin: String,
}

/// `cimon provision` (design §10.4): installs what auto-approve does not, from
/// `main`'s catalog — or, with `--recipe-ref`, from a reviewed trial entry.
pub fn provision(app: &App, a: &ProvisionArgs) -> Result<Vec<ProvisionLine>, String> {
    if a.from.is_some() && (a.kind.is_none() || a.version.is_none() || a.project.is_some()) {
        return Err("--from installs one toolchain: give --kind and --version (not a project)".into());
    }
    let main = app.recipes.require_main().map_err(|e| e.to_string())?;
    let source_ref = a.recipe_ref.clone().unwrap_or_else(|| app.config.recipes.default_branch.clone());
    let commit = app.recipes.resolve(&source_ref).map_err(|e| e.to_string())?;
    let tree = app.recipes.tree(&commit).map_err(|e| e.to_string())?;
    let cat = Catalog::parse(tree.read_str_or_empty("toolchains.toml"))?;
    let _ = main;
    // (kind, version, requirement)
    let mut wanted: Vec<(String, String, cimon_recipe::model::ToolchainReq)> = vec![];
    match (&a.project, &a.kind) {
        (Some(p), kind) => {
            let r = cimon_recipe::resolve_project(&*tree, p).map_err(|d| d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; "))?;
            for (k, req) in &r.toolchains {
                if kind.as_ref().is_some_and(|x| x != k) {
                    continue;
                }
                let version = match (&req.version, &req.version_file) {
                    (Some(v), _) => v.clone(),
                    (None, Some(f)) => {
                        let m = cimon_workspace::Mirror::fetch_with(&app.layout.mirror(p), &r.project.repo, &crate::git_auth(app)).map_err(|e| format!("fetching {p}: {e}"))?;
                        let c = m.branch(&r.project.default_ref).ok_or_else(|| format!("{p}: {} not found", r.project.default_ref))?;
                        let bytes = m.show(&c, f).ok_or_else(|| format!("{p}: {f} not found on {}", r.project.default_ref))?;
                        crate::jobs::channel_of(&bytes)?
                    }
                    (None, None) => String::new(),
                };
                wanted.push((k.clone(), version, req.clone()));
            }
        }
        (None, Some(k)) => {
            let v = a.version.clone().ok_or("--kind needs --version (or give a project)")?;
            wanted.push((k.clone(), v, Default::default()));
        }
        (None, None) => return Err("give a project, or --kind with --version".into()),
    }
    // a script installer reads its script from another project's mirror — fetch that
    // first (the installer itself only reads the mirror, and says provision fetches it)
    let mut fetched = std::collections::BTreeSet::new();
    for (kind, _, _) in &wanted {
        let Some(s) = cat.kinds.get(kind).and_then(|d| d.script.as_ref()) else { continue };
        if !fetched.insert(s.from_project.clone()) {
            continue;
        }
        let r = cimon_recipe::resolve_project(&*tree, &s.from_project).map_err(|d| d.iter().map(|x| x.to_string()).collect::<Vec<_>>().join("; "))?;
        cimon_workspace::Mirror::fetch_with(&app.layout.mirror(&s.from_project), &r.project.repo, &crate::git_auth(app)).map_err(|e| format!("fetching {}: {e}", s.from_project))?;
    }
    let mut out = vec![];
    for (kind, version, req) in wanted {
        let Some(def) = cat.kinds.get(&kind) else {
            out.push(ProvisionLine { kind: kind.clone(), version, outcome: "not in the catalog".into(), ok: false });
            continue;
        };
        if def.is_base() {
            let line = match cimon_toolchain::base::check(app_base(app), &kind, def, &req) {
                Ok(f) => ProvisionLine { kind, version: f.version, outcome: "base — present".into(), ok: true },
                Err(e) => ProvisionLine { kind, version, outcome: format!("base — MISSING: {e}{}", def.hint.as_ref().map(|h| format!(" ({h})")).unwrap_or_default()), ok: false },
            };
            out.push(line);
            continue;
        }
        let entry = cat.hash(&kind).unwrap_or_default().to_string();
        let vkey = cimon_toolchain::version_key(def, &version, &req);
        if app.store.install(&kind, &vkey, &entry).ok().flatten().is_some_and(|i| std::path::Path::new(&i.root).exists()) {
            out.push(ProvisionLine { kind, version: vkey, outcome: "already installed".into(), ok: true });
            continue;
        }
        let mut ctx = installer_ctx(app);
        ctx.installed_by = a.admin.clone();
        let _build_guard = app.build_guard();
        let res = match &a.from {
            Some(src) => cimon_toolchain::install_from_local(&ctx, &kind, def, &entry, &version, &req, src),
            None => cimon_toolchain::install(&ctx, &kind, def, &entry, &version, &req, &source_ref),
        };
        let source = a.from.as_ref().map(|p| format!("the local copy {} (vouched for by {}, no catalog checksum)", p.display(), a.admin)).unwrap_or_else(|| source_ref.clone());
        match res {
            Ok(inst) => {
                let _ = app.store.upsert_install(&inst);
                let _ = app.store.audit(cimon_store::AuditEntry {
                    user: &a.admin,
                    action: "provision",
                    recipe_commit: Some(&commit),
                    detail: Some(format!("{kind} {vkey} from {source} ({})", &entry[..8.min(entry.len())])),
                    ..Default::default()
                });
                out.push(ProvisionLine { kind, version: vkey, outcome: format!("installed from {source}"), ok: true });
            }
            Err(e) => out.push(ProvisionLine { kind, version: vkey, outcome: format!("FAILED: {}", e.message), ok: false }),
        }
    }
    Ok(out)
}
