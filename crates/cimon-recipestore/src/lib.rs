//! The recipe repository inside the build machine (design §4.1, §6.4–6.5): a bare git
//! repository read and written through the git CLI. Git refs are the source of truth
//! for branch heads and `main`; the job store keeps proposal metadata only.

pub mod git;

use cimon_core::now;
use cimon_recipe::check::{check_tree, Diag, Report};
use cimon_recipe::tree::{is_safe_rel, MemTree, Tree};
use cimon_store::Store;
use git::{Git, GitError};
use serde::Serialize;
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

#[derive(Debug, Clone)]
pub struct RsError {
    pub code: &'static str,
    pub message: String,
    pub diags: Vec<Diag>,
}

impl RsError {
    pub fn new(code: &'static str, message: impl Into<String>) -> Self {
        RsError { code, message: message.into(), diags: vec![] }
    }
}

impl std::fmt::Display for RsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "[{}] {}", self.code, self.message)?;
        for d in &self.diags {
            write!(f, "\n  {d}")?;
        }
        Ok(())
    }
}

impl From<GitError> for RsError {
    fn from(e: GitError) -> Self {
        RsError::new("git_error", e.to_string())
    }
}

impl From<cimon_store::StoreError> for RsError {
    fn from(e: cimon_store::StoreError) -> Self {
        RsError::new("store_error", e.to_string())
    }
}

pub type Result<T> = std::result::Result<T, RsError>;

/// One file change for `edit`.
#[derive(Debug, Clone, serde::Deserialize, Serialize)]
pub struct Change {
    pub path: String,
    #[serde(default)]
    pub content: Option<String>,
    #[serde(default)]
    pub delete: bool,
    /// Raw bytes (`recipe import` of a non-UTF-8 file); wins over `content`.
    #[serde(skip)]
    pub bytes: Option<Vec<u8>>,
}

#[derive(Debug, Clone, Serialize)]
pub struct CommitInfo {
    pub commit: String,
    pub author: String,
    pub date: String,
    pub subject: String,
    pub body: String,
}

#[derive(Debug, Clone, Serialize)]
pub struct EditResult {
    pub branch: String,
    pub commit: String,
    pub proposal_id: i64,
    pub proposal_created: bool,
    pub warnings: Vec<Diag>,
}

#[derive(Debug, Clone, Serialize)]
pub struct ConflictFile {
    pub path: String,
    pub base: Option<String>,
    pub main: Option<String>,
    pub branch: Option<String>,
}

#[derive(Debug, Clone, Serialize)]
pub enum SyncOutcome {
    /// `main` was already in the branch.
    UpToDate { head: String },
    Merged { commit: String },
    /// `main` is the commit the conflicts were computed against — resolutions must come
    /// back with it, so they are never applied to a different `main`.
    Conflicts { main: String, files: Vec<ConflictFile> },
}

pub struct RecipeStore {
    pub git: Git,
    pub main: String,
    pub prefix: String,
    write: Mutex<()>,
    cache: Mutex<HashMap<String, Arc<MemTree>>>,
}

pub fn author_env(user: &str) -> Vec<(&'static str, String)> {
    let email = format!("{user}@cimon.local");
    vec![
        ("GIT_AUTHOR_NAME", user.to_string()),
        ("GIT_AUTHOR_EMAIL", email.clone()),
        ("GIT_COMMITTER_NAME", user.to_string()),
        ("GIT_COMMITTER_EMAIL", email),
    ]
}

impl RecipeStore {
    /// Opens (creating an empty bare repository if needed) the store at `path`.
    pub fn open(path: &Path, main: &str, prefix: &str) -> Result<RecipeStore> {
        let git = if path.join("HEAD").exists() { Git::new(path) } else { Git::init_bare(path)? };
        Ok(RecipeStore { git, main: main.to_string(), prefix: prefix.to_string(), write: Mutex::new(()), cache: Mutex::new(HashMap::new()) })
    }

