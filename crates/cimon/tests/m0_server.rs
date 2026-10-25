//! M0: the server answers discovery and tools over the 2026-07-28 revision, requires a
//! token, and refuses a non-loopback bind.

mod common;

use common::*;
use serde_json::json;

#[tokio::test]
async fn unauthenticated_requests_get_401() {
    let h = Harness::start("").await;
    let resp = reqwest::Client::new()
        .post(&h.url)
        .header("Content-Type", "application/json")
        .header("Accept", "application/json, text/event-stream")
        .header("MCP-Protocol-Version", "2026-07-28")
        .json(&json!({"jsonrpc": "2.0", "id": 1, "method": "server/discover", "params": {}}))
        .send()
        .await
        .unwrap();
    assert_eq!(resp.status(), 401);
    let bad = reqwest::Client::new()
        .post(&h.url)
        .bearer_auth("not-a-token")
        .header("Content-Type", "application/json")
        .json(&json!({}))
        .send()
        .await
        .unwrap();
    assert_eq!(bad.status(), 401);
}

#[tokio::test]
async fn discover_and_list_projects() {
    let h = Harness::start("").await;
    let client = h.client(&h.member_token).await;
    let tools = client.list_all_tools().await.unwrap();
    assert!(tools.iter().any(|t| t.name == "list_projects"));
    let (v, is_error) = call(&client, "list_projects", json!({})).await;
    assert!(!is_error, "{v}");
    assert!(v["projects"].is_array());
    client.cancel().await.unwrap();
}

#[tokio::test]
async fn healthz_answers() {
    let h = Harness::start("").await;
    let v: serde_json::Value = reqwest::get(format!("{}/healthz", h.base)).await.unwrap().json().await.unwrap();
    assert_eq!(v["ok"], true);
    // without a token the machine's state stays private
    assert!(v.get("queued").is_none() && v.get("recipes_main").is_none(), "{v}");
    let v: serde_json::Value = reqwest::Client::new().get(format!("{}/healthz", h.base)).bearer_auth(&h.member_token).send().await.unwrap().json().await.unwrap();
    assert!(v.get("queued").is_some(), "{v}");
}

#[tokio::test]
async fn non_loopback_bind_refused() {
    let dir = tempfile::tempdir().unwrap();
    let mut cfg = base_config(dir.path(), "");
    cfg.server.bind = "0.0.0.0:0".into();
    let app = cimon_engine::App::open(cfg).unwrap();
    let err = cimon::serve::start(app, tokio_util::sync::CancellationToken::new()).await.unwrap_err();
    assert!(err.to_string().contains("loopback"), "{err}");
}
