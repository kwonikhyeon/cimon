//! Running steps (design §9): environment built from scratch, command resolution,
//! Job Objects, the build account, the leftover-process sweep and `[account_env]`.

pub mod exec;
#[cfg(windows)]
pub mod win;

pub use exec::{execute, Account, ExecResult, ExecSpec};

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// System variables every child gets (design §9.4, layer 1).
pub const SYSTEM_VARS: &[&str] = &[
    "SystemRoot",
    "windir",
    "ComSpec",
    "PATHEXT",
    "OS",
    "SystemDrive",
    "ProgramFiles",
    "ProgramFiles(x86)",
    "ProgramW6432",
    "ProgramData",
    "CommonProgramFiles",
    "CommonProgramFiles(x86)",
    "CommonProgramW6432",
    "NUMBER_OF_PROCESSORS",
    "PROCESSOR_ARCHITECTURE",
    "PROCESSOR_IDENTIFIER",
    "PROCESSOR_LEVEL",
    "PROCESSOR_REVISION",
    "COMPUTERNAME",
];

/// User variables (layer 2) — taken from the build account's profile, or from this
/// process in single-account mode.
pub const USER_VARS: &[&str] = &["USERPROFILE", "HOMEDRIVE", "HOMEPATH", "LOCALAPPDATA", "APPDATA", "USERNAME", "USERDOMAIN"];

/// Layers 1–2 plus the system part of PATH.
pub fn base_env(profile_dir: Option<&Path>, user_name: Option<&str>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for k in SYSTEM_VARS {
        if let Ok(v) = std::env::var(k) {
            env.insert(k.to_string(), v);
        }
    }
    match profile_dir {
        Some(p) => {
            let p = p.to_string_lossy().to_string();
            env.insert("USERPROFILE".into(), p.clone());
            env.insert("LOCALAPPDATA".into(), format!("{p}\\AppData\\Local"));
            env.insert("APPDATA".into(), format!("{p}\\AppData\\Roaming"));
            if let Some((drive, rest)) = p.split_once(':') {
                env.insert("HOMEDRIVE".into(), format!("{drive}:"));
                env.insert("HOMEPATH".into(), rest.to_string());
            }
            if let Some(u) = user_name {
                env.insert("USERNAME".into(), u.to_string());
            }
            if let Ok(d) = std::env::var("COMPUTERNAME") {
                env.insert("USERDOMAIN".into(), d);
            }
        }
        None => {
            for k in USER_VARS {
                if let Ok(v) = std::env::var(k) {
                    env.insert(k.to_string(), v);
                }
            }
        }
    }
    env
}

/// `System32` and friends — the tail of every PATH.
pub fn system_path() -> Vec<PathBuf> {
    let root = std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into());
    vec![
        PathBuf::from(format!(r"{root}\System32")),
        PathBuf::from(&root),
        PathBuf::from(format!(r"{root}\System32\Wbem")),
        PathBuf::from(format!(r"{root}\System32\WindowsPowerShell\v1.0")),
    ]
}

/// Resolves `run[0]` (design §9.3): with a path separator, relative to `cwd`; a bare
/// name, only in the constructed `PATH` (with `PATHEXT`), never the work tree.
pub fn resolve_program(program: &str, cwd: &Path, path: &[PathBuf], pathext: &str) -> Result<PathBuf, String> {
    let has_sep = program.contains('/') || program.contains('\\');
    let exts: Vec<String> = pathext.split(';').filter(|e| !e.is_empty()).map(|e| e.to_lowercase()).collect();
    let candidates = |base: PathBuf| -> Vec<PathBuf> {
        let mut v = vec![];
        let has_ext = base.extension().is_some_and(|e| exts.contains(&format!(".{}", e.to_string_lossy().to_lowercase())));
        if has_ext {
            v.push(base.clone());
        }
        for e in &exts {
            let mut s = base.clone().into_os_string();
            s.push(e);
            v.push(PathBuf::from(s));
        }
        if !has_ext && base.extension().is_some() {
            v.push(base);
        }
        v
    };
    if has_sep {
        let p = Path::new(program);
        let full = if p.is_absolute() { p.to_path_buf() } else { cwd.join(p) };
        return candidates(full.clone()).into_iter().find(|c| c.is_file()).ok_or_else(|| format!("{} does not exist", full.display()));
    }
    for dir in path {
        for c in candidates(dir.join(program)) {
            if c.is_file() {
                return Ok(c);
            }
        }
    }
    Err(format!("{program:?} is not on the constructed PATH (declared toolchains + System32) — declare its toolchain, or call a repository file with ./"))
}