    pub fn path(&self) -> &Path {
        &self.git.git_dir
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, ()> {
        self.write.lock().unwrap_or_else(|p| p.into_inner())
    }

    pub fn main_ref(&self) -> String {
        format!("refs/heads/{}", self.main)
    }

    pub fn main_commit(&self) -> Option<String> {
        self.git.ref_value(&self.main_ref())
    }

    pub fn require_main(&self) -> Result<String> {
        self.main_commit().ok_or_else(|| RsError::new("no_recipes", "the recipe repository is empty — an admin runs `cimon recipe import --init`"))
    }

    pub fn branch_head(&self, branch: &str) -> Option<String> {
        self.git.ref_value(&format!("refs/heads/{branch}"))
    }

    /// A caller's pin (`base`, `head`): a commit id only. A symbolic name would always
    /// match the current head and protect nothing.
    fn pin(&self, s: &str, what: &str) -> Result<String> {
        let s = s.trim();
        if !(7..=40).contains(&s.len()) || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(RsError::new("invalid_param", format!("{what} must be a commit id (hex), got {s:?}")));
        }
        self.git.rev(s).ok_or_else(|| RsError::new("recipe_conflict", format!("{what} {s} is not a commit in the recipe repository")))
    }

    /// Resolves a caller-supplied recipe ref: `main`, a `recipe/*` branch, or a commit.
    pub fn resolve(&self, r: &str) -> Result<String> {
        let r = r.trim();
        if r.is_empty() || r == self.main {
            return self.require_main();
        }
        if r.starts_with(&self.prefix) {
            return self.branch_head(r).ok_or_else(|| {
                RsError::new("unknown_recipe_ref", format!("{r} does not exist (promoted and closed proposals delete their branch) — see recipe_proposals"))
            });
        }
        if r.len() >= 7 && r.len() <= 40 && r.bytes().all(|b| b.is_ascii_hexdigit()) {
            return self.git.rev(r).ok_or_else(|| RsError::new("unknown_recipe_ref", format!("no commit {r} in the recipe repository")));
        }
        Err(RsError::new("trial_ref_not_allowed", format!("{r:?} is neither {}, a {}* branch nor a commit", self.main, self.prefix)))
    }

    /// Checks a branch name for `recipe_edit` and friends.
    pub fn validate_branch(&self, branch: &str) -> Result<()> {
        if branch == self.main || branch == format!("refs/heads/{}", self.main) {
            return Err(RsError::new("main_not_editable", format!("{} is never edited directly — edit a {}<설명> branch and promote it", self.main, self.prefix)));
        }
        if !branch.starts_with(&self.prefix) || branch.len() <= self.prefix.len() {
            return Err(RsError::new("trial_ref_not_allowed", format!("branch must start with {}", self.prefix)));
        }
        let ok = self.git.run_allow(&["check-ref-format", &format!("refs/heads/{branch}")], &[1], &[]).map(|o| o.code == Some(0)).unwrap_or(false);
        if !ok || branch.contains("..") || branch.chars().any(|c| c.is_whitespace() || c == '\\') {
            return Err(RsError::new("trial_ref_not_allowed", format!("{branch:?} is not a valid branch name")));
        }
        // why: loose ref files live on NTFS, where recipe/Foo and recipe/foo are one file
        if branch.chars().any(char::is_uppercase) {
            return Err(RsError::new("trial_ref_not_allowed", format!("{branch:?}: branch names are lower-case only (Windows ref files ignore case)")));
        }
        if branch.split('/').any(|c| c.ends_with('.') || !cimon_recipe::tree::is_safe_rel(c)) {
            return Err(RsError::new("trial_ref_not_allowed", format!("{branch:?} is not a valid branch name on Windows")));
        }
        Ok(())
    }

    /// The tree at `commit`, cached in memory per commit (commits never change).
    pub fn tree(&self, commit: &str) -> Result<Arc<MemTree>> {
        if let Some(t) = self.cache.lock().unwrap().get(commit) {
            return Ok(t.clone());
        }
        let entries = self.git.ls_tree(commit)?;
        let ids: Vec<String> = entries.iter().map(|(_, id)| id.clone()).collect();
        let blobs = self.git.cat_blobs(&ids)?;
        let mut t = MemTree::new();
        for ((path, _), data) in entries.into_iter().zip(blobs) {
            t.files.insert(path, data);
        }
        let t = Arc::new(t);
        let mut cache = self.cache.lock().unwrap();
        if cache.len() > 64 {
            cache.clear();
        }
        cache.insert(commit.to_string(), t.clone());
        Ok(t)
    }

