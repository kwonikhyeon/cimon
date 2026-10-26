//! Git credentials for build steps (design §11.6). The build account has none by
//! default; a build that fetches a private repository with the git CLI — a cargo `git`
//! dependency with `git-fetch-with-cli`, say — gets a token for that repository only,
//! stored by `cimon build-git-token <url>` DPAPI-encrypted in `config\build-git.dpapi`.
//! Every build step sees these tokens: give each one read access to its repository and
//! nothing else.

use cimon_core::Layout;
use std::collections::BTreeMap;
use std::path::PathBuf;

pub fn file(layout: &Layout) -> PathBuf {
    layout.config_dir().join("build-git.dpapi")
}

/// Repository URL → token. No file: none.
pub fn load(layout: &Layout) -> Result<BTreeMap<String, String>, String> {
    let p = file(layout);
    let blob = match std::fs::read(&p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(BTreeMap::new()),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    #[cfg(windows)]
    let raw = cimon_runner::win::unprotect(&blob).map_err(|e| format!("{}: {e}", p.display()))?;
    #[cfg(not(windows))]
    let raw = blob;
    serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", p.display()))
}

pub fn save(layout: &Layout, tokens: &BTreeMap<String, String>) -> Result<(), String> {
    let p = file(layout);
    if tokens.is_empty() {
        return match std::fs::remove_file(&p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", p.display())),
            _ => Ok(()),
        };
    }
    let raw = serde_json::to_vec(tokens).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let blob = cimon_runner::win::protect(&raw).map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let blob = raw;
    std::fs::write(&p, blob).map_err(|e| format!("{}: {e}", p.display()))
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

/// Adds the build steps' git configuration to `env` through `GIT_CONFIG_*` (after any
/// entries a recipe set): GitHub ssh URLs become https — the machine has no SSH keys —
/// and each stored URL gets its token as an HTTP header. Git sends an `http.<url>.*`
/// setting only to requests for that URL.
pub fn apply(tokens: &BTreeMap<String, String>, env: &mut BTreeMap<String, String>) {
    let mut entries: Vec<(String, String)> = vec![
        ("url.https://github.com/.insteadOf".into(), "ssh://git@github.com/".into()),
        ("url.https://github.com/.insteadOf".into(), "git@github.com:".into()),
    ];
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
        apply(&t, &mut env);
        assert_eq!(env["GIT_CONFIG_COUNT"], "4");
        assert_eq!(env["GIT_CONFIG_KEY_0"], "core.autocrlf", "the recipe's entry stays");
        assert_eq!(env["GIT_CONFIG_KEY_1"], "url.https://github.com/.insteadOf");
        assert_eq!(env["GIT_CONFIG_VALUE_1"], "ssh://git@github.com/");
        assert_eq!(env["GIT_CONFIG_KEY_3"], "http.https://github.com/org/dep.git.extraheader");
        // base64("x-access-token:abc")
        assert_eq!(env["GIT_CONFIG_VALUE_3"], "AUTHORIZATION: basic eC1hY2Nlc3MtdG9rZW46YWJj");
        let mut none = BTreeMap::new();
        apply(&BTreeMap::new(), &mut none);
        assert_eq!(none["GIT_CONFIG_COUNT"], "2", "the ssh rewrite applies without tokens");
    }
}
