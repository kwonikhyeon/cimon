//! Disk layout under `[paths].root` — design §5.

use std::path::{Path, PathBuf};

#[derive(Debug, Clone)]
pub struct Layout {
    pub root: PathBuf,
}

impl Layout {
    pub fn new(root: impl Into<PathBuf>) -> Self {
        Layout { root: root.into() }
    }
    pub fn state_dir(&self) -> PathBuf {
        self.root.join("state")
    }
    pub fn db(&self) -> PathBuf {
        self.state_dir().join("cimon.db")
    }
    pub fn draining_flag(&self) -> PathBuf {
        self.state_dir().join("draining")
    }
    pub fn config_dir(&self) -> PathBuf {
        self.root.join("config")
    }
    pub fn config_file(&self) -> PathBuf {
        self.config_dir().join("cimon.toml")
    }
    pub fn link_key(&self) -> PathBuf {
        self.config_dir().join("link.key")
    }
    pub fn recipe_checkouts(&self) -> PathBuf {
        self.root.join("recipes")
    }
    pub fn recipe_checkout(&self, commit: &str) -> PathBuf {
        self.recipe_checkouts().join(commit)
    }
    pub fn mirrors(&self) -> PathBuf {
        self.root.join("mirrors")
    }
    pub fn mirror(&self, project_key: &str) -> PathBuf {
        self.mirrors().join(format!("{project_key}.git"))
    }
    pub fn ws(&self) -> PathBuf {
        self.root.join("ws")
    }
    pub fn ws_project(&self, project_key: &str) -> PathBuf {
        self.ws().join(project_key)
    }
    pub fn warm_slot(&self, project_key: &str, slot: u32) -> PathBuf {
        self.ws_project(project_key).join(format!("warm-{slot}"))
    }
    pub fn clean_clone(&self, project_key: &str, job: i64) -> PathBuf {
        self.ws_project(project_key).join(format!("j{job}"))
    }
    pub fn sibling_isolated(&self, project_key: &str, job: i64) -> PathBuf {
        self.ws().join("_sibling").join(project_key).join(format!("j{job}"))
    }
    pub fn toolchains(&self) -> PathBuf {
        self.root.join("toolchains")
    }
    pub fn toolchain_dir(&self, kind: &str, version: &str, entry_hash: &str) -> PathBuf {
        let short = &entry_hash[..entry_hash.len().min(8)];
        self.toolchains().join(kind).join(format!("{}-{short}", sanitize(version)))
    }
    pub fn toolchain_staging(&self) -> PathBuf {
        self.toolchains().join("_staging")
    }
    pub fn bootstrap(&self) -> PathBuf {
        self.toolchains().join("_bootstrap")
    }
    pub fn caches(&self, project_key: &str) -> PathBuf {
        self.root.join("caches").join(project_key)
    }
    pub fn home_project(&self, project_key: &str, name: &str) -> PathBuf {
        self.root.join("homes").join("p").join(project_key).join(name)
    }
    pub fn home_job(&self, job: i64, name: &str) -> PathBuf {
        self.root.join("homes").join(format!("j{job}")).join(name)
    }
    pub fn homes_job_dir(&self, job: i64) -> PathBuf {
        self.root.join("homes").join(format!("j{job}"))
    }
    pub fn artifacts(&self) -> PathBuf {
        self.root.join("artifacts")
    }
    pub fn job_artifacts(&self, job: i64) -> PathBuf {
        self.artifacts().join(format!("j{job}"))
    }
    pub fn logs(&self) -> PathBuf {
        self.root.join("logs")
    }
    pub fn job_logs(&self, job: i64) -> PathBuf {
        self.logs().join(format!("j{job}"))
    }
    pub fn step_log_dir(&self, job: i64, step: &str) -> PathBuf {
        self.job_logs(job).join(step)
    }
    pub fn step_log(&self, job: i64, step: &str) -> PathBuf {
        self.step_log_dir(job, step).join("log.txt")
    }
    pub fn step_out(&self, job: i64, step: &str) -> PathBuf {
        self.step_log_dir(job, step).join("out")
    }
    pub fn service_logs(&self) -> PathBuf {
        self.logs().join("cimon")
    }
    pub fn tmp_job(&self, job: i64) -> PathBuf {
        self.root.join("tmp").join(format!("j{job}"))
    }
    pub fn backup(&self) -> PathBuf {
        self.root.join("backup")
    }

    /// Every directory `cimon setup` creates.
    pub fn all_dirs(&self) -> Vec<PathBuf> {
        vec![
            self.state_dir(),
            self.config_dir(),
            self.recipe_checkouts(),
            self.mirrors(),
            self.ws(),
            self.toolchains(),
            self.toolchain_staging(),
            self.bootstrap(),
            self.root.join("caches"),
            self.root.join("homes"),
            self.artifacts(),
            self.logs(),
            self.service_logs(),
            self.root.join("tmp"),
            self.backup(),
        ]
    }
}

/// Keeps a path component free of separators and characters Windows rejects.
pub fn sanitize(s: &str) -> String {
    s.chars()
        .map(|c| if c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_' | '+' | '@') { c } else { '_' })
        .collect()
}

/// True when `child` is `root` or lies under it, compared component-wise after
/// lexical normalisation. Used to keep recipe paths inside the work tree.
pub fn is_within(root: &Path, child: &Path) -> bool {
    let norm = |p: &Path| -> Vec<String> {
        let mut out: Vec<String> = Vec::new();
        for c in p.components() {
            match c {
                std::path::Component::ParentDir => {
                    out.pop();
                }
                std::path::Component::CurDir => {}
                other => out.push(other.as_os_str().to_string_lossy().to_lowercase()),
            }
        }
        out
    };
    let r = norm(root);
    let c = norm(child);
    c.len() >= r.len() && c[..r.len()] == r[..]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toolchain_dir_uses_short_hash() {
        let l = Layout::new("D:\\cimon");
        let d = l.toolchain_dir("jdk", "17.0.19+10", "abcdef0123456789");
        assert!(d.ends_with("jdk\\17.0.19+10-abcdef01") || d.ends_with("jdk/17.0.19+10-abcdef01"));
    }

    #[test]
    fn within() {
        assert!(is_within(Path::new("C:/ws/a"), Path::new("C:/ws/a/b/../c")));
        assert!(!is_within(Path::new("C:/ws/a"), Path::new("C:/ws/a/../b")));
    }
}
