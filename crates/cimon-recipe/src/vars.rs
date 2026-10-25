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
}
