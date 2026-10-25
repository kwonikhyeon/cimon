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

/// Sets `key` in an environment map the way Windows sees it: names are compared
/// without case, so a later layer's `npm_config_cache` replaces an earlier
/// `NPM_CONFIG_CACHE` instead of both reaching the child in byte order.
/// The spelling already in the map is kept (`PATH` stays `PATH` when a recipe writes
/// `Path`), because the runner itself looks keys up by their usual spelling.
pub fn env_set(env: &mut BTreeMap<String, String>, key: &str, value: String) {
    let existing: Vec<String> = env.keys().filter(|k| k.eq_ignore_ascii_case(key)).cloned().collect();
    match existing.split_first() {
        Some((keep, extra)) => {
            for k in extra {
                env.remove(k);
            }
            env.insert(keep.clone(), value);
        }
        None => {
            env.insert(key.to_string(), value);
        }
    }
}

/// Layers 1–2 plus the system part of PATH.
pub fn base_env(profile_dir: Option<&Path>, user_name: Option<&str>) -> BTreeMap<String, String> {
    let mut env = BTreeMap::new();
    for k in SYSTEM_VARS {
        if let Ok(v) = std::env::var(k) {
            env.insert(k.to_string(), v);
        }
    }
    // English tool messages, so error patterns match (a recipe's [env] may override)
    env.insert("VSLANG".into(), "1033".into());
    env.insert("DOTNET_CLI_UI_LANGUAGE".into(), "en".into());
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
/// itself and its ancestors, and deletes the scheduled tasks that run as it. Processes
/// come from a Toolhelp snapshot and ownership from each process token (no tasklist or
/// wmic: their output is localized, and wmic is gone from current Windows).
pub fn sweep_main() -> i32 {
    let mut rep = SweepReport::default();
    sweep_processes(&mut rep);
    let user = std::env::var("USERNAME").unwrap_or_default();
    let schtasks = PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\schtasks.exe");
    match std::process::Command::new(&schtasks).args(["/query", "/fo", "csv", "/v", "/nh"]).output() {
        Ok(out) if !out.status.success() => rep.errors.push(format!("schtasks /query exited {:?}: {}", out.status.code(), String::from_utf8_lossy(&out.stderr).trim())),
        Err(e) => rep.errors.push(format!("schtasks: {e}")),
        Ok(out) => {
            let text = String::from_utf8_lossy(&out.stdout);
            let mut seen = std::collections::BTreeSet::new();
            for line in text.lines() {
                let cols = parse_csv(line);
                // TaskName is column 1, "Run As User" is column 14 in the verbose CSV
                if cols.len() > 14 {
                    let name = cols[1].clone();
                    let run_as = cols[14].rsplit('\\').next().unwrap_or(&cols[14]).to_string();
                    if run_as.eq_ignore_ascii_case(&user) && !name.starts_with("\\Microsoft") && seen.insert(name.clone()) {
                        let ok = std::process::Command::new(&schtasks).args(["/delete", "/tn", &name, "/f"]).output().map(|o| o.status.success()).unwrap_or(false);
                        if ok {
                            rep.tasks_deleted.push(name);
                        } else {
                            rep.errors.push(format!("could not delete scheduled task {name}"));
                        }
                    }
                }
            }
        }
    }
    println!("{}", serde_json::to_string(&rep).unwrap());
    0
}

#[cfg(windows)]
fn sweep_processes(rep: &mut SweepReport) {
    let Some(sid) = win::current_user_sid() else {
        rep.errors.push("cannot read this process's user".into());
        return;
    };
    let procs = match win::processes() {
        Ok(p) => p,
        Err(e) => {
            rep.errors.push(format!("process list: {e}"));
            return;
        }
    };
    // spare this process and its build-account ancestors (the __exec helper); the walk
    // stops at the first ancestor of another user, so a reused parent id cannot spare
    // an unrelated process
    let parent: std::collections::HashMap<u32, u32> = procs.iter().map(|p| (p.pid, p.parent)).collect();
    let mut spare = std::collections::HashSet::new();
    let mut cur = std::process::id();
    while cur != 0 && (cur == std::process::id() || win::owned_by(cur, &sid)) && spare.insert(cur) {
        cur = parent.get(&cur).copied().unwrap_or(0);
    }
    // the console host of a spared process goes with it
    let consoles: Vec<u32> = procs.iter().filter(|p| p.name.eq_ignore_ascii_case("conhost.exe") && spare.contains(&p.parent)).map(|p| p.pid).collect();
    spare.extend(consoles);
    for p in &procs {
        if p.pid == 0 || spare.contains(&p.pid) {
            continue;
        }
        match win::terminate_if_owned(p.pid, &sid) {
            Ok(true) => rep.killed.push(format!("{} ({})", p.name, p.pid)),
            Ok(false) => {}
            Err(e) => rep.errors.push(format!("could not terminate {} ({}): {e}", p.name, p.pid)),
        }
    }
}

#[cfg(not(windows))]
fn sweep_processes(rep: &mut SweepReport) {
    rep.errors.push("process sweep is Windows-only".into());
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
            "clear" => {
                // only a value that is not there counts as cleared — any other failure
                // would leave the previous job's homes in the account's environment
                let present = std::process::Command::new("reg").args(["query", r"HKCU\Environment", "/v", item]).output().map(|o| o.status.success()).unwrap_or(true);
                if !present {
                    continue;
                }
                std::process::Command::new("reg").args(["delete", r"HKCU\Environment", "/v", item, "/f"]).output()
            }
            _ => return 2,
        };
        match res {
            Ok(o) if o.status.success() => {}
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

/// Opens `path` for reading only if, after following every link and junction, it is
/// still inside `root`. The work tree is writable by build steps, and the service
/// account copies and reads files from it: a junction must not turn an artifact or a
/// report into a copy of a service-only file. The check is made on the open handle,
/// so swapping a directory after the check does not help.
pub fn open_within(root: &Path, path: &Path) -> std::io::Result<std::fs::File> {
    let f = std::fs::File::open(path)?;
    #[cfg(windows)]
    let real = win::final_path(&f)?;
    #[cfg(not(windows))]
    let real = std::fs::canonicalize(path)?;
    let root = std::fs::canonicalize(root)?;
    let norm = |p: &Path| p.to_string_lossy().replace('/', "\\").to_lowercase();
    let (r, f_) = (norm(&root), norm(&real));
    if f_.starts_with(&format!("{}\\", r.trim_end_matches('\\'))) {
        Ok(f)
    } else {
        Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{} leads outside {} (to {})", path.display(), root.display(), real.display())))
    }
}

/// The first link, junction or other reparse point under `dir` (not following any).
pub fn find_reparse_point(dir: &Path) -> Option<PathBuf> {
    fn walk(d: &Path) -> Option<PathBuf> {
        for e in std::fs::read_dir(d).ok()?.flatten() {
            let p = e.path();
            let m = std::fs::symlink_metadata(&p).ok()?;
            if is_reparse(&m) {
                return Some(p);
            }
            if m.is_dir() {
                if let Some(x) = walk(&p) {
                    return Some(x);
                }
            }
        }
        None
    }
    walk(dir)
}

fn is_reparse(m: &std::fs::Metadata) -> bool {
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
        if m.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return true;
        }
    }
    m.file_type().is_symlink()
}

