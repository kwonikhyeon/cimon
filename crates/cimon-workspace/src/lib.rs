//! Project repositories (design §11): bare mirrors fetched by the service account,
//! independent clones that borrow the mirror's objects (alternates), warm slots,
//! keep refs that protect borrowed objects, and mirror GC.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

#[derive(Debug, thiserror::Error)]
pub enum WsError {
    #[error("git {args}: {stderr}")]
    Git { args: String, stderr: String },
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Other(String),
}

pub type Result<T> = std::result::Result<T, WsError>;

/// Runs git in `dir` (a work tree or a bare repository).
pub fn git(dir: &Path, args: &[&str]) -> Result<String> {
    git_env(dir, args, &[])
}

/// Like [`git`] with extra environment (credentials for fetch go here, never on the
/// command line).
pub fn git_env(dir: &Path, args: &[&str], env: &[(String, String)]) -> Result<String> {
    let mut c = git_cmd(Some(dir));
    c.args(args);
    for (k, v) in env {
        c.env(k, v);
    }
    let out = c.output()?;
    if !out.status.success() {
        return Err(WsError::Git { args: args.join(" "), stderr: String::from_utf8_lossy(&out.stderr).trim().to_string() });
    }
    Ok(String::from_utf8_lossy(&out.stdout).trim_end().to_string())
}

fn git_bytes(dir: &Path, args: &[&str]) -> Result<Vec<u8>> {
    let out = git_cmd(Some(dir)).args(args).output()?;
    if !out.status.success() {
        return Err(WsError::Git { args: args.join(" "), stderr: String::from_utf8_lossy(&out.stderr).trim().to_string() });
    }
    Ok(out.stdout)
}

fn git_ok(dir: &Path, args: &[&str]) -> bool {
    git_cmd(Some(dir)).args(args).stdout(Stdio::null()).stderr(Stdio::null()).status().map(|s| s.success()).unwrap_or(false)
}

fn git_cmd(dir: Option<&Path>) -> Command {
    let mut c = Command::new("git");
    if let Some(d) = dir {
        c.arg("-C").arg(d);
    }
    c.args(["-c", "core.longpaths=true", "-c", "core.autocrlf=false", "-c", "gc.auto=0", "-c", "advice.detachedHead=false"])
        .args(["-c", "url.https://github.com/.insteadOf=ssh://git@github.com/"])
        .env("GIT_TERMINAL_PROMPT", "0")
        .env_remove("GIT_DIR")
        .env_remove("GIT_WORK_TREE")
        .env_remove("GIT_INDEX_FILE")
        .stdin(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        c.creation_flags(0x0800_0000);
    }
    c
}

/// A bare mirror of one project repository.
#[derive(Debug, Clone)]
pub struct Mirror {
    pub dir: PathBuf,
}

impl Mirror {
    /// Creates the mirror if needed, points `origin` at `url` and fetches branches and
    /// tags. Only `refs/heads/*` and `refs/tags/*` are pruned — `refs/cimon/keep/*`
    /// stays (design §11.1).
    pub fn fetch(dir: &Path, url: &str) -> Result<Mirror> {
        Self::fetch_with(dir, url, &[])
    }

    /// `fetch` with extra git environment, e.g. [`auth_env`] for private repositories.
    pub fn fetch_with(dir: &Path, url: &str, env: &[(String, String)]) -> Result<Mirror> {
        if !dir.join("HEAD").exists() {
            std::fs::create_dir_all(dir)?;
            let out = git_cmd(None).args(["init", "--bare", "-q"]).arg(dir).output()?;
            if !out.status.success() {
                return Err(WsError::Git { args: "init --bare".into(), stderr: String::from_utf8_lossy(&out.stderr).into() });
            }
            git(dir, &["config", "gc.auto", "0"])?;
            git(dir, &["remote", "add", "origin", url])?;
        } else {
            let current = git(dir, &["remote", "get-url", "origin"]).unwrap_or_default();
            if current != url {
                git(dir, &["remote", "set-url", "origin", url])?;
            }
        }
        git_env(dir, &["fetch", "--quiet", "--prune", "--no-tags", "origin", "+refs/heads/*:refs/heads/*", "+refs/tags/*:refs/tags/*"], env)?;
        Ok(Mirror { dir: dir.to_path_buf() })
    }

    pub fn open(dir: &Path) -> Option<Mirror> {
        dir.join("HEAD").exists().then(|| Mirror { dir: dir.to_path_buf() })
    }

