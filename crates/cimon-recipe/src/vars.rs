//! `${…}` variables (recipe-spec §13).

use std::collections::BTreeMap;

/// Every `${name}` in `s`, in order.
pub fn refs(s: &str) -> Vec<String> {
    let mut out = vec![];
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        let after = &rest[start + 2..];
        match after.find('}') {
            Some(end) => {
                out.push(after[..end].to_string());
                rest = &after[end + 1..];
            }
            None => break,
        }
    }
    out
}

/// True when `s` has a `${` with no closing `}` — static checks reject it.
pub fn unterminated(s: &str) -> bool {
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        match rest[start + 2..].find('}') {
            Some(end) => rest = &rest[start + 2 + end + 1..],
            None => return true,
        }
    }
    false
}

/// Replaces every `${name}` using `lookup`. An unknown or unterminated reference is an
/// error — a recipe never runs with a literal `${x}` left in a command.
pub fn expand(s: &str, lookup: &dyn Fn(&str) -> Option<String>) -> Result<String, String> {
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(start) = rest.find("${") {
        out.push_str(&rest[..start]);
        let after = &rest[start + 2..];
        let end = after.find('}').ok_or_else(|| format!("unterminated variable in {s:?}"))?;
        let name = &after[..end];
        let value = lookup(name).ok_or_else(|| format!("unknown variable ${{{name}}}"))?;
        out.push_str(&value);
        rest = &after[end + 1..];
    }
    out.push_str(rest);
    Ok(out)
}

/// A map-backed lookup.
pub fn map_lookup(map: &BTreeMap<String, String>) -> impl Fn(&str) -> Option<String> + '_ {
    move |k| map.get(k).cloned()
}

/// What a static check knows about which names exist.
pub struct Known<'a> {
    pub vars: &'a BTreeMap<String, String>,
    pub params: Vec<&'a str>,
    pub toolchains: Vec<&'a str>,
    pub siblings: Vec<&'a str>,
    pub artifacts: Vec<&'a str>,
    pub allow_value: bool,
    /// `${secret.x}` — environment values only, so a secret never lands on a command
    /// line, in a path or in a judge's arguments. Also where a text upload may go.
    pub allow_secret: bool,
    /// `[upload.<name>]` declarations, and the step being checked: an upload reaches
    /// only the steps in its `applies_to` (recipe-spec §8a).
    pub uploads: Option<&'a BTreeMap<String, crate::model::UploadDef>>,
    pub step: Option<&'a str>,
}

/// A name `cimon build-secret` stores and `${secret.<name>}` reads.
pub fn valid_secret_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || matches!(c, '-' | '_'))
        && name.len() <= 64
}

/// The secret names `${secret.<name>}` references in `texts` ask for.
pub fn secret_refs<'a>(texts: impl IntoIterator<Item = &'a String>) -> std::collections::BTreeSet<String> {
    texts.into_iter().flat_map(|t| refs(t)).filter_map(|r| r.strip_prefix("secret.").map(str::to_string)).collect()
}

