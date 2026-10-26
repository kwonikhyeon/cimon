//! Thin wrapper over the git CLI for a bare repository.

use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

pub const ZERO: &str = "0000000000000000000000000000000000000000";

#[derive(Debug, thiserror::Error)]
pub enum GitError {
    #[error("git {args}: {stderr}")]
    Failed { args: String, stderr: String, code: Option<i32> },
    #[error("git: {0}")]
    Io(#[from] std::io::Error),
}

#[derive(Debug, Clone)]
pub struct Git {
    pub git_dir: PathBuf,
}

pub struct Output {
    pub stdout: Vec<u8>,
    pub code: Option<i32>,
}

impl Git {
    pub fn new(git_dir: impl Into<PathBuf>) -> Self {
        Git { git_dir: git_dir.into() }
    }

    fn command(&self) -> Command {
        let mut c = Command::new("git");
        // safe.directory=*: an admin's `cimon recipe import` creates the repository and
        // the service reads it (and the other way round); git's owner check would
        // refuse one of them. The repository is named explicitly (--git-dir) and only
        // the service and admins can write it.
        c.arg(format!("--git-dir={}", self.git_dir.display()))
            .args(["-c", "safe.directory=*", "-c", "core.autocrlf=false", "-c", "core.quotepath=false", "-c", "gc.auto=0", "-c", "advice.detachedHead=false"])
            .env("GIT_TERMINAL_PROMPT", "0")
            .env_remove("GIT_DIR")
            .env_remove("GIT_INDEX_FILE")
            .env_remove("GIT_WORK_TREE");
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            c.creation_flags(0x0800_0000); // CREATE_NO_WINDOW
        }
        c
    }

    /// Runs git; non-zero exit is an error.
    pub fn run(&self, args: &[&str]) -> Result<Vec<u8>, GitError> {
        self.run_with(args, None, &[])
    }

    pub fn run_str(&self, args: &[&str]) -> Result<String, GitError> {
        Ok(String::from_utf8_lossy(&self.run(args)?).trim().to_string())
    }

    pub fn run_with(&self, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Result<Vec<u8>, GitError> {
        let out = self.run_raw(args, stdin, env)?;
        match out.code {
            Some(0) => Ok(out.stdout),
            _ => unreachable!("run_raw returns errors for non-zero"),
        }
    }

    /// Like `run_with` but returns the exit code instead of failing on 1 (merge-tree).
    pub fn run_allow(&self, args: &[&str], allowed: &[i32], env: &[(&str, &str)]) -> Result<Output, GitError> {
        let mut c = self.command();
        c.args(args).stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped());
        for (k, v) in env {
            c.env(k, v);
        }
        let out = c.output()?;
        let code = out.status.code();
        if code.is_some_and(|c| c == 0 || allowed.contains(&c)) {
            Ok(Output { stdout: out.stdout, code })
        } else {
            Err(GitError::Failed { args: args.join(" "), stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(), code })
        }
    }

    fn run_raw(&self, args: &[&str], stdin: Option<&[u8]>, env: &[(&str, &str)]) -> Result<Output, GitError> {
        let mut c = self.command();
        c.args(args).stdout(Stdio::piped()).stderr(Stdio::piped());
        c.stdin(if stdin.is_some() { Stdio::piped() } else { Stdio::null() });
        for (k, v) in env {
            c.env(k, v);
        }
        let mut child = c.spawn()?;
        let writer = stdin.map(|data| {
            let mut pipe = child.stdin.take().unwrap();
            let data = data.to_vec();
            std::thread::spawn(move || {
                let _ = pipe.write_all(&data);
            })
        });
        let out = child.wait_with_output()?;
        if let Some(w) = writer {
            let _ = w.join();
        }
        if out.status.success() {
            Ok(Output { stdout: out.stdout, code: Some(0) })
        } else {
            Err(GitError::Failed {
                args: args.join(" "),
                stderr: String::from_utf8_lossy(&out.stderr).trim().to_string(),
                code: out.status.code(),
            })
        }
    }