pub fn join_path(dirs: &[PathBuf]) -> String {
    dirs.iter().map(|d| d.to_string_lossy().to_string()).collect::<Vec<_>>().join(";")
}

/// The build account, logged on once per service start.
pub struct BuildAccount {
    pub name: String,
    #[cfg(windows)]
    pub logon: std::sync::Arc<win::Logon>,
    pub profile_dir: PathBuf,
    pub helper: PathBuf,
}

impl BuildAccount {
    /// Logs on with the password stored by `cimon setup` (DPAPI, machine scope) in
    /// `password_file`.
    #[cfg(windows)]
    pub fn logon(name: &str, password_file: &Path, helper: &Path) -> Result<BuildAccount, String> {
        let blob = std::fs::read(password_file).map_err(|e| format!("{}: {e} — run `cimon setup`", password_file.display()))?;
        let pw = win::unprotect(&blob).map_err(|e| e.to_string())?;
        let pw = String::from_utf8(pw).map_err(|_| "stored password is not UTF-8".to_string())?;
        let logon = win::Logon::new(name, &pw).map_err(|e| e.to_string())?;
        let profile_dir = PathBuf::from(&logon.profile_dir);
        Ok(BuildAccount { name: name.to_string(), logon: std::sync::Arc::new(logon), profile_dir, helper: helper.to_path_buf() })
    }

    pub fn account(&self) -> Account {
        #[cfg(windows)]
        {
            Account::Build { logon: self.logon.clone(), helper: self.helper.clone() }
        }
        #[cfg(not(windows))]
        {
            Account::Current
        }
    }
}

/// Result of `__sweep` (run as the build account): what was still running and which
/// scheduled tasks it had registered.
#[derive(Debug, Clone, Default, serde::Serialize, serde::Deserialize)]
pub struct SweepReport {
    pub killed: Vec<String>,
    pub tasks_deleted: Vec<String>,
    pub errors: Vec<String>,
}

/// `cimon __sweep` — terminates every process of the current (build) user except
/// itself and its parent chain, and deletes the scheduled tasks that run as it.
pub fn sweep_main() -> i32 {
    let mut rep = SweepReport::default();
    let me = std::process::id();
    let user = std::env::var("USERNAME").unwrap_or_default();
    // tasklist /v gives the user name; /fo csv for parsing
    let out = std::process::Command::new("tasklist").args(["/v", "/fo", "csv", "/nh"]).output();
    if let Ok(out) = out {
        let text = String::from_utf8_lossy(&out.stdout);
        let parent = parent_pid(me);
        for line in text.lines() {
            let cols = parse_csv(line);
            if cols.len() < 7 {
                continue;
            }
            let (name, pid, owner) = (&cols[0], cols[1].parse::<u32>().unwrap_or(0), &cols[6]);
            let owner_user = owner.rsplit('\\').next().unwrap_or(owner);
            if pid == 0 || pid == me || Some(pid) == parent || !owner_user.eq_ignore_ascii_case(&user) {
                continue;
            }
            if matches!(name.to_lowercase().as_str(), "conhost.exe" | "tasklist.exe" | "dllhost.exe" | "sihost.exe" | "taskhostw.exe") {
                continue;
            }
            let ok = std::process::Command::new("taskkill").args(["/f", "/t", "/pid", &pid.to_string()]).output().map(|o| o.status.success()).unwrap_or(false);
            if ok {
                rep.killed.push(format!("{name} ({pid})"));
            } else {
                rep.errors.push(format!("could not terminate {name} ({pid})"));
            }
        }
    } else {
        rep.errors.push("tasklist failed".into());
    }
    if let Ok(out) = std::process::Command::new("schtasks").args(["/query", "/fo", "csv", "/v", "/nh"]).output() {
        let text = String::from_utf8_lossy(&out.stdout);
        let mut seen = std::collections::BTreeSet::new();
        for line in text.lines() {
            let cols = parse_csv(line);
            // TaskName is column 1, "Run As User" is column 14 in the verbose CSV
            if cols.len() > 14 {
                let name = cols[1].clone();
                let run_as = cols[14].rsplit('\\').next().unwrap_or(&cols[14]).to_string();
                if run_as.eq_ignore_ascii_case(&user) && !name.starts_with("\\Microsoft") && seen.insert(name.clone()) {
                    let ok = std::process::Command::new("schtasks").args(["/delete", "/tn", &name, "/f"]).output().map(|o| o.status.success()).unwrap_or(false);
                    if ok {
                        rep.tasks_deleted.push(name);
                    } else {
                        rep.errors.push(format!("could not delete scheduled task {name}"));
                    }
                }
            }
        }
    }
    println!("{}", serde_json::to_string(&rep).unwrap());
    0
}

