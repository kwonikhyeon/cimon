//! Team-member onboarding (design §16.3): invite codes, the connect scripts the server
//! serves, and redeeming a code for a new user's token.

mod common;

use cimon_core::Role;
use common::*;
use serde_json::{json, Value};

#[tokio::test]
async fn invite_is_redeemed_once_and_the_scripts_carry_the_address() {
    let h = Harness::start("").await;
    let http = reqwest::Client::new();
    let redeem = |body: Value| http.post(format!("{}/connect/redeem", h.base)).json(&body).send();

    // the scripts are served with this server's address in them
    for path in ["connect.ps1", "connect.sh"] {
        let r = http.get(format!("{}/{path}", h.base)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let text = r.text().await.unwrap();
        assert!(text.contains(&format!("'{}'", h.base)), "{path} lacks the address");
        assert!(!text.contains("{{URL}}"), "{path}");
    }

    // one user per device: a name that already exists is not invited
    assert!(h.app.store.create_invite("member-b", Role::Member, "admin-a", 10).is_err());
    let (code, _) = h.app.store.create_invite("dev-laptop", Role::Member, "admin-a", 10).unwrap();
    assert_eq!(code.len(), 9, "XXXX-XXXX: {code}");

    let r = redeem(json!({"code": "ZZZZ-ZZZZ"})).await.unwrap();
    assert_eq!(r.status(), 403, "a wrong code");

    // typed loosely: lower case, a space for the dash, O for zero
    let typed = code.to_lowercase().replace('-', " ").replace('0', "o");
    let r = redeem(json!({"code": typed, "device": "LAPTOP-1"})).await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["user"], "dev-laptop");
    assert_eq!(v["role"], "member");
    assert_eq!(v["url"], h.base.as_str());
    let token = v["token"].as_str().unwrap().to_string();

    // the token signs in
    let hz: Value = http.get(format!("{}/healthz", h.base)).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert!(hz.get("queued").is_some(), "{hz}");

    // once only
    let r = redeem(json!({"code": code})).await.unwrap();
    assert_eq!(r.status(), 403, "a used code");

    // an expired code
    let (old, _) = h.app.store.create_invite("old-pc", Role::Member, "admin-a", 0).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let r = redeem(json!({"code": old})).await.unwrap();
    assert_eq!(r.status(), 403, "an expired code");
    assert!(h.app.store.user("old-pc").unwrap().is_none(), "no user for an expired invite");
}
