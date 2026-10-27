//! `cimon.toml` — design §14. No secrets live here: user tokens are hashed in the job
//! store, the artifact link key is `config\link.key`.

use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

#[derive(Debug, thiserror::Error)]
pub enum ConfigError {
    #[error("{path}: {source}")]
    Read { path: String, source: std::io::Error },
    #[error("{path}: {message}")]
    Parse { path: String, message: String },
    #[error("{0}")]
    Invalid(String),
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub tailscale: TailscaleConfig,
    pub paths: PathsConfig,
    #[serde(default)]
    pub accounts: AccountsConfig,
    #[serde(default)]
    pub recipes: RecipesConfig,
    #[serde(default)]
    pub review: ReviewConfig,
    #[serde(default)]
    pub runner: RunnerConfig,
    #[serde(default)]
    pub base: BTreeMap<String, PathBuf>,
    #[serde(default)]
    pub bootstrap: BootstrapConfig,
    #[serde(default)]
    pub toolchains: ToolchainsConfig,
    #[serde(default)]
    pub queue: QueueConfig,
    #[serde(default)]
    pub workspace: WorkspaceConfig,
    #[serde(default)]
    pub backup: BackupConfig,
    #[serde(default)]
    pub disk: DiskConfig,
    #[serde(default)]
    pub web: WebConfig,
    #[serde(default)]
    pub github: GithubConfig,
    #[serde(default)]
    pub uploads: UploadsConfig,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "d_bind")]
    pub bind: String,
    #[serde(default = "d_public_url")]
    pub public_url: String,
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        ServerConfig { bind: d_bind(), public_url: d_public_url(), allowed_hosts: vec![] }
    }
}