/// Checks one reference against what exists. `Ok` carries the param name for
/// `${param.x}` so callers can check `applies_to`.
pub fn validate_ref(name: &str, k: &Known<'_>) -> Result<Option<String>, String> {
    let (head, rest) = name.split_once('.').unwrap_or((name, ""));
    match head {
        "ws" | "recipes" if rest.is_empty() => Ok(None),
        "value" if rest.is_empty() => {
            if k.allow_value {
                Ok(None)
            } else {
                Err("${value} is only valid inside params.*.args".into())
            }
        }
        "vars" if k.vars.contains_key(rest) => Ok(None),
        "vars" => Err(format!("unknown variable ${{{name}}} — no [vars] {rest}")),
        "param" if k.params.contains(&rest) => Ok(Some(rest.to_string())),
        "param" => Err(format!("unknown parameter ${{{name}}}")),
        "secret" if !k.allow_secret => Err(format!("${{{name}}}: a secret may only be an env value — on a command line or in a path it would show up in logs and process lists")),
        "secret" if valid_secret_name(rest) => Ok(None),
        "secret" => Err(format!("${{{name}}}: a secret name is [a-z0-9][a-z0-9_-]*")),
        "upload" => {
            let (n, field) = match rest.split_once('.') {
                Some((n, f)) => (n, Some(f)),
                None => (rest, None),
            };
            let Some(u) = k.uploads.and_then(|m| m.get(n)) else {
                return Err(format!("unknown upload ${{{name}}} — no [upload.{n}]"));
            };
            let Some(step) = k.step else {
                return Err(format!("${{{name}}}: an upload reaches only the steps in its applies_to — not [env], [workspace] or fixtures"));
            };
            if !u.applies_to.iter().any(|a| a == step) {
                return Err(format!("${{{name}}}: step {step:?} is not in [upload.{n}].applies_to"));
            }
            match (u.is_text(), field) {
                (_, Some(f)) if f != "dir" => Err(format!("${{{name}}}: an upload has no field {f:?} — ${{upload.{n}}} or ${{upload.{n}.dir}}")),
                (true, Some(_)) => Err(format!("${{{name}}}: a text upload is a value, not a file")),
                (true, None) if !k.allow_secret => Err(format!("${{{name}}}: a text upload may only be an env value — on a command line it would show up in logs and process lists")),
                _ => Ok(None),
            }
        }
        "job" if matches!(rest, "id" | "commit" | "commit_short") => Ok(None),
        "step" if rest == "out" => Ok(None),
        "artifact" if k.artifacts.contains(&rest) => Ok(None),
        "artifact" => Err(format!("unknown artifact ${{{name}}}")),
        "toolchain" => {
            let (kind, field) = rest.split_once('.').unwrap_or((rest, ""));
            if !matches!(field, "home" | "bin" | "version") {
                return Err(format!("${{{name}}}: toolchain field must be home, bin or version"));
            }
            if k.toolchains.contains(&kind) {
                Ok(None)
            } else {
                Err(format!("${{{name}}}: toolchain {kind} is not declared in [toolchains]"))
            }
        }
        "sibling" => {
            let (proj, field) = rest.split_once('.').unwrap_or((rest, ""));
            if !matches!(field, "root" | "commit") {
                return Err(format!("${{{name}}}: sibling field must be root or commit"));
            }
            if k.siblings.contains(&proj) {
                Ok(None)
            } else {
                Err(format!("${{{name}}}: {proj} is not a [[sibling]] of this project"))
            }
        }
        _ => Err(format!("unknown variable ${{{name}}}")),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn expands() {
        let mut m = BTreeMap::new();
        m.insert("job.id".to_string(), "j000001".to_string());
        assert_eq!(expand("cimon-${job.id}", &map_lookup(&m)).unwrap(), "cimon-j000001");
        assert!(expand("${nope}", &map_lookup(&m)).is_err());
        assert!(expand("${job.id", &map_lookup(&m)).is_err());
        assert_eq!(refs("a ${x} b ${y.z}"), vec!["x", "y.z"]);
        assert!(unterminated("${vars.x"));
        assert!(unterminated("${a} ${b"));
        assert!(!unterminated("${a} $b {c}"));
    }

    #[test]
    fn secrets_only_where_allowed() {
        let vars = BTreeMap::new();
        let k = |allow_secret| Known { vars: &vars, params: vec![], toolchains: vec![], siblings: vec![], artifacts: vec![], allow_value: false, allow_secret, uploads: None, step: None };
        assert!(validate_ref("secret.artifactory-conan", &k(true)).is_ok());
        assert!(validate_ref("secret.artifactory-conan", &k(false)).is_err(), "not on a command line");
        assert!(validate_ref("secret.Artifactory", &k(true)).is_err());
        assert!(validate_ref("secret.", &k(true)).is_err());
        assert!(validate_ref("secret.-x", &k(true)).is_err());
        let texts = ["u ${secret.a}".to_string(), "${secret.b_1}${ws}".to_string(), "${vars.secret}".to_string()];
        assert_eq!(secret_refs(&texts).into_iter().collect::<Vec<_>>(), vec!["a", "b_1"]);
    }
}