    /// Writes a full commit for `tree` with `parents`.
    fn commit_tree(&self, tree: &MemTree, parents: &[&str], message: &str, author: &str) -> Result<String> {
        let dir = tempfile::tempdir().map_err(|e| RsError::new("io", e.to_string()))?;
        let index = dir.path().join("index");
        let index_s = index.to_string_lossy().to_string();
        if let Some((a, b)) = cimon_recipe::tree::case_collision(tree.files.keys().map(|s| s.as_str())) {
            return Err(RsError::new("recipe_invalid", format!("{a} and {b} are the same path on Windows — rename one")));
        }
        let mut info = String::new();
        for (path, data) in &tree.files {
            if !is_safe_rel(path) {
                return Err(RsError::new("recipe_invalid", format!("unsafe path {path:?}")));
            }
            let id = self.git.hash_object(data)?;
            info.push_str(&format!("100644 {id}\t{path}\n"));
        }
        let env = [("GIT_INDEX_FILE", index_s.as_str())];
        self.git.run_with(&["read-tree", "--empty"], None, &env)?;
        self.git.run_with(&["update-index", "--index-info"], Some(info.as_bytes()), &env)?;
        let tree_id = String::from_utf8_lossy(&self.git.run_with(&["write-tree"], None, &env)?).trim().to_string();
        let mut args: Vec<String> = vec!["commit-tree".into(), tree_id];
        for p in parents {
            args.push("-p".into());
            args.push(p.to_string());
        }
        args.push("-m".into());
        args.push(message.to_string());
        let aenv = author_env(author);
        let aenv: Vec<(&str, &str)> = aenv.iter().map(|(k, v)| (*k, v.as_str())).collect();
        let argv: Vec<&str> = args.iter().map(|s| s.as_str()).collect();
        Ok(String::from_utf8_lossy(&self.git.run_with(&argv, None, &aenv)?).trim().to_string())
    }

    /// `cimon recipe import --init`: creates `main` once from a checked tree.
    pub fn import_init(&self, tree: &MemTree, author: &str, message: &str) -> Result<String> {
        let _g = self.lock();
        if self.main_commit().is_some() {
            return Err(RsError::new("main_exists", format!("{} already exists — import into a {}* branch instead", self.main, self.prefix)));
        }
        let report = check_tree(tree);
        if !report.ok() {
            let mut e = RsError::new("recipe_invalid", format!("{} error(s) — nothing was imported", report.errors.len()));
            e.diags = report.errors;
            return Err(e);
        }
        let commit = self.commit_tree(tree, &[], message, author)?;
        self.git.update_ref(&self.main_ref(), &commit, None, "cimon recipe import --init")?;
        Ok(commit)
    }

    /// Applies `changes` to the tree at `base`.
    pub fn apply(&self, base: &MemTree, changes: &[Change]) -> Result<MemTree> {
        let mut t = base.clone();
        if changes.is_empty() {
            return Err(RsError::new("recipe_invalid", "changes is empty"));
        }
        for c in changes {
            let path = cimon_recipe::tree::normalize(&c.path);
            if !is_safe_rel(&path) {
                return Err(RsError::new(
                    "recipe_invalid",
                    format!("path {:?} must be relative inside the repository and valid on Windows (no trailing dot or space, no device names like NUL)", c.path),
                ));
            }
            if c.delete {
                if t.files.remove(&path).is_none() {
                    return Err(RsError::new("recipe_invalid", format!("{path} does not exist — nothing to delete")));
                }
            } else {
                let data = match (&c.bytes, &c.content) {
                    (Some(b), _) => b.clone(),
                    (None, Some(s)) => s.clone().into_bytes(),
                    (None, None) => return Err(RsError::new("recipe_invalid", format!("{path}: give content or delete = true"))),
                };
                t.files.insert(path, data);
            }
        }
        if let Some((a, b)) = cimon_recipe::tree::case_collision(t.files.keys().map(|s| s.as_str())) {
            return Err(RsError::new("recipe_invalid", format!("{a} and {b} are the same path on Windows — a checkout would keep only one")));
        }
        Ok(t)
    }