fn d_bind() -> String {
    "127.0.0.1:8770".into()
}
fn d_public_url() -> String {
    "http://127.0.0.1:8770".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TailscaleConfig {
    #[serde(default = "d_serve_target")]
    pub serve_target: String,
    /// Refuse to start when Funnel is on (design §15.3). Development machines without
    /// Tailscale set this to false.
    #[serde(default = "d_true")]
    pub require_funnel_off: bool,
    /// When the service account cannot query Tailscale — on Windows, Tailscale answers
    /// only the user who owns it — false (the default) refuses to start; true starts
    /// with a warning in the service log and `/healthz`. Funnel is then checked by
    /// `cimon doctor` run by that user, and is best made impossible by tailnet policy
    /// (no `funnel` node attribute). A Funnel that is seen to be on always refuses.
    #[serde(default)]
    pub allow_unverified_funnel: bool,
    #[serde(default = "d_true")]
    pub audit_identity_headers: bool,
}

impl Default for TailscaleConfig {
    fn default() -> Self {
        TailscaleConfig { serve_target: d_serve_target(), require_funnel_off: true, allow_unverified_funnel: false, audit_identity_headers: true }
    }
}

fn d_serve_target() -> String {
    "http://127.0.0.1:8770".into()
}
fn d_true() -> bool {
    true
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathsConfig {
    pub root: PathBuf,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AccountsConfig {
    #[serde(default = "d_service_account")]
    pub service: String,
    /// The account steps run as. Empty = single-account mode: children run as the
    /// service process's own user. Only for development — the build account is what
    /// keeps build scripts away from the job store (design §15.1).
    #[serde(default = "d_build_account")]
    pub build: String,
}

impl Default for AccountsConfig {
    fn default() -> Self {
        AccountsConfig { service: d_service_account(), build: d_build_account() }
    }
}

impl AccountsConfig {
    pub fn single_account(&self) -> bool {
        self.build.trim().is_empty()
    }
}

fn d_service_account() -> String {
    "cimon".into()
}
fn d_build_account() -> String {
    "cimon-build".into()
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecipesConfig {
    /// Defaults to `<root>\recipes.git`.
    #[serde(default)]
    pub store: Option<PathBuf>,
    #[serde(default = "d_main")]
    pub default_branch: String,
    #[serde(default = "d_trial_prefix")]
    pub trial_prefix: String,
    #[serde(default)]
    pub allowed_repo_prefixes: Vec<String>,
}

impl Default for RecipesConfig {
    fn default() -> Self {
        RecipesConfig { store: None, default_branch: d_main(), trial_prefix: d_trial_prefix(), allowed_repo_prefixes: vec![] }
    }
}

fn d_main() -> String {
    "main".into()
}
fn d_trial_prefix() -> String {
    "recipe/".into()
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequireTrial {
    ChangedProjects,
    None,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ReviewConfig {
    #[serde(default = "d_require_trial")]
    pub require_trial: RequireTrial,
    #[serde(default = "d_true")]
    pub trial_must_pass: bool,
}

impl Default for ReviewConfig {
    fn default() -> Self {
        ReviewConfig { require_trial: RequireTrial::ChangedProjects, trial_must_pass: true }
    }
}

fn d_require_trial() -> RequireTrial {
    RequireTrial::ChangedProjects
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RunnerConfig {
    #[serde(default = "d_runner_name")]
    pub name: String,
    #[serde(default = "d_platform")]
    pub platform: String,
    /// display · device · gpu
    #[serde(default)]
    pub capabilities: Vec<String>,
}

impl Default for RunnerConfig {
    fn default() -> Self {
        RunnerConfig { name: d_runner_name(), platform: d_platform(), capabilities: vec![] }
    }
}

fn d_runner_name() -> String {
    "cimon-01".into()
}
fn d_platform() -> String {
    "windows-x86_64".into()
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BootstrapConfig {
    /// rustup-init version `cimon setup` installs into `toolchains\_bootstrap\`
    /// (design §10.1), e.g. "1.29.2". Required by setup — no silent "latest".
    #[serde(default)]
    pub rustup: Option<String>,
    /// uv release `cimon setup` installs, e.g. "0.13.5".
    #[serde(default)]
    pub uv: Option<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct ToolchainsConfig {
    #[serde(default)]
    pub auto_approve: Vec<String>,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QueueConfig {
    #[serde(default = "d_max_queued")]
    pub max_queued_per_user: u32,
    #[serde(default = "d_max_queue_hours")]
    pub max_queue_hours: u32,
}

impl Default for QueueConfig {
    fn default() -> Self {
        QueueConfig { max_queued_per_user: d_max_queued(), max_queue_hours: d_max_queue_hours() }
    }
}

fn d_max_queued() -> u32 {
    5
}
fn d_max_queue_hours() -> u32 {
    12
}

/// `[uploads]` (design §11.7): what a caller may send for one job.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct UploadsConfig {
    /// An upload no job has taken by then is deleted.
    #[serde(default = "d_upload_ttl")]
    pub ttl_minutes: u32,
    /// Largest file upload (a text upload is at most 4 KiB).
    #[serde(default = "d_upload_max")]
    pub max_bytes: u64,
    /// Untaken uploads one user may have at a time.
    #[serde(default = "d_upload_pending")]
    pub max_pending_per_user: u32,
    /// How long the artifacts of a job that took uploads are kept (at most the project's
    /// retention) — they may carry what was uploaded.
    #[serde(default = "d_upload_artifact_days")]
    pub artifact_days: u32,
}

impl Default for UploadsConfig {
    fn default() -> Self {
        UploadsConfig { ttl_minutes: d_upload_ttl(), max_bytes: d_upload_max(), max_pending_per_user: d_upload_pending(), artifact_days: d_upload_artifact_days() }
    }
}

fn d_upload_artifact_days() -> u32 {
    1
}

fn d_upload_ttl() -> u32 {
    60
}
fn d_upload_max() -> u64 {
    1 << 20
}
fn d_upload_pending() -> u32 {
    20
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WorkspaceConfig {
    #[serde(default = "d_one")]
    pub warm_slots_per_project: u32,
}

impl Default for WorkspaceConfig {
    fn default() -> Self {
        WorkspaceConfig { warm_slots_per_project: 1 }
    }
}

fn d_one() -> u32 {
    1
}

#[derive(Debug, Clone, Serialize, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct BackupConfig {
    /// Required (design §16.5): a directory outside the build machine, or "none".
    #[serde(default)]
    pub offsite: Option<String>,
    #[serde(default = "d_keep")]
    pub keep: u32,
}

fn d_keep() -> u32 {
    14
}

impl BackupConfig {
    pub fn offsite_dir(&self) -> Option<PathBuf> {
        match self.offsite.as_deref() {
            None | Some("none") | Some("") => None,
            Some(p) => Some(PathBuf::from(p)),
        }
    }
    pub fn offsite_disabled(&self) -> bool {
        matches!(self.offsite.as_deref(), Some("none"))
    }
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DiskConfig {
    #[serde(default = "d_min_free")]
    pub min_free_gb: u64,
    #[serde(default = "d_cache_max")]
    pub cache_max_gb_per_project: u64,
    #[serde(default = "d_log_max")]
    pub log_max_mb_per_step: u64,
}

impl Default for DiskConfig {
    fn default() -> Self {
        DiskConfig { min_free_gb: d_min_free(), cache_max_gb_per_project: d_cache_max(), log_max_mb_per_step: d_log_max() }
    }
}

fn d_min_free() -> u64 {
    100
}
fn d_cache_max() -> u64 {
    50
}
fn d_log_max() -> u64 {
    200
}

#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct WebConfig {
    #[serde(default)]
    pub enabled: bool,
    #[serde(default = "d_session_hours")]
    pub session_hours: u32,
    #[serde(default = "d_login_minutes")]
    pub login_code_minutes: u32,
}

impl Default for WebConfig {
    fn default() -> Self {
        WebConfig { enabled: false, session_hours: d_session_hours(), login_code_minutes: d_login_minutes() }
    }
}

fn d_session_hours() -> u32 {
    12
}
fn d_login_minutes() -> u32 {
    5
}

/// Where release metadata comes from for the `github-release` installer. Tests point it
/// at a local server.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GithubConfig {
    #[serde(default = "d_github_api")]
    pub api_url: String,
}

impl Default for GithubConfig {
    fn default() -> Self {
        GithubConfig { api_url: d_github_api() }
    }
}

fn d_github_api() -> String {
    "https://api.github.com".into()
}

impl Config {
    pub fn load(path: &Path) -> Result<Config, ConfigError> {
        let text = std::fs::read_to_string(path).map_err(|e| ConfigError::Read { path: path.display().to_string(), source: e })?;
        Self::parse(&text, &path.display().to_string())
    }

    pub fn parse(text: &str, origin: &str) -> Result<Config, ConfigError> {
        let cfg: Config = toml::from_str(text).map_err(|e| ConfigError::Parse { path: origin.to_string(), message: e.to_string() })?;
        cfg.validate()?;
        Ok(cfg)
    }

    fn validate(&self) -> Result<(), ConfigError> {
        if !self.recipes.trial_prefix.ends_with('/') || self.recipes.trial_prefix.len() < 2 {
            return Err(ConfigError::Invalid("[recipes].trial_prefix must look like \"recipe/\"".into()));
        }
        for cap in &self.runner.capabilities {
            if !matches!(cap.as_str(), "display" | "device" | "gpu") {
                return Err(ConfigError::Invalid(format!("[runner].capabilities: unknown capability {cap:?}")));
            }
        }
        Ok(())
    }

    pub fn recipes_store(&self) -> PathBuf {
        self.recipes.store.clone().unwrap_or_else(|| self.paths.root.join("recipes.git"))
    }

    /// The `[backup].offsite` setting is required; returns the warning to attach to
    /// `recipe_*` results when it is "none" or missing.
    pub fn backup_setting_warning(&self) -> Option<String> {
        match self.backup.offsite.as_deref() {
            None | Some("") => Some("[backup].offsite 가 설정되지 않았다 — 레시피 저장소의 외부 사본이 없다".into()),
            Some("none") => Some("외부 백업 없음([backup].offsite = \"none\") — 레시피 저장소가 이 머신에만 있다".into()),
            _ => None,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config() {
        let cfg = Config::parse("[paths]\nroot = 'D:\\cimon'\n", "t").unwrap();
        assert_eq!(cfg.server.bind, "127.0.0.1:8770");
        assert_eq!(cfg.queue.max_queued_per_user, 5);
        assert!(!cfg.accounts.single_account());
        assert_eq!(cfg.recipes_store(), PathBuf::from("D:\\cimon").join("recipes.git"));
        assert!(cfg.backup_setting_warning().is_some());
    }

    #[test]
    fn unknown_key_rejected() {
        let err = Config::parse("[paths]\nroot = 'x'\n[queue]\nmax_queued = 3\n", "t").unwrap_err();
        assert!(err.to_string().contains("max_queued"), "{err}");
    }

    #[test]
    fn design_sample_parses() {
        let text = r#"
[server]
bind = "127.0.0.1:8770"
public_url = "https://cimon.example.ts.net"
allowed_hosts = ["cimon.example.ts.net"]

[tailscale]
serve_target = "http://127.0.0.1:8770"
require_funnel_off = true
audit_identity_headers = true

[paths]
root = 'D:\cimon'

[accounts]
service = "cimon"
build = "cimon-build"

[recipes]
store = 'D:\cimon\recipes.git'
default_branch = "main"
trial_prefix = "recipe/"
allowed_repo_prefixes = ["https://github.com/example-org/"]

[review]
require_trial = "changed_projects"
trial_must_pass = true

[runner]
name = "cimon-01"
platform = "windows-x86_64"
capabilities = []

[base]
git = 'C:\Program Files\Git'

[bootstrap]
rustup = "x"
uv = "y"

[toolchains]
auto_approve = ["rust", "node"]

[queue]
max_queued_per_user = 5
max_queue_hours = 12

[workspace]
warm_slots_per_project = 1

[backup]
offsite = '\\server\cimon-backup'
keep = 14

[disk]
min_free_gb = 100
cache_max_gb_per_project = 50
log_max_mb_per_step = 200

[web]
enabled = false
session_hours = 12
login_code_minutes = 5
"#;
        let cfg = Config::parse(text, "t").unwrap();
        assert_eq!(cfg.base["git"], PathBuf::from("C:\\Program Files\\Git"));
        assert!(cfg.backup_setting_warning().is_none());
    }
}
