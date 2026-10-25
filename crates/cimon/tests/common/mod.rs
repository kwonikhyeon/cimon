//! In-process service harness for integration tests.
#![allow(dead_code)]

use cimon_core::{Config, Role};
use cimon_engine::App;
use rmcp::{
    model::{CallToolRequestParams, CallToolResponse, CallToolResult, ProtocolVersion},
    service::RunningService,
    transport::{streamable_http_client::StreamableHttpClientTransportConfig, StreamableHttpClientTransport},
    ClientLifecycleMode, ClientServiceExt, RoleClient,
};
use serde_json::Value;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

pub struct Harness {
    pub dir: tempfile::TempDir,
    pub app: Arc<App>,
    pub url: String,
    pub base: String,
    pub admin_token: String,
    pub member_token: String,
    pub shutdown: CancellationToken,
}

pub fn base_config(root: &Path, extra: &str) -> Config {
    // reserve a port so links (public_url) point at the test server
    let port = std::net::TcpListener::bind("127.0.0.1:0").unwrap().local_addr().unwrap().port();
    let backup = if extra.contains("[backup]") { "" } else { "[backup]\noffsite = \"none\"\n" };
    let text = format!(
        "[server]\nbind = \"127.0.0.1:{port}\"\npublic_url = \"http://127.0.0.1:{port}\"\n[tailscale]\nrequire_funnel_off = false\n[paths]\nroot = '{}'\n[accounts]\nbuild = \"\"\n{backup}{extra}",
        root.display()
    );
    Config::parse(&text, "test").unwrap()
}

impl Harness {
    pub async fn start(extra: &str) -> Harness {
        Self::start_with(extra, |_| {}).await
    }

    pub async fn start_with(extra: &str, prepare: impl FnOnce(&Path)) -> Harness {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().join("cimon");
        std::fs::create_dir_all(&root).unwrap();
        prepare(&root);
        let cfg = base_config(&root, extra);
        let app = App::open(cfg).unwrap();
        let admin_token = app.store.create_user("admin-a", Role::Admin).unwrap();
        let member_token = app.store.create_user("member-b", Role::Member).unwrap();
        let shutdown = CancellationToken::new();
        let (addr, _h) = cimon::serve::start(app.clone(), shutdown.clone()).await.unwrap();
        Harness {
            dir,
            app,
            url: format!("http://{addr}/mcp"),
            base: format!("http://{addr}"),
            admin_token,
            member_token,
            shutdown,
        }
    }

    pub fn root(&self) -> PathBuf {
        self.app.config.paths.root.clone()
    }

    /// A client that declares the Tasks extension (like Claude Code v2.1.304+).
    pub async fn task_client(&self, token: &str) -> RunningService<RoleClient, rmcp::model::ClientConfig> {
        let info = rmcp::model::ClientConfig::new(
            rmcp::model::ClientCapabilities::builder().enable_tasks().build(),
            rmcp::model::Implementation::new("cimon-test", "0"),
        );
        info.serve_with_lifecycle(
            StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(self.url.clone()).auth_header(token.to_string()),
            ),
            ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        )
        .await
        .expect("discover")
    }

    pub async fn client(&self, token: &str) -> RunningService<RoleClient, ()> {
        ().serve_with_lifecycle(
            StreamableHttpClientTransport::from_config(
                StreamableHttpClientTransportConfig::with_uri(self.url.clone()).auth_header(token.to_string()),
            ),
            ClientLifecycleMode::Discover { preferred_versions: vec![ProtocolVersion::V_2026_07_28] },
        )
        .await
        .expect("discover")
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        self.shutdown.cancel();
    }
}