    /// `recipe_edit`: one commit with every change, checked before it is stored.
    pub fn edit(&self, store: &Store, user: &str, branch: &str, base: &str, message: &str, changes: &[Change]) -> Result<EditResult> {
        self.validate_branch(branch)?;
        if message.trim().is_empty() {
            return Err(RsError::new("recipe_invalid", "message is required"));
        }
        let _g = self.lock();
        let main = self.require_main()?;
        let refname = format!("refs/heads/{branch}");
        let current = self.git.ref_value(&refname);
        let base = self.pin(base, "base")?;
        match &current {
            None if base != main => {
                return Err(RsError::new(
                    "recipe_conflict",
                    format!("{branch} is new, so base must be the current {} ({}) — {} moved; read it again", self.main, short(&main), self.main),
                ))
            }
            Some(head) if *head != base => {
                return Err(RsError::new("recipe_conflict", format!("{branch} moved to {} since {} — read it again and redo the change", short(head), short(&base))))
            }
            _ => {}
        }
        let base_tree = self.tree(&base)?;
        let new_tree = self.apply(&base_tree, changes)?;
        if *base_tree == new_tree {
            return Err(RsError::new("recipe_invalid", "the changes leave every file as it was"));
        }
        let warnings = gate(&base_tree, &new_tree)?;
        let commit = self.commit_tree(&new_tree, &[&base], message, user)?;
        self.git.update_ref(&refname, &commit, current.as_deref(), &format!("recipe_edit by {user}"))?;
        let (proposal_id, created) = match store.open_proposal_for_branch(branch)? {
            Some(p) => {
                store.touch_proposal(p.id)?;
                (p.id, false)
            }
            None => (store.create_proposal(branch, first_line(message), user, &main)?, true),
        };
        Ok(EditResult { branch: branch.to_string(), commit, proposal_id, proposal_created: created, warnings })
    }

    /// `cimon recipe import <dir> --branch`: the branch becomes exactly `tree`.
    pub fn import_branch(&self, store: &Store, user: &str, branch: &str, tree: &MemTree, message: &str) -> Result<EditResult> {
        let main = self.require_main()?;
        let base = self.branch_head(branch).unwrap_or(main);
        let base_tree = self.tree(&base)?;
        let mut changes: Vec<Change> = tree
            .files
            .iter()
            .filter(|(p, d)| base_tree.read(p) != Some(d.as_slice()))
            .map(|(p, d)| Change { path: p.clone(), content: None, delete: false, bytes: Some(d.clone()) })
            .collect();
        for p in base_tree.files.keys() {
            if !tree.files.contains_key(p) {
                changes.push(Change { path: p.clone(), content: None, delete: true, bytes: None });
            }
        }
        self.edit(store, user, branch, &base, message, &changes)
    }

    pub fn show(&self, commit: &str, path: &str) -> Result<ShowResult> {
        let t = self.tree(commit)?;
        let path = cimon_recipe::tree::normalize(path).trim_end_matches('/').to_string();
        if let Some(d) = t.read(&path) {
            return Ok(ShowResult::File { path, content: String::from_utf8_lossy(d).into_owned() });
        }
        let prefix = if path.is_empty() { String::new() } else { format!("{path}/") };
        let mut entries: Vec<String> = t
            .files
            .keys()
            .filter_map(|f| f.strip_prefix(&prefix).map(|rest| match rest.split_once('/') {
                Some((dir, _)) => format!("{dir}/"),
                None => rest.to_string(),
            }))
            .collect();
        entries.dedup();
        if entries.is_empty() {
            return Err(RsError::new("not_found", format!("{path} does not exist at {}", short(commit))));
        }
        Ok(ShowResult::Dir { path, entries })
    }

    pub fn history(&self, rev: &str, path: Option<&str>, limit: usize) -> Result<Vec<CommitInfo>> {
        let fmt = "--format=%H%x1f%an%x1f%aI%x1f%s%x1f%b%x1e";
        let n = format!("-n{}", limit.clamp(1, 200));
        let mut args = vec!["log", fmt, n.as_str(), rev];
        if let Some(p) = path {
            args.push("--");
            args.push(p);
        }
        let out = String::from_utf8_lossy(&self.git.run(&args)?).into_owned();
        Ok(out
            .split('\u{1e}')
            .filter_map(|rec| {
                let rec = rec.trim_start_matches('\n');
                let f: Vec<&str> = rec.split('\u{1f}').collect();
                (f.len() >= 5).then(|| CommitInfo {
                    commit: f[0].into(),
                    author: f[1].into(),
                    date: f[2].into(),
                    subject: f[3].into(),
                    body: f[4].trim().into(),
                })
            })
            .collect())
    }

