//! TOML shapes (recipe-spec §3–§13). Unknown keys are errors (`deny_unknown_fields`).

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum OneOrMany {
    One(String),
    Many(Vec<String>),
}

impl OneOrMany {
    pub fn to_vec(&self) -> Vec<String> {
        match self {
            OneOrMany::One(s) => vec![s.clone()],
            OneOrMany::Many(v) => v.clone(),
        }
    }
}

impl Default for OneOrMany {
    fn default() -> Self {
        OneOrMany::Many(vec![])
    }
}

// ── project.toml ───────────────────────────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProjectFile {
    pub schema: u32,
    pub id: String,
    #[serde(default)]
    pub display_name: Option<String>,
    pub repo: String,
    pub allowed_refs: Vec<String>,
    pub default_ref: String,
    #[serde(default = "d_central")]
    pub recipe_source: String,
    #[serde(default = "d_platforms")]
    pub platforms: Vec<String>,
    #[serde(default = "d_max_job")]
    pub max_job_minutes: u32,
    #[serde(default)]
    pub retention_days: Option<u32>,
    #[serde(default)]
    pub warm_slots: Option<u32>,
    #[serde(default)]
    pub sibling: Vec<Sibling>,
}

fn d_central() -> String {
    "central".into()
}
fn d_platforms() -> Vec<String> {
    vec!["windows-x86_64".into()]
}
fn d_max_job() -> u32 {
    120
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Sibling {
    pub project: String,
    #[serde(rename = "ref")]
    pub ref_: String,
    #[serde(default)]
    pub env: OneOrMany,
    #[serde(default = "d_isolated")]
    pub place: String,
}

fn d_isolated() -> String {
    "isolated".into()
}

// ── recipe.toml · presets · includes ───────────────────────────────────────

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct RecipeFile {
    pub schema: u32,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub extends: Vec<String>,
    #[serde(default)]
    pub include: Vec<String>,
    #[serde(default)]
    pub vars: BTreeMap<String, String>,
    #[serde(default)]
    pub toolchains: BTreeMap<String, ToolchainReq>,
    #[serde(default)]
    pub workspace: Option<WorkspaceDef>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub isolate: BTreeMap<String, String>,
    #[serde(default)]
    pub account_env: BTreeMap<String, String>,
    #[serde(default)]
    pub params: BTreeMap<String, ParamDef>,
    /// Files and values the caller uploads for one job (recipe-spec §8a) — the project's
    /// `recipe.toml` only.
    #[serde(default)]
    pub upload: BTreeMap<String, UploadDef>,
    #[serde(default)]
    pub step: Vec<StepDef>,
    #[serde(default)]
    pub fixture: BTreeMap<String, FixtureDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolchainReq {
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub version_file: Option<String>,
    /// Other `rust-toolchain.toml` files in the repository that may pin a different
    /// version: the job builds with `version_file`'s and says so in its warnings.
    /// Empty is left out of the serialized form, so existing recipes hash the same.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub ignore_version_files: Vec<String>,
    #[serde(default)]
    pub min: Option<String>,
    #[serde(default)]
    pub optional: bool,
    #[serde(default)]
    pub packages: Option<Vec<String>>,
    #[serde(default)]
    pub components: Option<Vec<String>>,
    #[serde(default)]
    pub platforms: Option<Vec<String>>,
    #[serde(default)]
    pub build_tools: Option<Vec<String>>,
}

impl ToolchainReq {
    /// Kind-specific fields that are set, by name (validated against catalog `fields`).
    pub fn extra_fields(&self) -> Vec<&'static str> {
        let mut v = vec![];
        if self.packages.is_some() {
            v.push("packages");
        }
        if self.components.is_some() {
            v.push("components");
        }
        if self.platforms.is_some() {
            v.push("platforms");
        }
        if self.build_tools.is_some() {
            v.push("build_tools");
        }
        v
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceDef {
    #[serde(default)]
    pub keep: Vec<String>,
    #[serde(default)]
    pub clean_env: BTreeMap<String, String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ParamDef {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub default: Option<toml::Value>,
    #[serde(default)]
    pub pattern: Option<String>,
    #[serde(default)]
    pub min: Option<i64>,
    #[serde(default)]
    pub max: Option<i64>,
    #[serde(default)]
    pub values: Option<Vec<String>>,
    #[serde(default)]
    pub env: Option<String>,
    #[serde(default)]
    pub args: Option<Vec<String>>,
    #[serde(default)]
    pub applies_to: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
}

/// `[upload.<name>]` (recipe-spec §8a): something the caller sends from their own
/// machine for one job — a signing key, a site's TLS key — that the build machine does
/// not keep. The caller uploads it outside the conversation and passes the handle.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadDef {
    /// `file` (default): `${upload.<name>}` is the path of the file. `text`: one line
    /// (a password), `${upload.<name>}` is the value — env values only.
    #[serde(default)]
    pub kind: Option<String>,
    /// The file's name in the job's upload folder (file only; default: the upload name).
    #[serde(default)]
    pub file: Option<String>,
    /// The steps that get it. Selecting one makes the upload required.
    pub applies_to: Vec<String>,
    /// Branches a job that gets it may build (default: the project's `default_ref`).
    #[serde(default)]
    pub refs: Option<Vec<String>>,
    #[serde(default)]
    pub description: Option<String>,
}

impl UploadDef {
    pub fn is_text(&self) -> bool {
        self.kind.as_deref() == Some("text")
    }
    pub fn kind_str(&self) -> &str {
        self.kind.as_deref().unwrap_or("file")
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, Hash, PartialOrd, Ord)]
#[serde(rename_all = "snake_case")]
pub enum Kind {
    Setup,
    Build,
    Test,
    Package,
    Scenario,
}

impl Kind {
    pub fn as_str(&self) -> &'static str {
        match self {
            Kind::Setup => "setup",
            Kind::Build => "build",
            Kind::Test => "test",
            Kind::Package => "package",
            Kind::Scenario => "scenario",
        }
    }
    pub fn default_selected(&self) -> bool {
        matches!(self, Kind::Build | Kind::Test)
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct StepDef {
    pub id: String,
    #[serde(default)]
    pub kind: Option<Kind>,
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub run: Vec<String>,
    #[serde(default)]
    pub shell: Option<String>,
    #[serde(default)]
    pub cwd: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub timeout_min: Option<u32>,
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub tags: Vec<String>,
    #[serde(default)]
    pub default: Option<bool>,
    #[serde(default)]
    pub needs: Vec<String>,
    #[serde(default)]
    pub uses: Vec<String>,
    #[serde(default)]
    pub fixture: Option<String>,
    #[serde(default)]
    pub fail_on_output: Vec<String>,
    #[serde(default)]
    pub error_patterns: Vec<String>,
    #[serde(default)]
    pub judge: Option<JudgeDef>,
    #[serde(default)]
    pub uncounted: bool,
    #[serde(default)]
    pub when: Option<String>,
    #[serde(default)]
    pub inputs: Vec<String>,
    #[serde(default)]
    pub disabled: bool,
    #[serde(default)]
    pub artifact: Vec<ArtifactDef>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JudgeDef {
    #[serde(rename = "type")]
    pub type_: String,
    #[serde(default)]
    pub min_tests: Option<u32>,
    #[serde(default)]
    pub reports: Option<OneOrMany>,
    #[serde(default)]
    pub run: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ArtifactDef {
    pub name: String,
    pub glob: OneOrMany,
    #[serde(default)]
    pub exclude: Vec<String>,
    #[serde(default)]
    pub expect: Option<u32>,
    #[serde(default)]
    pub expect_min: Option<u32>,
    #[serde(default)]
    pub pack: Option<String>,
    #[serde(default)]
    pub pack_name: Option<String>,
    #[serde(default)]
    pub default: Option<bool>,
    #[serde(default)]
    pub inspect: Vec<InspectItem>,
    #[serde(default)]
    pub description: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum InspectItem {
    Name(String),
    Table(InspectTable),
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct InspectTable {
    #[serde(default)]
    pub builtin: Option<String>,
    #[serde(default)]
    pub script: Option<Vec<String>>,
    #[serde(default)]
    pub required: bool,
}

/// One inspect, normalised.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum Inspect {
    Builtin { name: String, required: bool },
    Script { run: Vec<String>, required: bool },
}

impl InspectItem {
    pub fn normalize(&self) -> Result<Inspect, String> {
        match self {
            InspectItem::Name(n) => Ok(Inspect::Builtin { name: n.clone(), required: false }),
            InspectItem::Table(t) => match (&t.builtin, &t.script) {
                (Some(b), None) => Ok(Inspect::Builtin { name: b.clone(), required: t.required }),
                (None, Some(s)) if !s.is_empty() => Ok(Inspect::Script { run: s.clone(), required: t.required }),
                _ => Err("inspect table needs exactly one of builtin or script".into()),
            },
        }
    }
}

pub const BUILTIN_INSPECTS: &[&str] = &["apk-signer", "pe-version"];

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct FixtureDef {
    #[serde(default)]
    pub after: Vec<String>,
    #[serde(default)]
    pub requires_artifacts: Vec<String>,
    #[serde(default)]
    pub on_up_failure: Option<String>,
    #[serde(default)]
    pub env: BTreeMap<String, String>,
    #[serde(default)]
    pub collect: Vec<String>,
    #[serde(default)]
    pub up: Vec<FixtureCmd>,
    #[serde(default)]
    pub ready: Option<Poll>,
    #[serde(default)]
    pub down: Vec<FixtureCmd>,
    #[serde(default)]
    pub gone: Option<Poll>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FixtureCmd {
    pub run: Vec<String>,
    #[serde(default)]
    pub timeout_min: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Poll {
    pub run: Vec<String>,
    pub until: Until,
    #[serde(default)]
    pub every_s: Option<u32>,
    #[serde(default)]
    pub timeout_s: Option<u32>,
    #[serde(default)]
    pub on_timeout: Option<OnTimeout>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Until {
    #[serde(default)]
    pub exit: Option<i32>,
    #[serde(default)]
    pub exit_not: Option<i32>,
    #[serde(default)]
    pub json_all: Option<JsonAll>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JsonAll {
    pub path: String,
    pub equals: toml::Value,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct OnTimeout {
    pub run: Vec<String>,
    #[serde(default)]
    pub exited_path: Option<String>,
}

/// Keys a fragment (`include`) may carry.
pub fn fragment_violations(r: &RecipeFile) -> Vec<&'static str> {
    let mut v = vec![];
    if r.description.is_some() {
        v.push("description");
    }
    if !r.extends.is_empty() {
        v.push("extends");
    }
    if !r.include.is_empty() {
        v.push("include");
    }
    if !r.vars.is_empty() {
        v.push("vars");
    }
    if !r.toolchains.is_empty() {
        v.push("toolchains");
    }
    if r.workspace.is_some() {
        v.push("workspace");
    }
    if !r.env.is_empty() {
        v.push("env");
    }
    if !r.isolate.is_empty() {
        v.push("isolate");
    }
    if !r.account_env.is_empty() {
        v.push("account_env");
    }
    if !r.upload.is_empty() {
        v.push("upload");
    }
    v
}

/// Keys a preset may not carry.
pub fn preset_violations(r: &RecipeFile) -> Vec<&'static str> {
    let mut v = vec![];
    if !r.extends.is_empty() {
        v.push("extends");
    }
    if !r.include.is_empty() {
        v.push("include");
    }
    // what a caller hands over is the project's to ask for, not a shared preset's
    if !r.upload.is_empty() {
        v.push("upload");
    }
    v
}