/// Copies `src` to a new `dst` as plain directories and files written by this process,
/// refusing any reparse point (link, junction) on the way.
pub fn copy_tree_no_links(src: &Path, dst: &Path) -> Result<(), String> {
    std::fs::create_dir_all(dst).map_err(|e| format!("{}: {e}", dst.display()))?;
    for e in std::fs::read_dir(src).map_err(|e| format!("{}: {e}", src.display()))? {
        let e = e.map_err(|e| e.to_string())?;
        let from = e.path();
        let m = std::fs::symlink_metadata(&from).map_err(|e| format!("{}: {e}", from.display()))?;
        if is_reparse(&m) {
            return Err(format!("{} is a link or junction — refused", from.display()));
        }
        let to = dst.join(e.file_name());
        if m.is_dir() {
            copy_tree_no_links(&from, &to)?;
        } else if m.is_file() {
            let mut r = std::fs::File::open(&from).map_err(|e| format!("{}: {e}", from.display()))?;
            let mut w = std::fs::File::create(&to).map_err(|e| format!("{}: {e}", to.display()))?;
            std::io::copy(&mut r, &mut w).map_err(|e| format!("{}: {e}", from.display()))?;
        }
    }
    Ok(())
}

/// Gives `user` modify rights on `dir` and below (a step's `${step.out}` in the logs
/// tree, which is otherwise service-only).
pub fn grant_modify(dir: &Path, user: &str) -> Result<(), String> {
    let out = std::process::Command::new("icacls").arg(dir).args(["/grant", &format!("{user}:(OI)(CI)M"), "/Q"]).output().map_err(|e| format!("icacls: {e}"))?;
    if !out.status.success() {
        return Err(format!("icacls {} /grant {user}: {}", dir.display(), String::from_utf8_lossy(&out.stdout).trim()));
    }
    Ok(())
}