    /// Unified diff text between two commits (optionally one path).
    pub fn diff_text(&self, a: &str, b: &str, path: Option<&str>) -> Result<String> {
        let mut args = vec!["diff", "--no-color", "--no-ext-diff", a, b];
        if let Some(p) = path {
            args.push("--");
            args.push(p);
        }
        Ok(String::from_utf8_lossy(&self.git.run(&args)?).into_owned())
    }

    /// `recipe/*` branches with their heads.
    pub fn branches(&self) -> Result<Vec<(String, String)>> {
        let out = self.git.run_str(&["for-each-ref", "--format=%(refname:short) %(objectname)", &format!("refs/heads/{}", self.prefix)])?;
        Ok(out.lines().filter_map(|l| l.split_once(' ').map(|(a, b)| (a.to_string(), b.to_string()))).collect())
    }

    /// `recipe_sync`: merges `main` into the branch. Conflicts come back with the three
    /// versions of each file; resolved contents produce the merge commit.
    pub fn sync(&self, store: &Store, user: &str, branch: &str, head: &str, main_at: Option<&str>, resolutions: &HashMap<String, String>) -> Result<SyncOutcome> {
        self.validate_branch(branch)?;
        let _g = self.lock();
        let main = self.require_main()?;
        let refname = format!("refs/heads/{branch}");
        let current = self.git.ref_value(&refname).ok_or_else(|| RsError::new("unknown_recipe_ref", format!("{branch} does not exist")))?;
        let head = self.pin(head, "head")?;
        if current != head {
            return Err(RsError::new("recipe_conflict", format!("{branch} moved to {} — read it again", short(&current))));
        }
        if !resolutions.is_empty() {
            let at = main_at.ok_or_else(|| RsError::new("invalid_param", "resolutions need main — the main commit the conflicts were reported against"))?;
            if self.pin(at, "main")? != main {
                return Err(RsError::new("recipe_conflict", format!("{} moved to {} since the conflicts were reported — sync again and resolve against it", self.main, short(&main))));
            }
        }
        if self.git.is_ancestor(&main, &current) {
            return Ok(SyncOutcome::UpToDate { head: current });
        }
        let out = self.git.run_allow(&["merge-tree", "--write-tree", "-z", "--name-only", "--no-messages", &current, &main], &[1], &[])?;
        let mut tokens = out.stdout.split(|b| *b == 0).map(|t| String::from_utf8_lossy(t).into_owned());
        let tree_id = tokens.next().unwrap_or_default();
        let conflicted: Vec<String> = if out.code == Some(1) { tokens.take_while(|t| !t.is_empty()).collect() } else { vec![] };
        let base = self.git.merge_base(&current, &main);
        let mut merged = MemTree::new();
        {
            let entries = self.git.ls_tree(&tree_id)?;
            let ids: Vec<String> = entries.iter().map(|(_, id)| id.clone()).collect();
            for ((p, _), d) in entries.into_iter().zip(self.git.cat_blobs(&ids)?) {
                merged.files.insert(p, d);
            }
        }
        if !conflicted.is_empty() {
            let missing: Vec<&String> = conflicted.iter().filter(|p| !resolutions.contains_key(*p)).collect();
            if !missing.is_empty() {
                let read = |c: Option<&str>, p: &str| -> Option<String> {
                    c.and_then(|c| self.tree(c).ok()).and_then(|t| t.read(p).map(|d| String::from_utf8_lossy(d).into_owned()))
                };
                let files = conflicted
                    .iter()
                    .map(|p| ConflictFile {
                        path: p.clone(),
                        base: read(base.as_deref(), p),
                        main: read(Some(&main), p),
                        branch: read(Some(&current), p),
                    })
                    .collect();
                return Ok(SyncOutcome::Conflicts { main: main.clone(), files });
            }
            for p in &conflicted {
                let c = &resolutions[p];
                merged.files.insert(p.clone(), c.clone().into_bytes());
            }
        }
        for (p, c) in resolutions {
            if !conflicted.contains(p) {
                return Err(RsError::new("recipe_invalid", format!("{p} is not a conflicted file — resolutions only cover conflicts")));
            }
            let _ = c;
        }
        let main_tree = self.tree(&main)?;
        let warnings = gate(&main_tree, &merged)?;
        let _ = warnings;
        let commit = self.commit_tree(&merged, &[&current, &main], &format!("{} 을 {branch} 에 합침", self.main), user)?;
        self.git.update_ref(&refname, &commit, Some(&current), &format!("recipe_sync by {user}"))?;
        if let Some(p) = store.open_proposal_for_branch(branch)? {
            store.set_proposal_base(p.id, &main)?;
        }
        Ok(SyncOutcome::Merged { commit })
    }