    pub fn branch(&self, name: &str) -> Option<String> {
        if name.starts_with('-') {
            return None;
        }
        git(&self.dir, &["rev-parse", "--verify", "--quiet", &format!("refs/heads/{name}^{{commit}}")]).ok().filter(|s| !s.is_empty())
    }

    pub fn commit(&self, rev: &str) -> Option<String> {
        if rev.starts_with('-') {
            return None;
        }
        git(&self.dir, &["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).ok().filter(|s| !s.is_empty())
    }

    pub fn branches(&self) -> Vec<(String, String)> {
        git(&self.dir, &["for-each-ref", "--format=%(refname:short) %(objectname)", "refs/heads/"])
            .unwrap_or_default()
            .lines()
            .filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string())))
            .collect()
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        git_ok(&self.dir, &["merge-base", "--is-ancestor", a, b])
    }

    /// Resolves a requested ref to a commit (design §7.3): a branch name, or a commit
    /// reachable from one of the allowed branches.
    pub fn resolve(&self, r: &str, allowed: impl Fn(&str) -> bool) -> std::result::Result<(String, Option<String>), ResolveError> {
        if let Some(c) = self.branch(r) {
            return Ok((c, Some(r.to_string())));
        }
        let looks_sha = r.len() >= 7 && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_hexdigit());
        if !looks_sha {
            return Err(ResolveError::RefNotFound(format!("branch {r:?} does not exist in the repository")));
        }
        let Some(c) = self.commit(r) else {
            return Err(ResolveError::RefNotFound(format!("commit {r} does not exist in the repository")));
        };
        for (b, head) in self.branches() {
            if allowed(&b) && (head == c || self.is_ancestor(&c, &head)) {
                return Ok((c, None));
            }
        }
        Err(ResolveError::CommitNotAllowed(format!("commit {} is not reachable from any allowed branch", &c[..10])))
    }

    pub fn show(&self, commit: &str, path: &str) -> Option<Vec<u8>> {
        git_bytes(&self.dir, &["show", &format!("{commit}:{path}")]).ok()
    }

    /// Every tracked path at `commit`.
    pub fn tracked(&self, commit: &str) -> Result<Vec<String>> {
        let out = git_bytes(&self.dir, &["ls-tree", "-r", "-z", "--name-only", "--full-tree", commit])?;
        Ok(out.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
    }

    pub fn keep(&self, name: &str, commit: &str) -> Result<()> {
        git(&self.dir, &["update-ref", &format!("refs/cimon/keep/{name}"), commit]).map(|_| ())
    }

    pub fn unkeep(&self, name: &str) {
        let _ = git(&self.dir, &["update-ref", "-d", &format!("refs/cimon/keep/{name}")]);
    }

    pub fn keeps(&self) -> Vec<String> {
        git(&self.dir, &["for-each-ref", "--format=%(refname)", "refs/cimon/keep/"])
            .unwrap_or_default()
            .lines()
            .map(|s| s.trim_start_matches("refs/cimon/keep/").to_string())
            .collect()
    }

    /// GC — only when the runner is idle (design §11.1). Keep refs protect the commits
    /// warm slots and running jobs borrow.
    pub fn gc(&self) -> Result<()> {
        git(&self.dir, &["gc", "--quiet", "--prune=now"]).map(|_| ())
    }
}