fn parent_pid(pid: u32) -> Option<u32> {
    let out = std::process::Command::new("wmic").args(["process", "where", &format!("ProcessId={pid}"), "get", "ParentProcessId", "/value"]).output().ok()?;
    String::from_utf8_lossy(&out.stdout).lines().find_map(|l| l.trim().strip_prefix("ParentProcessId=").and_then(|v| v.trim().parse().ok()))
}

fn parse_csv(line: &str) -> Vec<String> {
    let mut out = vec![];
    let mut cur = String::new();
    let mut quoted = false;
    let mut chars = line.chars().peekable();
    while let Some(c) = chars.next() {
        match c {
            '"' if quoted && chars.peek() == Some(&'"') => {
                cur.push('"');
                chars.next();
            }
            '"' => quoted = !quoted,
            ',' if !quoted => out.push(std::mem::take(&mut cur)),
            other => cur.push(other),
        }
    }
    out.push(cur);
    out
}

/// `cimon __account-env set K=V… | clear K…` — writes the current (build) user's
/// `HKCU\Environment` so scheduled tasks see the job's homes (design §9.2).
pub fn account_env_main(args: &[String]) -> i32 {
    let Some((mode, rest)) = args.split_first() else {
        eprintln!("usage: __account-env set K=V… | clear K…");
        return 2;
    };
    for item in rest {
        let res = match mode.as_str() {
            "set" => match item.split_once('=') {
                Some((k, v)) => std::process::Command::new("reg").args(["add", r"HKCU\Environment", "/v", k, "/t", "REG_SZ", "/d", v, "/f"]).output(),
                None => {
                    eprintln!("bad item {item}");
                    return 2;
                }
            },
            "clear" => std::process::Command::new("reg").args(["delete", r"HKCU\Environment", "/v", item, "/f"]).output(),
            _ => return 2,
        };
        match res {
            Ok(o) if o.status.success() || mode == "clear" => {}
            Ok(o) => {
                eprintln!("reg failed for {item}: {}", String::from_utf8_lossy(&o.stderr));
                return 1;
            }
            Err(e) => {
                eprintln!("reg: {e}");
                return 1;
            }
        }
    }
    0
}

/// Free disk space for `[disk].min_free_gb`.
pub fn free_bytes(path: &Path) -> Option<u64> {
    #[cfg(windows)]
    {
        win::free_bytes(path)
    }
    #[cfg(not(windows))]
    {
        let _ = path;
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resolution_rules() {
        let d = tempfile::tempdir().unwrap();
        std::fs::write(d.path().join("gradlew.bat"), "@echo off").unwrap();
        std::fs::create_dir_all(d.path().join("bin")).unwrap();
        std::fs::write(d.path().join("bin").join("tool.exe"), "").unwrap();
        std::fs::write(d.path().join("cargo.exe"), "").unwrap();
        let pathext = ".COM;.EXE;.BAT;.CMD";
        // repo file with ./
        assert_eq!(resolve_program("./gradlew.bat", d.path(), &[], pathext).unwrap(), d.path().join("./gradlew.bat"));
        let gw = resolve_program("./gradlew", d.path(), &[], pathext).unwrap();
        assert_eq!(gw.file_name().unwrap().to_string_lossy().to_lowercase(), "gradlew.bat");
        // bare name only from PATH, never the work tree
        let path = vec![d.path().join("bin")];
        let tool = resolve_program("tool", d.path(), &path, pathext).unwrap();
        assert_eq!(tool.file_name().unwrap().to_string_lossy().to_lowercase(), "tool.exe");
        assert!(tool.starts_with(d.path().join("bin")));
        assert!(resolve_program("cargo", d.path(), &path, pathext).is_err(), "cargo.exe in the work tree must not be picked");
        assert!(resolve_program("cmd", d.path(), &system_path(), pathext).is_ok());
    }

    #[test]
    fn csv() {
        assert_eq!(parse_csv(r#""a","b,c","d""e""#), vec!["a", "b,c", "d\"e"]);
    }
}