    /// `recipe_promote`: fast-forwards `main` to `head` after `conditions` (evaluated
    /// under the write lock, against the trees as they are now) accept it.
    pub fn promote(
        &self,
        store: &Store,
        user: &str,
        proposal_id: i64,
        head: &str,
        override_reason: Option<&str>,
        conditions: impl FnOnce(&str, &str) -> Result<()>,
    ) -> Result<String> {
        let _g = self.lock();
        let p = store.proposal(proposal_id)?.ok_or_else(|| RsError::new("not_found", format!("no proposal {}", cimon_core::proposal_label(proposal_id))))?;
        if p.status != "open" {
            return Err(RsError::new("proposal_closed", format!("{} is {} — editing the same branch opens a new proposal", cimon_core::proposal_label(p.id), p.status)));
        }
        let refname = format!("refs/heads/{}", p.branch);
        let current = self.git.ref_value(&refname).ok_or_else(|| RsError::new("unknown_recipe_ref", format!("{} no longer exists", p.branch)))?;
        let head_full = self.pin(head, "head")?;
        if current != head_full {
            return Err(RsError::new("recipe_conflict", format!("the proposal changed to {} since you looked at {} — look again", short(&current), short(head))));
        }
        let main = self.require_main()?;
        if !self.git.is_ancestor(&main, &current) {
            return Err(RsError::new("not_fast_forward", format!("{} moved to {} — recipe_sync, run the trials again, then promote", self.main, short(&main))));
        }
        conditions(&main, &current)?;
        // Order matters for crash recovery (see `reconcile`): who promoted and why is
        // written first, then main moves, then the proposal is marked, then the branch
        // goes. Once main has moved the promote has happened — later failures are left
        // to `reconcile`, which keeps the recorded promoter and reason.
        store.set_promote_intent(p.id, &current, user, override_reason)?;
        if let Err(e) = self.git.update_ref(&self.main_ref(), &current, Some(&main), &format!("recipe_promote {} by {user}", cimon_core::proposal_label(p.id))) {
            let _ = store.clear_promote_intent(p.id);
            return Err(e.into());
        }
        let _ = store.mark_promoted(p.id, &current, user, override_reason);
        let _ = self.git.delete_ref(&refname, &current);
        Ok(current)
    }

    /// `recipe_close`: keeps the commits under `refs/cimon/closed/<id>`, deletes the branch.
    pub fn close(&self, store: &Store, user: &str, proposal_id: i64, reason: Option<&str>) -> Result<()> {
        let _g = self.lock();
        let p = store.proposal(proposal_id)?.ok_or_else(|| RsError::new("not_found", format!("no proposal {}", cimon_core::proposal_label(proposal_id))))?;
        if p.status != "open" {
            return Err(RsError::new("proposal_closed", format!("{} is already {}", cimon_core::proposal_label(p.id), p.status)));
        }
        let refname = format!("refs/heads/{}", p.branch);
        if let Some(head) = self.git.ref_value(&refname) {
            // the commits stay reachable from the closed ref, or the branch is not deleted
            let keep = format!("refs/cimon/closed/{}", cimon_core::proposal_label(p.id));
            self.git.run(&["update-ref", "-m", "recipe_close", &keep, &head])?;
            self.git.delete_ref(&refname, &head)?;
        }
        store.mark_closed(p.id, user, reason)?;
        Ok(())
    }

