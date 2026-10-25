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

    /// Writes all files under `dir` (used by export and recipe checkouts).
    pub fn write_to(&self, dir: &Path) -> std::io::Result<()> {
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

/// A relative path with no `..`, no root, no drive and no empty components.
pub fn is_safe_rel(p: &str) -> bool {
    if p.is_empty() || p.starts_with('/') || p.starts_with('\\') || p.contains(':') {
        return false;
    }
    p.split(['/', '\\']).all(|c| !c.is_empty() && c != ".." && c != ".")
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