/// Git environment that sends a GitHub token as an HTTP header for github.com only.
/// Passed through `GIT_CONFIG_*` variables so it never appears in a process command
/// line or a config file. Existing `GIT_CONFIG_COUNT` entries are kept.
pub fn auth_env(token: &str) -> Vec<(String, String)> {
    use std::fmt::Write;
    let start: usize = std::env::var("GIT_CONFIG_COUNT").ok().and_then(|v| v.parse().ok()).unwrap_or(0);
    let mut b64 = String::new();
    let raw = format!("x-access-token:{token}");
    // minimal base64 (standard alphabet, padded)
    const T: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    for chunk in raw.as_bytes().chunks(3) {
        let n = chunk.iter().enumerate().fold(0u32, |acc, (i, b)| acc | (*b as u32) << (16 - 8 * i));
        for i in 0..4 {
            if i <= chunk.len() {
                let _ = b64.write_char(T[((n >> (18 - 6 * i)) & 63) as usize] as char);
            } else {
                b64.push('=');
            }
        }
    }
    vec![
        ("GIT_CONFIG_COUNT".into(), (start + 1).to_string()),
        (format!("GIT_CONFIG_KEY_{start}"), "http.https://github.com/.extraheader".into()),
        (format!("GIT_CONFIG_VALUE_{start}"), format!("AUTHORIZATION: basic {b64}")),
    ]
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolveError {
    RefNotFound(String),
    CommitNotAllowed(String),
}

/// A work tree checked out from a mirror.
#[derive(Debug, Clone)]
pub struct WorkTree {
    pub dir: PathBuf,
}

impl WorkTree {
    /// Clones `mirror` into `dir` without copying objects (`--shared`), then checks out
    /// `commit` detached.
    pub fn create(mirror: &Mirror, dir: &Path, commit: &str) -> Result<WorkTree> {
        if dir.exists() {
            remove_dir_all_robust(dir)?;
        }
        if let Some(p) = dir.parent() {
            std::fs::create_dir_all(p)?;
        }
        let out = git_cmd(None).args(["clone", "--quiet", "--shared", "--no-checkout"]).arg(&mirror.dir).arg(dir).output()?;
        if !out.status.success() {
            return Err(WsError::Git { args: "clone --shared".into(), stderr: String::from_utf8_lossy(&out.stderr).into() });
        }
        git(dir, &["config", "core.longpaths", "true"])?;
        git(dir, &["config", "core.autocrlf", "false"])?;
        let wt = WorkTree { dir: dir.to_path_buf() };
        wt.checkout(commit)?;
        Ok(wt)
    }

    pub fn open(dir: &Path) -> Option<WorkTree> {
        dir.join(".git").exists().then(|| WorkTree { dir: dir.to_path_buf() })
    }

    pub fn checkout(&self, commit: &str) -> Result<()> {
        git(&self.dir, &["checkout", "--quiet", "--force", "--detach", commit]).map(|_| ())
    }

    /// `git clean -ffdx` except the `keep` patterns (gitignore syntax).
    pub fn clean(&self, keep: &[String]) -> Result<()> {
        let mut args: Vec<String> = vec!["clean".into(), "-ffdxq".into()];
        for k in keep {
            args.push("-e".into());
            args.push(k.clone());
        }
        let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        git(&self.dir, &argv).map(|_| ())
    }

    pub fn head(&self) -> Option<String> {
        git(&self.dir, &["rev-parse", "HEAD"]).ok()
    }

    pub fn has_commit(&self, commit: &str) -> bool {
        git_ok(&self.dir, &["cat-file", "-e", &format!("{commit}^{{commit}}")])
    }

    /// Tracked files (for the "never delete tracked files" rule of pre-clean).
    pub fn tracked(&self) -> Result<Vec<String>> {
        let out = git_bytes(&self.dir, &["ls-files", "-z"])?;
        Ok(out.split(|b| *b == 0).filter(|s| !s.is_empty()).map(|s| String::from_utf8_lossy(s).into_owned()).collect())
    }

    /// Prepares a warm slot for `commit`: reuse when healthy, otherwise recreate.
    /// Returns (tree, recreated, note).
    pub fn prepare_warm(mirror: &Mirror, dir: &Path, commit: &str, keep: &[String]) -> Result<(WorkTree, bool, Option<String>)> {
        if let Some(wt) = WorkTree::open(dir) {
            let reuse = wt.has_commit(commit).then(|| wt.checkout(commit).and_then(|_| wt.clean(keep)));
            match reuse {
                Some(Ok(())) => return Ok((wt, false, None)),
                Some(Err(e)) => {
                    let note = format!("warm slot could not be reused ({e}) — recreated, this job starts cold");
                    let wt = WorkTree::create(mirror, dir, commit)?;
                    return Ok((wt, true, Some(note)));
                }
                None => {
                    let note = "warm slot is missing the commit's objects — recreated, this job starts cold".to_string();
                    let wt = WorkTree::create(mirror, dir, commit)?;
                    return Ok((wt, true, Some(note)));
                }
            }
        }
        Ok((WorkTree::create(mirror, dir, commit)?, true, None))
    }
}

/// `remove_dir_all` that also clears read-only attributes (git objects are read-only on
/// Windows).
pub fn remove_dir_all_robust(dir: &Path) -> std::io::Result<()> {
    match std::fs::remove_dir_all(dir) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(()),
        Err(_) => {
            fn clear(p: &Path) {
                if let Ok(rd) = std::fs::read_dir(p) {
                    for e in rd.flatten() {
                        let path = e.path();
                        if let Ok(md) = e.metadata() {
                            let mut perm = md.permissions();
                            #[allow(clippy::permissions_set_readonly_false)]
                            perm.set_readonly(false);
                            let _ = std::fs::set_permissions(&path, perm);
                            if md.is_dir() {
                                clear(&path);
                            }
                        }
                    }
                }
            }
            clear(dir);
            std::fs::remove_dir_all(dir)
        }
    }
}