    /// `recipe_revert`: a commit on `main` undoing `commit`, checked like any edit.
    pub fn revert(&self, user: &str, commit: &str, reason: &str) -> Result<String> {
        if reason.trim().len() < 5 {
            return Err(RsError::new("recipe_invalid", "a revert needs a reason"));
        }
        let _g = self.lock();
        let main = self.require_main()?;
        let target = self.git.rev(commit).ok_or_else(|| RsError::new("not_found", format!("no commit {commit}")))?;
        if !self.git.is_ancestor(&target, &main) {
            return Err(RsError::new("recipe_invalid", format!("{} is not in {}'s history", short(&target), self.main)));
        }
        if self.git.rev(&format!("{target}^2")).is_some() {
            return Err(RsError::new(
                "recipe_invalid",
                format!("{} is a merge (recipe_sync) commit — reverting it would undo what was merged in from {}; revert the proposal's own commits instead", short(&target), self.main),
            ));
        }
        let parent = self.git.rev(&format!("{target}^1")).ok_or_else(|| RsError::new("recipe_invalid", "the first commit cannot be reverted"))?;
        let out = self.git.run_allow(
            &["merge-tree", "--write-tree", "-z", "--name-only", "--no-messages", &format!("--merge-base={target}"), &main, &parent],
            &[1],
            &[],
        )?;
        let mut tokens = out.stdout.split(|b| *b == 0).map(|t| String::from_utf8_lossy(t).into_owned());
        let tree_id = tokens.next().unwrap_or_default();
        if out.code == Some(1) {
            let files: Vec<String> = tokens.take_while(|t| !t.is_empty()).collect();
            return Err(RsError::new("recipe_conflict", format!("reverting {} conflicts with later changes in: {}", short(&target), files.join(", "))));
        }
        let mut reverted = MemTree::new();
        let entries = self.git.ls_tree(&tree_id)?;
        let ids: Vec<String> = entries.iter().map(|(_, id)| id.clone()).collect();
        for ((p, _), d) in entries.into_iter().zip(self.git.cat_blobs(&ids)?) {
            reverted.files.insert(p, d);
        }
        let main_tree = self.tree(&main)?;
        if *main_tree == reverted {
            return Err(RsError::new("recipe_invalid", format!("{}'s changes are already gone from {} — nothing to revert", short(&target), self.main)));
        }
        gate(&main_tree, &reverted)?;
        let subject = self.git.run_str(&["log", "-1", "--format=%s", &target])?;
        let msg = format!("되돌리기: {subject}\n\n되돌린 커밋 {target}\n사유: {reason}");
        let new = self.commit_tree(&reverted, &[&main], &msg, user)?;
        self.git.update_ref(&self.main_ref(), &new, Some(&main), &format!("recipe_revert by {user}"))?;
        Ok(new)
    }

    /// Startup reconciliation (design §6.5). Promote moves main, then records the
    /// proposal, then deletes the branch, so after a crash:
    /// - an open proposal whose branch head is already in `main` was promoted;
    /// - an open proposal without a branch is closed ("브랜치 없음");
    /// - a branch with no open proposal whose head is in `main` is a leftover of a
    ///   finished promote and is deleted.
    pub fn reconcile(&self, store: &Store) -> Result<Vec<String>> {
        let _g = self.lock();
        let mut notes = vec![];
        let main = self.main_commit();
        for p in store.proposals(Some("open"), 10_000)? {
            match self.branch_head(&p.branch) {
                // every edit adds a commit on top of its base, so a branch head inside
                // main only gets there through a promote
                Some(head) if head != p.base_commit && main.as_deref().is_some_and(|m| self.git.is_ancestor(&head, m)) => {
                    store.mark_promoted(p.id, &head, "cimon(reconcile)", None)?;
                    let _ = self.git.delete_ref(&format!("refs/heads/{}", p.branch), &head);
                    notes.push(format!("{} marked promoted ({})", cimon_core::proposal_label(p.id), short(&head)));
                }
                Some(_) => {}
                None => {
                    store.mark_closed(p.id, "cimon(reconcile)", Some("브랜치 없음"))?;
                    notes.push(format!("{} closed — its branch is gone", cimon_core::proposal_label(p.id)));
                }
            }
        }
        if let Some(m) = &main {
            for (branch, head) in self.branches()? {
                if store.open_proposal_for_branch(&branch)?.is_none() && self.git.is_ancestor(&head, m) {
                    let _ = self.git.delete_ref(&format!("refs/heads/{branch}"), &head);
                    notes.push(format!("deleted leftover branch {branch} (already in {})", self.main));
                }
            }
        }
        Ok(notes)
    }

