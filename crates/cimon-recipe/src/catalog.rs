//! Toolchain catalog — `toolchains.toml` (recipe-spec §16).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CatalogFile {
    pub schema: u32,
    #[serde(default)]
    pub kind: BTreeMap<String, KindDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindDef {
    pub tier: String,
    pub installer: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub checksum: Option<Checksum>,
    #[serde(default)]
    pub bin: Vec<String>,
    #[serde(default)]
    pub home: Option<String>,
    #[serde(default)]
    pub exports: BTreeMap<String, String>,
    #[serde(default)]
    pub probe: Option<Probe>,
    #[serde(default)]
    pub version_any: bool,
    #[serde(default)]
    pub fields: Vec<String>,
    #[serde(default)]
    pub hint: Option<String>,
    #[serde(default)]
    pub check: Option<BaseCheck>,
    #[serde(default)]
    pub components: Vec<String>,
    #[serde(default)]
    pub repo: Option<String>,
    #[serde(default)]
    pub tag: Option<String>,
    #[serde(default)]
    pub asset: Option<String>,
    #[serde(default)]
    pub package: Option<String>,
    /// uv-tool: the exact Python the tool is installed on (uv downloads it into the
    /// install). Without it uv takes the newest interpreter it can get, which the
    /// package's wheels may not cover yet. Left out of the serialized form when unset,
    /// so existing entries hash the same.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub python: Option<String>,
    #[serde(default)]
    pub script: Option<ScriptInstaller>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Checksum {
    pub source: String,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub values: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Probe {
    pub run: Vec<String>,
    pub expect: String,
    #[serde(default)]
    pub version_map: Option<VersionMap>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VersionMap {
    #[serde(default)]
    pub strip_after: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BaseCheck {
    #[serde(default)]
    pub vswhere: bool,
    #[serde(default)]
    pub path_from: Option<String>,
    #[serde(default)]
    pub sdkmanager: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ScriptInstaller {
    pub from_project: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    pub path: String,
    #[serde(default)]
    pub args: Vec<String>,
}

pub const INSTALLERS: &[&str] = &["archive", "github-release", "rustup", "uv-python", "uv-tool", "script", "base"];
pub const CHECKSUM_SOURCES: &[&str] = &["shasums", "sha256", "adoptium"];

impl KindDef {
    pub fn is_base(&self) -> bool {
        self.tier == "base"
    }

    /// What the probe output must contain for `version`.
    pub fn expected_probe(&self, version: &str) -> Option<String> {
        let probe = self.probe.as_ref()?;
        if self.version_any {
            return Some(probe.expect.clone());
        }
        let mapped = match probe.version_map.as_ref().and_then(|m| m.strip_after.as_deref()) {
            Some(sep) => version.split(sep).next().unwrap_or(version).to_string(),
            None => version.to_string(),
        };
        Some(probe.expect.replace("{version_probe}", &mapped).replace("{version}", version))
    }
}

/// A parsed catalog plus each kind's entry hash.
#[derive(Debug, Clone, PartialEq)]
pub struct Catalog {
    pub kinds: BTreeMap<String, KindDef>,
    pub hashes: BTreeMap<String, String>,
}

impl Catalog {
    pub fn parse(text: &str) -> Result<Catalog, String> {
        let file: CatalogFile = toml::from_str(text).map_err(|e| e.to_string())?;
        if file.schema != 1 {
            return Err(format!("unknown schema {}", file.schema));
        }
        let raw: toml::Table = toml::from_str(text).map_err(|e| e.to_string())?;
        let mut hashes = BTreeMap::new();
        if let Some(toml::Value::Table(kinds)) = raw.get("kind") {
            for (name, v) in kinds {
                hashes.insert(name.clone(), entry_hash(v));
            }
        }
        Ok(Catalog { kinds: file.kind, hashes })
    }

    pub fn hash(&self, kind: &str) -> Option<&str> {
        self.hashes.get(kind).map(|s| s.as_str())
    }
}

/// sha256 of a kind table rendered as canonical JSON (keys sorted). Two entries that
/// download from the same place and check the same way hash the same, whatever the
/// formatting of the file.
pub fn entry_hash(v: &toml::Value) -> String {
    fn to_json(v: &toml::Value) -> serde_json::Value {
        match v {
            toml::Value::String(s) => serde_json::Value::String(s.clone()),
            toml::Value::Integer(i) => serde_json::json!(i),
            toml::Value::Float(f) => serde_json::json!(f),
            toml::Value::Boolean(b) => serde_json::Value::Bool(*b),
            toml::Value::Datetime(d) => serde_json::Value::String(d.to_string()),
            toml::Value::Array(a) => serde_json::Value::Array(a.iter().map(to_json).collect()),
            toml::Value::Table(t) => {
                let sorted: BTreeMap<&String, serde_json::Value> = t.iter().map(|(k, v)| (k, to_json(v))).collect();
                let mut m = serde_json::Map::new();
                for (k, v) in sorted {
                    m.insert(k.clone(), v);
                }
                serde_json::Value::Object(m)
            }
        }
    }
    cimon_core::sha256_hex(to_json(v).to_string().as_bytes())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn hashes_ignore_formatting() {
        let a = Catalog::parse("schema = 1\n[kind.node]\ntier = \"managed\"\ninstaller = \"archive\"\nurl = \"u\"\n").unwrap();
        let b = Catalog::parse("schema = 1\n[kind.node]\ninstaller = \"archive\"\nurl = \"u\"\ntier = \"managed\"\n").unwrap();
        assert_eq!(a.hash("node"), b.hash("node"));
        let c = Catalog::parse("schema = 1\n[kind.node]\ninstaller = \"archive\"\nurl = \"v\"\ntier = \"managed\"\n").unwrap();
        assert_ne!(a.hash("node"), c.hash("node"));
    }

    #[test]
    fn probe_expectation() {
        let c = Catalog::parse(
            "schema = 1\n[kind.jdk]\ntier = \"managed\"\ninstaller = \"archive\"\nprobe = { run = [\"java\"], expect = \"\\\"{version_probe}\\\"\", version_map = { strip_after = \"+\" } }\n",
        )
        .unwrap();
        assert_eq!(c.kinds["jdk"].expected_probe("17.0.19+10").unwrap(), "\"17.0.19\"");
    }
}
