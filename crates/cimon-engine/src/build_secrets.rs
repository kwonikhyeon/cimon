//! Secrets for build steps (design §11.6). A tool a build step runs may need a
//! credential of its own — a package registry login, say (Conan reads
//! `CONAN_LOGIN_USERNAME_<REMOTE>` and `CONAN_PASSWORD_<REMOTE>`). The admin stores it
//! for one project with `cimon build-secret <project> <name>` (stdin), DPAPI-encrypted
//! in `config\build-secrets.dpapi`, and that project's recipe hands it to the tool as
//! an environment value, `${secret.<name>}`. The static check allows it nowhere else,
//! so it never reaches a command line or a path. Every step that gets the variable can
//! read it: store read-only credentials.

use cimon_core::Layout;
use cimon_recipe::{vars, Resolved, Step};
use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

/// Project → secret name → value.
pub type Secrets = BTreeMap<String, BTreeMap<String, String>>;

pub fn file(layout: &Layout) -> PathBuf {
    layout.config_dir().join("build-secrets.dpapi")
}

/// No file: none.
pub fn load(layout: &Layout) -> Result<Secrets, String> {
    crate::protected::load(&file(layout))
}

pub fn save(layout: &Layout, secrets: &Secrets) -> Result<(), String> {
    crate::protected::save(&file(layout), secrets, secrets.values().all(|m| m.is_empty()))
}

/// The secrets a step's environment names, through every layer it gets (design §9.4):
/// `[env]`, `[workspace].clean_env` on a clean job, its fixture's `env`, its own `env`.
pub fn step_needs(r: &Resolved, step: &Step, clean: bool) -> BTreeSet<String> {
    let mut texts: Vec<&String> = r.env.values().collect();
    if clean {
        texts.extend(r.clean_env.values());
    }
    if let Some(f) = step.def.fixture.as_ref().and_then(|f| r.fixtures.get(f)) {
        texts.extend(f.env.values());
    }
    texts.extend(step.def.env.values());
    vars::secret_refs(texts)
}

/// What a job and `plan` say about a secret the project has not been given.
pub fn missing(project: &str, name: &str) -> String {
    format!("비밀 {name} 이 없다 — 관리자가 `cimon build-secret {project} {name}` 으로 넣는다")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn needs_follow_the_layers() {
        let mut t = cimon_recipe::MemTree::new();
        t.insert("toolchains.toml", "schema = 1\n");
        t.insert("projects/p/project.toml", "schema = 1\nid = \"p\"\nrepo = \"https://example.invalid/p.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\nplatforms = [\"windows-x86_64\"]\n");
        t.insert(
            "projects/p/recipe.toml",
            "schema = 1\n[env]\nA = \"${secret.a}\"\n[workspace]\nclean_env = { B = \"${secret.b}\" }\n\
             [fixture.f]\nup = [{ run = [\"x\"] }]\nenv = { C = \"${secret.c}\" }\n\
             [[step]]\nid = \"one\"\nkind = \"build\"\nrun = [\"x\"]\nenv = { D = \"${secret.d}\" }\n\
             [[step]]\nid = \"two\"\nkind = \"test\"\nrun = [\"x\"]\nfixture = \"f\"\n",
        );
        let r = cimon_recipe::resolve_project(&t, "p").unwrap();
        let names = |id: &str, clean: bool| step_needs(&r, r.step(id).unwrap(), clean).into_iter().collect::<Vec<_>>();
        assert_eq!(names("one", false), ["a", "d"]);
        assert_eq!(names("one", true), ["a", "b", "d"]);
        assert_eq!(names("two", false), ["a", "c"]);
    }
}