    /// `git bundle create <dest> --all` (design §16.5).
    pub fn bundle(&self, dest: &Path) -> Result<()> {
        if let Some(p) = dest.parent() {
            std::fs::create_dir_all(p).map_err(|e| RsError::new("io", e.to_string()))?;
        }
        let _g = self.lock();
        self.git.run(&["bundle", "create", &dest.to_string_lossy(), "--all"])?;
        Ok(())
    }

    /// Rebuilds a store at `path` from a bundle. Refuses to overwrite an existing store.
    pub fn restore(bundle: &Path, path: &Path) -> Result<()> {
        if path.join("HEAD").exists() {
            return Err(RsError::new("exists", format!("{} already holds a repository — move it away first", path.display())));
        }
        if std::fs::read_dir(path).is_ok_and(|mut d| d.next().is_some()) {
            return Err(RsError::new("exists", format!("{} is not empty — restore into an empty or new directory", path.display())));
        }
        let res = (|| -> Result<()> {
            let git = Git::init_bare(path)?;
            git.run(&["bundle", "verify", &bundle.to_string_lossy()])?;
            git.run(&["fetch", "--quiet", &bundle.to_string_lossy(), "+refs/*:refs/*"])?;
            Ok(())
        })();
        if res.is_err() {
            // a half-made repository would make the retry fail with "exists"
            let _ = std::fs::remove_dir_all(path);
        }
        res
    }

    /// Writes the tree of `commit` to `dir` once (recipe checkout for `${recipes}`).
    pub fn checkout(&self, commit: &str, dir: &Path) -> Result<PathBuf> {
        if dir.join(".cimon-complete").exists() {
            return Ok(dir.to_path_buf());
        }
        let t = self.tree(commit)?;
        let tmp = dir.with_extension(format!("tmp{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        t.write_to(&tmp).map_err(|e| RsError::new("io", e.to_string()))?;
        std::fs::write(tmp.join(".cimon-complete"), now()).map_err(|e| RsError::new("io", e.to_string()))?;
        if std::fs::rename(&tmp, dir).is_err() {
            let _ = std::fs::remove_dir_all(&tmp);
            if !dir.join(".cimon-complete").exists() {
                return Err(RsError::new("io", format!("could not create {}", dir.display())));
            }
        }
        Ok(dir.to_path_buf())
    }

    /// Drops `refs/cimon/closed/<label>` (janitor, 30 days after closing).
    pub fn drop_closed(&self, label: &str) -> Result<()> {
        let refname = format!("refs/cimon/closed/{label}");
        if let Some(v) = self.git.ref_value(&refname) {
            self.git.delete_ref(&refname, &v)?;
        }
        Ok(())
    }

    pub fn fsck(&self) -> Result<()> {
        self.git.run(&["fsck", "--no-progress", "--no-dangling"])?;
        Ok(())
    }
}

#[derive(Debug, Clone, Serialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum ShowResult {
    File { path: String, content: String },
    Dir { path: String, entries: Vec<String> },
}

/// The pre-save check: errors the change introduced block it; errors that were already
/// there come back as warnings, together with new warnings.
pub fn gate(base: &MemTree, new: &MemTree) -> Result<Vec<Diag>> {
    let before = check_tree(base);
    let after = check_tree(new);
    let new_errors = after.new_errors_since(&before);
    if !new_errors.is_empty() {
        let mut e = RsError::new("recipe_invalid", format!("{} new error(s) — not saved", new_errors.len()));
        e.diags = new_errors;
        return Err(e);
    }
    let mut warnings: Vec<Diag> = after.preexisting_errors(&before).into_iter().map(|mut d| {
        d.severity = cimon_recipe::Severity::Warning;
        d.message = format!("(이미 있던 오류) {}", d.message);
        d
    }).collect();
    let old_w: std::collections::BTreeSet<cimon_recipe::check::DiagKey> = before.warnings.iter().map(|d| d.key()).collect();
    warnings.extend(after.warnings.into_iter().filter(|d| !old_w.contains(&d.key())));
    Ok(warnings)
}

pub fn check_report(t: &MemTree) -> Report {
    check_tree(t)
}

pub fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s).trim()
}

#[cfg(test)]
mod tests;