/// Calls a tool that must answer synchronously; returns (structuredContent, isError).
pub async fn call(client: &RunningService<RoleClient, ()>, name: &str, args: Value) -> (Value, bool) {
    let mut p = CallToolRequestParams::new(name.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match client.call_tool(p).await.expect("call_tool") {
        r => result_json(r),
    }
}

pub fn result_json(r: CallToolResult) -> (Value, bool) {
    (r.structured_content.unwrap_or(Value::Null), r.is_error.unwrap_or(false))
}

pub async fn call_raw(client: &RunningService<RoleClient, ()>, name: &str, args: Value) -> CallToolResponse {
    let mut p = CallToolRequestParams::new(name.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    client.call_tool_once(p).await.expect("call_tool_once")
}

/// Starts a task and returns its id (panics on a non-task answer).
pub async fn start_task<S: rmcp::Service<RoleClient>>(client: &RunningService<RoleClient, S>, name: &str, args: Value) -> String {
    let mut p = CallToolRequestParams::new(name.to_string());
    if let Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match client.call_tool_once(p).await.expect("call_tool_once") {
        CallToolResponse::Task(t) => {
            assert!(t.task.status_message.is_some(), "statusMessage must always be present");
            assert!(t.task.ttl_ms.is_some());
            t.task.task_id
        }
        CallToolResponse::Complete(r) => panic!("expected a task, got {:?}", r.structured_content),
        other => panic!("unexpected {other:?}"),
    }
}

/// Polls a task until it is terminal; returns (status, result structuredContent, isError).
pub async fn wait_task<S: rmcp::Service<RoleClient>>(client: &RunningService<RoleClient, S>, task_id: &str, secs: u64) -> (rmcp::model::TaskStatus, Value, bool) {
    use rmcp::model::{GetTaskParams, TaskPayload};
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(secs);
    loop {
        let r = client.peer().get_task(GetTaskParams::new(task_id.to_string())).await.expect("tasks/get");
        assert!(r.task.task.status_message.is_some(), "statusMessage must always be present");
        if r.task.status().is_terminal() {
            return match r.task.payload {
                TaskPayload::Completed { result } => {
                    let res: CallToolResult = serde_json::from_value(Value::Object(result)).unwrap();
                    (r.task.task.status, res.structured_content.unwrap_or(Value::Null), res.is_error.unwrap_or(false))
                }
                _ => (r.task.task.status, Value::Null, false),
            };
        }
        assert!(std::time::Instant::now() < deadline, "task {task_id} did not finish in {secs}s (last: {:?})", r.task.task.status_message);
        tokio::time::sleep(std::time::Duration::from_millis(200)).await;
    }
}

/// Process-wide directory of test git repositories, reachable as
/// `https://example.invalid/<name>.git` through a git `insteadOf` rewrite.
pub fn repos_dir() -> &'static Path {
    static DIR: std::sync::OnceLock<PathBuf> = std::sync::OnceLock::new();
    DIR.get_or_init(|| {
        let d = std::env::temp_dir().join(format!("cimon-test-repos-{}", std::process::id()));
        std::fs::create_dir_all(&d).unwrap();
        let url = format!("file:///{}/", d.to_string_lossy().replace('\\', "/"));
        std::env::set_var("GIT_CONFIG_COUNT", "1");
        std::env::set_var("GIT_CONFIG_KEY_0", format!("url.{url}.insteadOf"));
        std::env::set_var("GIT_CONFIG_VALUE_0", "https://example.invalid/");
        d
    })
}

pub fn git(dir: &Path, args: &[&str]) -> String {
    let out = std::process::Command::new("git").arg("-C").arg(dir).args(["-c", "user.name=t", "-c", "user.email=t@t", "-c", "core.autocrlf=false"]).args(args).output().unwrap();
    assert!(out.status.success(), "git {args:?}: {}", String::from_utf8_lossy(&out.stderr));
    String::from_utf8_lossy(&out.stdout).trim().to_string()
}

/// Creates (or resets) a repository `<name>.git` with `files` on `main`.
pub fn make_repo(name: &str, files: &[(&str, &str)]) -> PathBuf {
    let dir = repos_dir().join(format!("{name}.git"));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).unwrap();
    git(&dir, &["init", "-q", "-b", "main"]);
    write_files(&dir, files);
    git(&dir, &["add", "-A"]);
    git(&dir, &["commit", "-q", "-m", "init"]);
    dir
}

pub fn write_files(dir: &Path, files: &[(&str, &str)]) {
    for (p, c) in files {
        let fp = dir.join(p);
        std::fs::create_dir_all(fp.parent().unwrap()).unwrap();
        std::fs::write(fp, c).unwrap();
    }
}

/// Commits `files` on a new branch `branch` (from main).
pub fn branch_with(dir: &Path, branch: &str, files: &[(&str, &str)]) {
    git(dir, &["checkout", "-q", "-B", branch, "main"]);
    write_files(dir, files);
    git(dir, &["add", "-A"]);
    git(dir, &["commit", "-q", "-m", branch]);
    git(dir, &["checkout", "-q", "main"]);
}
