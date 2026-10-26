//! Git for build steps (design §11.6). The build account has no credentials. A build
//! that fetches a repository with the git CLI — a cargo `git` dependency with
//! `git-fetch-with-cli`, say — gets:
//!
//! - **a registered project's repository from cimon's mirror** (`mirrors`): no
//!   credentials in the build, no network;
//! - for any other private repository, a token for that repository only, stored by
//!   `cimon build-git-token <url>` DPAPI-encrypted in `config\build-git.dpapi`. Every
//!   build step sees these tokens: give each one read access to its repository and
//!   nothing else.

use cimon_core::Layout;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn file(layout: &Layout) -> PathBuf {
    layout.config_dir().join("build-git.dpapi")
}

/// Repository URL → token. No file: none.
pub fn load(layout: &Layout) -> Result<BTreeMap<String, String>, String> {
    crate::protected::load(&file(layout))
}

pub fn save(layout: &Layout, tokens: &BTreeMap<String, String>) -> Result<(), String> {
    crate::protected::save(&file(layout), tokens, tokens.is_empty())
}

/// A URL a token may be bound to: https, a host, an owner and a repository, and
/// nothing git could read as more than a URL.
pub fn valid_url(url: &str) -> bool {
    let Some(rest) = url.strip_prefix("https://") else { return false };
    let Some((host, path)) = rest.split_once('/') else { return false };
    !host.is_empty()
        && path.split('/').filter(|s| !s.is_empty()).count() >= 2
        && url.chars().all(|c| c.is_ascii_graphic())
        && !url.contains(['?', '#', '@', '"', '\\'])
}

/// (repository URL, mirror) for every project of the recipe tree whose mirror exists.
/// A mirror is as fresh as the last fetch of that project — intake fetches the job's
/// project and its siblings; a dependency newer than the mirror fails with git's
/// "not our ref" naming the mirror.
pub fn mirrors(layout: &Layout, tree: &dyn cimon_recipe::Tree) -> Vec<(String, PathBuf)> {
    let ids: std::collections::BTreeSet<String> = tree
        .files()
        .into_iter()
        .filter_map(|f| f.strip_prefix("projects/").and_then(|r| r.strip_suffix("/project.toml")).filter(|id| !id.contains('/')).map(String::from))
        .collect();
    let mut out = vec![];
    for id in ids {
        let Ok(r) = cimon_recipe::resolve_project(tree, &id) else { continue };
        let m = layout.mirror(&id);
        if m.join("HEAD").exists() {
            out.push((r.project.repo.clone(), m));
        }
    }
    out
}

/// The spellings a build may use for an `https://<host>/<path>` repository: as is, and
/// the ssh forms. Git rewrites a URL once, by the longest matching `insteadOf`, so the
/// ssh forms must be listed themselves (the generic ssh → https rule does not chain).
fn spellings(repo: &str) -> Vec<String> {
    let mut v = vec![repo.to_string()];
    if let Some((host, path)) = repo.strip_prefix("https://").and_then(|r| r.split_once('/')) {
        v.push(format!("ssh://git@{host}/{path}"));
        v.push(format!("git@{host}:{path}"));
    }
    v
}