/// Matches recipe globs (relative to the work tree) against relative paths.
pub fn glob_set(patterns: &[String]) -> std::result::Result<globset::GlobSet, String> {
    let mut b = globset::GlobSetBuilder::new();
    for p in patterns {
        let g = globset::GlobBuilder::new(&p.replace('\\', "/")).literal_separator(true).build().map_err(|e| format!("{p}: {e}"))?;
        b.add(g);
    }
    b.build().map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;

    pub fn make_repo(dir: &Path, files: &[(&str, &str)]) -> String {
        std::fs::create_dir_all(dir).unwrap();
        let w = dir.join("work");
        git_cmd(None).args(["init", "-q", "-b", "main"]).arg(&w).status().unwrap();
        for (p, c) in files {
            let fp = w.join(p);
            std::fs::create_dir_all(fp.parent().unwrap()).unwrap();
            std::fs::write(fp, c).unwrap();
        }
        git(&w, &["add", "-A"]).unwrap();
        git(&w, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "init"]).unwrap();
        git(&w, &["rev-parse", "HEAD"]).unwrap()
    }

    #[test]
    fn mirror_resolve_clone_keep() {
        let d = tempfile::tempdir().unwrap();
        let c1 = make_repo(&d.path().join("src"), &[("a.txt", "1"), ("rust-toolchain.toml", "[toolchain]\nchannel = \"1.94.0\"\n")]);
        let url = d.path().join("src").join("work").to_string_lossy().replace('\\', "/");
        let m = Mirror::fetch(&d.path().join("m.git"), &url).unwrap();
        assert_eq!(m.branch("main").unwrap(), c1);
        let allowed = |b: &str| b == "main";
        assert_eq!(m.resolve("main", allowed).unwrap().0, c1);
        assert_eq!(m.resolve(&c1[..10], allowed).unwrap().0, c1);
        assert!(matches!(m.resolve("nope", allowed), Err(ResolveError::RefNotFound(_))));
        assert!(m.tracked(&c1).unwrap().contains(&"a.txt".to_string()));
        assert!(m.show(&c1, "rust-toolchain.toml").is_some());

        // a commit only on a branch that is not allowed
        let w = d.path().join("src").join("work");
        git(&w, &["checkout", "-q", "-b", "secret"]).unwrap();
        std::fs::write(w.join("b.txt"), "2").unwrap();
        git(&w, &["add", "-A"]).unwrap();
        git(&w, &["-c", "user.name=t", "-c", "user.email=t@t", "commit", "-q", "-m", "s"]).unwrap();
        let c2 = git(&w, &["rev-parse", "HEAD"]).unwrap();
        let m = Mirror::fetch(&d.path().join("m.git"), &url).unwrap();
        assert!(matches!(m.resolve(&c2, allowed), Err(ResolveError::CommitNotAllowed(_))));

        // warm slot: create, add untracked files, reuse with keep
        let slot = d.path().join("ws").join("warm-1");
        let (wt, recreated, _) = WorkTree::prepare_warm(&m, &slot, &c1, &[]).unwrap();
        assert!(recreated);
        std::fs::create_dir_all(slot.join("target")).unwrap();
        std::fs::write(slot.join("target").join("x.o"), "obj").unwrap();
        std::fs::write(slot.join("junk.txt"), "junk").unwrap();
        let (_, recreated, _) = WorkTree::prepare_warm(&m, &slot, &c2, &["target".to_string()]).unwrap();
        assert!(!recreated);
        assert!(slot.join("target/x.o").exists(), "kept");
        assert!(!slot.join("junk.txt").exists(), "cleaned");
        assert!(slot.join("b.txt").exists());
        assert_eq!(wt.head().unwrap(), c2);
        m.keep("slot-1", &c2).unwrap();
        assert_eq!(m.keeps(), vec!["slot-1"]);
        m.unkeep("slot-1");
        assert!(m.keeps().is_empty());
    }

    #[test]
    fn auth_header_base64() {
        let env = auth_env("abc");
        let v = &env.iter().find(|(k, _)| k.starts_with("GIT_CONFIG_VALUE_")).unwrap().1;
        // base64("x-access-token:abc")
        assert_eq!(v, "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46YWJj");
    }

    #[test]
    fn globs() {
        let g = glob_set(&["app/**/build/*.apk".to_string(), "dist/*.zip".to_string()]).unwrap();
        assert!(g.is_match("app/x/build/a.apk"));
        assert!(g.is_match("dist/a.zip"));
        assert!(!g.is_match("dist/sub/a.zip"));
    }
}
