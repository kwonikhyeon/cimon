//! Base toolchains (design §10.2): installed by an admin, only checked here.

use cimon_recipe::catalog::KindDef;
use cimon_recipe::model::ToolchainReq;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::time::Duration;

#[derive(Debug, Clone)]
pub struct Found {
    pub root: PathBuf,
    pub bins: Vec<PathBuf>,
    pub version: String,
    pub probe: Option<String>,
}

pub fn check(base: &BTreeMap<String, PathBuf>, kind: &str, def: &KindDef, req: &ToolchainReq) -> Result<Found, String> {
    let check = def.check.clone().unwrap_or_default();
    let mut found = Found { root: PathBuf::new(), bins: vec![], version: "present".into(), probe: None };
    if check.vswhere {
        let vswhere = PathBuf::from(std::env::var("ProgramFiles(x86)").unwrap_or_else(|_| r"C:\Program Files (x86)".into())).join(r"Microsoft Visual Studio\Installer\vswhere.exe");
        if !vswhere.exists() {
            return Err(format!("vswhere not found at {} — Visual Studio Build Tools are not installed", vswhere.display()));
        }
        let mut args: Vec<String> = vec!["-products".into(), "*".into(), "-format".into(), "json".into(), "-utf8".into()];
        if let Some(c) = &req.components {
            if !c.is_empty() {
                args.push("-requires".into());
                args.extend(c.iter().cloned());
            }
        }
        if let Some(min) = &req.min {
            args.push("-version".into());
            args.push(format!("[{min},)"));
        }
        let out = std::process::Command::new(&vswhere).args(&args).output().map_err(|e| e.to_string())?;
        let v: serde_json::Value = serde_json::from_slice(&out.stdout).map_err(|e| format!("vswhere output: {e}"))?;
        let inst = v.as_array().and_then(|a| a.first()).ok_or_else(|| {
            format!("no Visual Studio instance has {}{}", req.components.clone().unwrap_or_default().join(", "), req.min.as_ref().map(|m| format!(" (>= {m})")).unwrap_or_default())
        })?;
        found.root = PathBuf::from(inst["installationPath"].as_str().unwrap_or_default());
        found.version = inst["installationVersion"].as_str().unwrap_or("present").to_string();
        return Ok(found);
    }
    let root = match &check.path_from {
        Some(key) => base.get(key).cloned().ok_or_else(|| format!("[base].{key} is not set in cimon.toml"))?,
        None => PathBuf::new(),
    };
    if check.path_from.is_some() && !root.exists() {
        return Err(format!("{} does not exist", root.display()));
    }
    found.root = root.clone();
    found.bins = if def.bin.is_empty() { vec![root.clone()] } else { def.bin.iter().map(|b| root.join(b.replace('/', "\\"))).collect() };
    if check.sdkmanager {
        for p in req.platforms.clone().unwrap_or_default() {
            if !root.join("platforms").join(&p).exists() {
                return Err(format!("Android SDK platform {p} is not installed under {}", root.display()));
            }
        }
        for b in req.build_tools.clone().unwrap_or_default() {
            if !root.join("build-tools").join(&b).exists() {
                return Err(format!("Android SDK build-tools {b} are not installed under {}", root.display()));
            }
        }
    }
    if let Some(p) = &def.probe {
        let expected = def.expected_probe(req.version.as_deref().unwrap_or("")).unwrap_or_default();
        let out = run_probe(&found.bins, &p.run, &root)?;
        if !out.contains(&expected) {
            return Err(format!("probe `{}` did not print {expected:?}", p.run.join(" ")));
        }
        found.version = out.lines().next().unwrap_or("present").trim().to_string();
        found.probe = Some(out);
    }
    let _ = kind;
    Ok(found)
}

fn run_probe(bins: &[PathBuf], run: &[String], cwd: &Path) -> Result<String, String> {
    let mut env = cimon_runner::base_env(None, None);
    let mut path = bins.to_vec();
    path.extend(cimon_runner::system_path());
    env.insert("PATH".into(), cimon_runner::join_path(&path));
    let program = cimon_runner::resolve_program(&run[0], cwd, &path, env.get("PATHEXT").map(|s| s.as_str()).unwrap_or(".EXE;.BAT;.CMD"))?;
    let tmp = std::env::temp_dir().join(format!("cimon-probe-{}", cimon_core::random_id128()));
    let _ = std::fs::create_dir_all(&tmp);
    let spec = cimon_runner::ExecSpec {
        program,
        args: run[1..].to_vec(),
        cwd: tmp.clone(),
        env,
        timeout: Duration::from_secs(60),
        log_path: tmp.join("probe.log"),
        log_cap: 1 << 20,
        error_patterns: vec![],
        fail_on_output: vec![],
        capture_prefixes: vec![],
        preamble: vec![],
        redact: vec![],
    };
    let r = cimon_runner::execute(&spec, &cimon_runner::Account::Current, &AtomicBool::new(false));
    let _ = std::fs::remove_dir_all(&tmp);
    if let Some(e) = r.spawn_error {
        return Err(e);
    }
    Ok(r.tail.join("\n"))
}
