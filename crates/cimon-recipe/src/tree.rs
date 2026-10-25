//! A recipe repository snapshot: a set of files keyed by forward-slash paths.

use std::collections::BTreeMap;
use std::path::Path;

pub trait Tree {
    fn read(&self, path: &str) -> Option<&[u8]>;
    fn files(&self) -> Vec<&str>;

    fn read_str(&self, path: &str) -> Option<&str> {
        self.read(path).and_then(|b| std::str::from_utf8(b).ok())
    }
    fn exists(&self, path: &str) -> bool {
        self.read(path).is_some()
    }
    /// Project ids: directories under `projects/` that contain a `project.toml`.
    fn project_ids(&self) -> Vec<String> {
        let mut ids: Vec<String> = self
            .files()
            .into_iter()
            .filter_map(|f| {
                let rest = f.strip_prefix("projects/")?;
                let (id, file) = rest.split_once('/')?;
                (file == "project.toml").then(|| id.to_string())
            })
            .collect();
        ids.sort();
        ids.dedup();
        ids
    }
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MemTree {
    pub files: BTreeMap<String, Vec<u8>>,
}

impl MemTree {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn insert(&mut self, path: impl Into<String>, data: impl Into<Vec<u8>>) {
        self.files.insert(normalize(&path.into()), data.into());
    }

    /// Loads every file under `dir`, skipping `.git`.
    pub fn from_dir(dir: &Path) -> std::io::Result<MemTree> {
        let mut t = MemTree::new();
        fn walk(base: &Path, dir: &Path, t: &mut MemTree) -> std::io::Result<()> {
            for entry in std::fs::read_dir(dir)? {
                let entry = entry?;
                let p = entry.path();
                let name = entry.file_name();
                if name == ".git" {
                    continue;
                }
                let ft = entry.file_type()?;
                if ft.is_dir() {
                    walk(base, &p, t)?;
                } else if ft.is_file() {
                    let rel = p.strip_prefix(base).unwrap().to_string_lossy().replace('\\', "/");
                    t.files.insert(rel, std::fs::read(&p)?);
                }
            }
            Ok(())
        }
        walk(dir, dir, &mut t)?;
        Ok(t)
    }

    /// Writes all files under `dir` (used by export and recipe checkouts). Refuses a
    /// tree whose names collide on a case-insensitive file system — a later file would
    /// silently replace a reviewed one.
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
        if let Some((a, b)) = case_collision(self.files.keys().map(|s| s.as_str())) {
            return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("{a} and {b} are the same path on Windows")));
        }
        for (path, data) in &self.files {
            if !is_safe_rel(path) {
                return Err(std::io::Error::new(std::io::ErrorKind::InvalidInput, format!("unsafe path {path:?}")));
            }
            let p = dir.join(path);
            if let Some(parent) = p.parent() {
                std::fs::create_dir_all(parent)?;
            }
            std::fs::write(&p, data)?;
        }
        Ok(())
    }

    /// sha256 over the sorted (path, content) pairs of `paths` (missing ones skipped).
    pub fn digest(&self, paths: &[String]) -> String {
        use sha2::{Digest, Sha256};
        let mut sorted: Vec<&String> = paths.iter().collect();
        sorted.sort();
        sorted.dedup();
        let mut h = Sha256::new();
        for p in sorted {
            if let Some(d) = self.files.get(p) {
                h.update(p.as_bytes());
                h.update([0]);
                h.update((d.len() as u64).to_le_bytes());
                h.update(d);
            }
        }
        hex::encode(h.finalize())
    }
}

impl Tree for MemTree {
    fn read(&self, path: &str) -> Option<&[u8]> {
        self.files.get(&normalize(path)).map(|v| v.as_slice())
    }
    fn files(&self) -> Vec<&str> {
        self.files.keys().map(|s| s.as_str()).collect()
    }
}

pub fn normalize(p: &str) -> String {
    p.replace('\\', "/").trim_start_matches("./").to_string()
}

