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
    let text = format!(
        "[server]\nbind = \"127.0.0.1:0\"\n[tailscale]\nrequire_funnel_off = false\n[paths]\nroot = '{}'\n[accounts]\nbuild = \"\"\n[backup]\noffsite = \"none\"\n{extra}",
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