/// Adds the build steps' git configuration to `env` through `GIT_CONFIG_*` (after any
/// entries a recipe set):
/// - GitHub ssh URLs become https — the machine has no SSH keys;
/// - each registered project's repository URL (in its https and ssh spellings) reads
///   its mirror instead. A fetch by commit (cargo pins a `rev`) asks for an object the
///   mirror does not advertise, so the build's git allows that, and trusts the mirror
///   although the service account owns it (`safe.directory`; the build account cannot
///   write it);
/// - each stored URL gets its token as an HTTP header — git sends an `http.<url>.*`
///   setting only to requests for that URL.
pub fn apply(tokens: &BTreeMap<String, String>, mirrors: &[(String, PathBuf)], env: &mut BTreeMap<String, String>) {
    let mut entries: Vec<(String, String)> = vec![
        ("url.https://github.com/.insteadOf".into(), "ssh://git@github.com/".into()),
        ("url.https://github.com/.insteadOf".into(), "git@github.com:".into()),
    ];
    for (repo, mirror) in mirrors {
        let local = mirror.to_string_lossy().replace('\\', "/");
        for s in spellings(repo) {
            entries.push((format!("url.{local}.insteadOf"), s));
        }
    }
    if !mirrors.is_empty() {
        entries.push(("uploadpack.allowAnySHA1InWant".into(), "true".into()));
        entries.push(("safe.directory".into(), "*".into()));
    }
    for (url, token) in tokens {
        entries.push((format!("http.{url}.extraheader"), format!("AUTHORIZATION: basic {}", cimon_workspace::basic_auth(token))));
    }
    let count_key = env.keys().find(|k| k.eq_ignore_ascii_case("GIT_CONFIG_COUNT")).cloned();
    let start: usize = count_key.as_ref().and_then(|k| env.get(k)).and_then(|v| v.parse().ok()).unwrap_or(0);
    for (i, (k, v)) in entries.iter().enumerate() {
        env.insert(format!("GIT_CONFIG_KEY_{}", start + i), k.clone());
        env.insert(format!("GIT_CONFIG_VALUE_{}", start + i), v.clone());
    }
    cimon_runner::env_set(env, "GIT_CONFIG_COUNT", (start + entries.len()).to_string());
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls() {
        assert!(valid_url("https://github.com/org/repo.git"));
        assert!(valid_url("https://github.com/org/repo"));
        assert!(!valid_url("https://github.com/org"), "an owner is not a repository");
        assert!(!valid_url("http://github.com/org/repo.git"));
        assert!(!valid_url("ssh://git@github.com/org/repo.git"));
        assert!(!valid_url("https://user@github.com/org/repo.git"));
        assert!(!valid_url("https://github.com/org/repo.git?x=1"));
        assert!(!valid_url("https://github.com/org/my repo.git"));
    }

    #[test]
    fn appends_after_a_recipe_and_scopes_the_header() {
        let mut env = BTreeMap::new();
        env.insert("GIT_CONFIG_COUNT".to_string(), "1".to_string());
        env.insert("GIT_CONFIG_KEY_0".to_string(), "core.autocrlf".to_string());
        env.insert("GIT_CONFIG_VALUE_0".to_string(), "false".to_string());
        let mut t = BTreeMap::new();
        t.insert("https://github.com/org/dep.git".to_string(), "abc".to_string());
        apply(&t, &[], &mut env);
        assert_eq!(env["GIT_CONFIG_COUNT"], "4");
        assert_eq!(env["GIT_CONFIG_KEY_0"], "core.autocrlf", "the recipe's entry stays");
        assert_eq!(env["GIT_CONFIG_KEY_1"], "url.https://github.com/.insteadOf");
        assert_eq!(env["GIT_CONFIG_VALUE_1"], "ssh://git@github.com/");
        assert_eq!(env["GIT_CONFIG_KEY_3"], "http.https://github.com/org/dep.git.extraheader");
        // base64("x-access-token:abc")
        assert_eq!(env["GIT_CONFIG_VALUE_3"], "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46YWJj");
        let mut none = BTreeMap::new();
        apply(&BTreeMap::new(), &[], &mut none);
        assert_eq!(none["GIT_CONFIG_COUNT"], "2", "the ssh rewrite applies without tokens");
    }

    /// A cargo-style fetch by commit, through the ssh spelling, lands on the mirror —
    /// with no credentials and nothing on the network.
    #[test]
    fn registered_repository_is_fetched_from_its_mirror() {
        let d = tempfile::tempdir().unwrap();
        let src = d.path().join("src");
        let git = |dir: &std::path::Path, args: &[&str]| {
            let o = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t"]).args(args).output().unwrap();
            assert!(o.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&o.stderr));
            String::from_utf8_lossy(&o.stdout).trim().to_string()
        };
        std::process::Command::new("git").args(["init", "-q", "-b", "main"]).arg(&src).status().unwrap();
        std::fs::write(src.join("lib.rs"), "1").unwrap();
        git(&src, &["add", "-A"]);
        git(&src, &["commit", "-qm", "one"]);
        let pinned = git(&src, &["rev-parse", "HEAD"]);
        std::fs::write(src.join("lib.rs"), "2").unwrap();
        git(&src, &["commit", "-qam", "two"]); // the pinned commit is not a branch head
        let mirror = d.path().join("mirrors").join("dep.git");
        cimon_workspace::Mirror::fetch(&mirror, &src.to_string_lossy().replace('\\', "/")).unwrap();

        let mut env = BTreeMap::new();
        apply(&BTreeMap::new(), &[("https://github.com/org/dep.git".to_string(), mirror)], &mut env);
        let user = d.path().join("user");
        std::process::Command::new("git").args(["init", "-q"]).arg(&user).status().unwrap();
        let o = std::process::Command::new("git")
            .arg("-C")
            .arg(&user)
            .args(["fetch", "--no-tags", "ssh://git@github.com/org/dep.git", &format!("+{pinned}:refs/commit/{pinned}")])
            .envs(&env)
            .env("GIT_TERMINAL_PROMPT", "0")
            .env("GIT_SSH_COMMAND", "exit 1") // any attempt at the network fails loudly
            .output()
            .unwrap();
        assert!(o.status.success(), "fetch by commit from the mirror: {}", String::from_utf8_lossy(&o.stderr));
        assert_eq!(git(&user, &["rev-parse", &format!("refs/commit/{pinned}")]), pinned);
    }
}