    pub fn init_bare(dir: &Path) -> Result<Git, GitError> {
        std::fs::create_dir_all(dir)?;
        let g = Git::new(dir);
        let out = Command::new("git").args(["init", "--bare", "-q", "-b", "main"]).arg(dir).output()?;
        if !out.status.success() {
            return Err(GitError::Failed { args: "init --bare".into(), stderr: String::from_utf8_lossy(&out.stderr).into(), code: out.status.code() });
        }
        g.run(&["config", "gc.auto", "0"])?;
        Ok(g)
    }

    /// Full commit id for `rev`, or None if it does not resolve to a commit.
    pub fn rev(&self, rev: &str) -> Option<String> {
        if rev.starts_with('-') {
            return None;
        }
        self.run_str(&["rev-parse", "--verify", "--quiet", &format!("{rev}^{{commit}}")]).ok().filter(|s| !s.is_empty())
    }

    pub fn ref_value(&self, refname: &str) -> Option<String> {
        self.run_str(&["rev-parse", "--verify", "--quiet", refname]).ok().filter(|s| !s.is_empty())
    }

    /// Compare-and-swap a ref. `old` = None asserts the ref does not exist.
    pub fn update_ref(&self, refname: &str, new: &str, old: Option<&str>, msg: &str) -> Result<(), GitError> {
        self.run(&["update-ref", "-m", msg, refname, new, old.unwrap_or(ZERO)]).map(|_| ())
    }

    pub fn delete_ref(&self, refname: &str, old: &str) -> Result<(), GitError> {
        self.run(&["update-ref", "-d", refname, old]).map(|_| ())
    }

    pub fn is_ancestor(&self, a: &str, b: &str) -> bool {
        self.run_allow(&["merge-base", "--is-ancestor", a, b], &[1], &[]).map(|o| o.code == Some(0)).unwrap_or(false)
    }

    pub fn merge_base(&self, a: &str, b: &str) -> Option<String> {
        self.run_str(&["merge-base", a, b]).ok().filter(|s| !s.is_empty())
    }

    /// Every file at `commit`: (path, blob id).
    pub fn ls_tree(&self, commit: &str) -> Result<Vec<(String, String)>, GitError> {
        let out = self.run(&["ls-tree", "-r", "-z", "--full-tree", commit])?;
        let mut v = vec![];
        for entry in out.split(|b| *b == 0).filter(|e| !e.is_empty()) {
            let s = String::from_utf8_lossy(entry);
            let Some((meta, path)) = s.split_once('\t') else { continue };
            let parts: Vec<&str> = meta.split_whitespace().collect();
            if parts.len() == 3 && parts[1] == "blob" {
                v.push((path.to_string(), parts[2].to_string()));
            }
        }
        Ok(v)
    }

    /// Reads many blobs through one `cat-file --batch`.
    pub fn cat_blobs(&self, ids: &[String]) -> Result<Vec<Vec<u8>>, GitError> {
        if ids.is_empty() {
            return Ok(vec![]);
        }
        let input = ids.join("\n") + "\n";
        let out = self.run_with(&["cat-file", "--batch"], Some(input.as_bytes()), &[])?;
        let mut res = Vec::with_capacity(ids.len());
        let mut pos = 0usize;
        for _ in ids {
            let nl = out[pos..].iter().position(|b| *b == b'\n').ok_or_else(|| GitError::Failed { args: "cat-file".into(), stderr: "short output".into(), code: None })? + pos;
            let header = String::from_utf8_lossy(&out[pos..nl]).to_string();
            let size: usize = header.rsplit(' ').next().and_then(|s| s.parse().ok()).ok_or_else(|| GitError::Failed {
                args: "cat-file".into(),
                stderr: format!("bad header {header}"),
                code: None,
            })?;
            let start = nl + 1;
            res.push(out[start..start + size].to_vec());
            pos = start + size + 1;
        }
        Ok(res)
    }

    pub fn hash_object(&self, data: &[u8]) -> Result<String, GitError> {
        Ok(String::from_utf8_lossy(&self.run_with(&["hash-object", "-w", "--stdin"], Some(data), &[])?).trim().to_string())
    }
}