/// Replaces every ACL under `dir` with what it inherits from its parent — after a move
/// (which keeps the old ACL) or after a script installer had write access.
pub fn reset_acl(dir: &Path) -> Result<(), String> {
    let out = std::process::Command::new("icacls").arg(dir).args(["/reset", "/T", "/C", "/Q"]).output().map_err(|e| format!("icacls: {e}"))?;
    if !out.status.success() {
        return Err(format!("icacls {} /reset: {}", dir.display(), String::from_utf8_lossy(&out.stdout).trim()));
    }
    Ok(())
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
    fn env_set_keeps_spelling_and_copy_refuses_junctions() {
        let mut env = BTreeMap::new();
        env.insert("PATH".to_string(), "a".to_string());
        env_set(&mut env, "Path", "b".into());
        assert_eq!(env.len(), 1);
        assert_eq!(env.get("PATH").map(|s| s.as_str()), Some("b"));
        env_set(&mut env, "TEMP", "t".into());
        assert_eq!(env.get("TEMP").map(|s| s.as_str()), Some("t"));

        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        std::fs::create_dir_all(src.join("bin")).unwrap();
        std::fs::write(src.join("bin/tool.exe"), "x").unwrap();
        copy_tree_no_links(&src, &d.path().join("dst")).unwrap();
        assert!(d.path().join("dst/bin/tool.exe").is_file());
        assert!(find_reparse_point(&src).is_none());
        #[cfg(windows)]
        {
            let outside = d.path().join("outside");
            std::fs::create_dir_all(&outside).unwrap();
            let ok = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(src.join("j")).arg(&outside).output().map(|o| o.status.success()).unwrap_or(false);
            if ok {
                assert!(find_reparse_point(&src).is_some());
                assert!(copy_tree_no_links(&src, &d.path().join("dst2")).is_err());
            }
        }
    }

    #[test]
    fn open_within_refuses_links_out() {
        let d = tempfile::tempdir().unwrap();
        let ws = d.path().join("ws");
        let outside = d.path().join("secret");
        std::fs::create_dir_all(ws.join("sub")).unwrap();
        std::fs::create_dir_all(&outside).unwrap();
        std::fs::write(ws.join("sub/a.txt"), "a").unwrap();
        std::fs::write(outside.join("key"), "k").unwrap();
        assert!(open_within(&ws, &ws.join("sub/a.txt")).is_ok());
        assert!(open_within(&ws, &outside.join("key")).is_err());
        assert!(open_within(&ws, &ws.join("sub/../../secret/key")).is_err());
        #[cfg(windows)]
        {
            // a junction inside the work tree pointing out of it
            let j = ws.join("j");
            let ok = std::process::Command::new("cmd").args(["/c", "mklink", "/J"]).arg(&j).arg(&outside).output().map(|o| o.status.success()).unwrap_or(false);
            if ok {
                assert!(open_within(&ws, &j.join("key")).is_err());
            }
        }
    }

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