/// A relative path with no `..`, no root, no drive and no empty components, that names
/// the same file on Windows as it does in git: no trailing dot or space, no reserved
/// device name, no characters Windows refuses.
pub fn is_safe_rel(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') || p.starts_with('\\') || p.contains(':') {
        return false;
    }
    p.split(['/', '\\']).all(|c| !c.is_empty() && c != ".." && c != "." && portable_component(c))
}

fn portable_component(c: &str) -> bool {
    if c.ends_with('.') || c.ends_with(' ') {
        return false;
    }
    if c.chars().any(|ch| (ch as u32) < 0x20 || matches!(ch, '<' | '>' | '"' | '|' | '?' | '*')) {
        return false;
    }
    let stem = c.split('.').next().unwrap_or(c).trim_end().to_ascii_uppercase();
    let reserved = matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$")
        || ((stem.starts_with("COM") || stem.starts_with("LPT")) && stem.len() == 4 && stem.as_bytes()[3].is_ascii_digit());
    !reserved
}

/// Two paths (or a file and a directory) that are the same on a case-insensitive file
/// system, if any.
pub fn case_collision<'a>(paths: impl Iterator<Item = &'a str>) -> Option<(String, String)> {
    let mut seen: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut dirs: std::collections::HashMap<String, String> = std::collections::HashMap::new();
    let mut files = vec![];
    for p in paths {
        let n = normalize(p);
        let low = n.to_lowercase();
        if let Some(prev) = seen.get(&low) {
            if *prev != n {
                return Some((prev.clone(), n));
            }
        }
        seen.insert(low.clone(), n.clone());
        let mut acc = String::new();
        for comp in n.split('/').collect::<Vec<_>>().split_last().map(|(_, d)| d.to_vec()).unwrap_or_default() {
            if !acc.is_empty() {
                acc.push('/');
            }
            acc.push_str(comp);
            match dirs.get(&acc.to_lowercase()) {
                Some(prev) if *prev != acc => return Some((prev.clone(), acc.clone())),
                _ => {
                    dirs.insert(acc.to_lowercase(), acc.clone());
                }
            }
        }
        files.push((low, n));
    }
    for (low, n) in files {
        if let Some(d) = dirs.get(&low) {
            return Some((n, format!("{d}/")));
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn safe_paths() {
        assert!(is_safe_rel("projects/a/recipe.toml"));
        assert!(!is_safe_rel("../x"));
        assert!(!is_safe_rel("a/../../x"));
        assert!(!is_safe_rel("C:/x"));
        assert!(!is_safe_rel("/x"));
        assert!(!is_safe_rel("a//b"));
        assert!(!is_safe_rel("scripts/judge.py."));
        assert!(!is_safe_rel("scripts/judge.py "));
        assert!(!is_safe_rel("scripts/NUL.txt"));
        assert!(!is_safe_rel("com1/x"));
        assert!(!is_safe_rel("a?b"));
        assert!(is_safe_rel("scripts/console.py"));
        assert!(is_safe_rel("com10/x"));
    }

    #[test]
    fn collisions() {
        assert!(case_collision(["scripts/Run.ps1", "scripts/run.ps1"].into_iter()).is_some());
        assert!(case_collision(["Scripts/a", "scripts/b"].into_iter()).is_some());
        assert!(case_collision(["a", "A/b"].into_iter()).is_some());
        assert!(case_collision(["scripts/a", "scripts/b", "x"].into_iter()).is_none());
        let mut t = MemTree::new();
        t.insert("s/Run.ps1", "a");
        t.insert("s/run.ps1", "b");
        let d = tempfile_dir();
        assert!(t.write_to(&d).is_err());
        let _ = std::fs::remove_dir_all(&d);
    }

    fn tempfile_dir() -> std::path::PathBuf {
        let d = std::env::temp_dir().join(format!("cimon-tree-{}-{}", std::process::id(), line!()));
        let _ = std::fs::remove_dir_all(&d);
        d
    }

    #[test]
    fn project_ids() {
        let mut t = MemTree::new();
        t.insert("projects/b/project.toml", "");
        t.insert("projects/a/project.toml", "");
        t.insert("projects/a/recipe.toml", "");
        t.insert("projects/c/recipe.toml", "");
        assert_eq!(t.project_ids(), vec!["a", "b"]);
    }
}
