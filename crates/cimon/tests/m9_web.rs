//! M9: the review screen (review-ui §14).

mod common;

use cimon_recipe::tree::MemTree;
use common::*;
use serde_json::json;

const RECIPE: &str = "schema = 1\nextends = [\"shared\"]\n[[step]]\nid = \"test\"\nkind = \"test\"\nrun = [\"cmd\", \"/c\", \"echo\", \"test result: ok. 3 passed; 0 failed; 0 ignored\"]\njudge = { type = \"cargo-test\", min_tests = 2 }\n";

fn tree() -> MemTree {
    let mut t = MemTree::new();
    t.insert("toolchains.toml", "schema = 1\n");
    t.insert("presets/shared.toml", "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\", \"build\"]\n");
    for p in ["m9a", "m9b"] {
        t.insert(format!("projects/{p}/project.toml"), format!("schema = 1\nid = \"{p}\"\nrepo = \"https://example.invalid/{p}.git\"\nallowed_refs = [\"main\"]\ndefault_ref = \"main\"\n"));
        t.insert(format!("projects/{p}/recipe.toml"), RECIPE);
    }
    t.insert("docs/notes.md", "<script>alert('x')</script>");
    t
}

fn browser() -> reqwest::Client {
    reqwest::Client::builder().cookie_store(true).redirect(reqwest::redirect::Policy::limited(5)).build().unwrap()
}

fn csrf(html: &str) -> String {
    let i = html.find("name=\"csrf\" value=\"").expect("csrf field") + "name=\"csrf\" value=\"".len();
    html[i..].split('"').next().unwrap().to_string()
}

async fn login(h: &Harness, token: &str) -> reqwest::Client {
    let c = h.client(token).await;
    let (v, err) = call(&c, "web_login", json!({})).await;
    assert!(!err, "{v}");
    let url = v["url"].as_str().unwrap().to_string();
    let b = browser();
    let r = b.get(&url).send().await.unwrap();
    assert_eq!(r.status(), 200, "{}", r.text().await.unwrap());
    // the code is single use
    let again = browser().get(&url).send().await.unwrap();
    assert_eq!(again.status(), 401);
    c.cancel().await.unwrap();
    b
}

#[tokio::test]
async fn review_screen_end_to_end() {
    repos_dir();
    make_repo("m9a", &[("x", "1")]);
    make_repo("m9b", &[("x", "1")]);
    let h = Harness::start("[web]\nenabled = true\n").await;
    let main = h.app.recipes.import_init(&tree(), "admin-a", "init").unwrap();

    // without a session: login page with 401, CSP on every response
    let r = browser().get(format!("{}/ui/", h.base)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    let csp = r.headers().get("content-security-policy").unwrap().to_str().unwrap().to_string();
    assert!(csp.contains("default-src 'self'") && csp.contains("frame-ancestors 'none'"), "{csp}");
    assert!(r.text().await.unwrap().contains("web_login"));

    let admin = login(&h, &h.admin_token).await;
    let member = login(&h, &h.member_token).await;

    // a preset change affects both projects; lowering min_tests is a weakening; run changes are [실행]
    let mcp = h.task_client(&h.member_token).await;
    let (e, err) = call_any(&mcp, "recipe_edit", json!({"branch": "recipe/preset", "base": main, "message": "preset + weaker test", "changes": [
        {"path": "presets/shared.toml", "content": "schema = 1\n[[step]]\nid = \"build\"\nkind = \"build\"\nrun = [\"cmd\", \"/c\", \"echo\", \"build2\"]\n"},
        {"path": "projects/m9a/recipe.toml", "content": RECIPE.replace("min_tests = 2", "min_tests = 1")}
    ]})).await;
    assert!(!err, "{e}");
    let label = e["proposal"].as_str().unwrap().to_string();
    let head = e["commit"].as_str().unwrap().to_string();
    let page = admin.get(format!("{}/ui/proposals/{label}", h.base)).send().await.unwrap().text().await.unwrap();
    assert!(page.contains("영향받는 프로젝트: <strong>m9a, m9b</strong>"), "{page}");
    assert!(page.contains("[실행]"), "run change badge");
    assert!(page.contains("[동작·완화]"), "weakening badge");
    assert!(page.contains("판정을 느슨하게 하는 변경"));
    // the conditions on screen are the same as recipe_proposal's
    let (pv, _) = call_any(&mcp, "recipe_proposal", json!({"proposal": label})).await;
    for c in pv["conditions"].as_array().unwrap() {
        let name = c["name"].as_str().unwrap();
        let state = c["state"].as_str().unwrap();
        assert!(page.contains(&format!("{state}</span> <span class=\"mono\">{name}</span>")), "condition {name}={state} missing on the page");
    }
    // file diff and a script in a recipe file stay text
    let files = admin.get(format!("{}/ui/proposals/{label}?tab=files", h.base)).send().await.unwrap().text().await.unwrap();
    assert!(files.contains("build2"));
    let notes = admin.get(format!("{}/ui/recipes/main/tree/docs/notes.md", h.base)).send().await.unwrap().text().await.unwrap();
    assert!(notes.contains("&lt;script&gt;") && !notes.contains("<script>alert"), "{notes}");

    // comments: escaped, read back by recipe_proposal
    let token = csrf(&page);
    let r = member.post(format!("{}/ui/proposals/{label}/comment", h.base)).form(&[("csrf", csrf(&member.get(format!("{}/ui/proposals/{label}", h.base)).send().await.unwrap().text().await.unwrap()).as_str()), ("body", "**확인** <script>alert(1)</script>")]).send().await.unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let c = admin.get(format!("{}/ui/proposals/{label}?tab=comments", h.base)).send().await.unwrap().text().await.unwrap();
    assert!(c.contains("<strong>확인</strong> &lt;script&gt;"), "{c}");
    let (pv, _) = call_any(&mcp, "recipe_proposal", json!({"proposal": label})).await;
    assert_eq!(pv["comments"][0]["author"], "member-b");

    // CSRF is required
    let r = admin.post(format!("{}/ui/proposals/{label}/promote", h.base)).form(&[("head", head.as_str())]).send().await.unwrap();
    assert_eq!(r.status(), 403);
    // members cannot promote
    let mtok = csrf(&member.get(format!("{}/ui/proposals/{label}", h.base)).send().await.unwrap().text().await.unwrap());
    let r = member.post(format!("{}/ui/proposals/{label}/promote", h.base)).form(&[("csrf", mtok.as_str()), ("head", head.as_str())]).send().await.unwrap();
    assert_eq!(r.status(), 403);
    // the proposal changes after the admin opened it: the old head is refused
    let (e2, err) = call_any(&mcp, "recipe_edit", json!({"branch": "recipe/preset", "base": head, "message": "more", "changes": [{"path": "docs/x.md", "content": "x"}]})).await;
    assert!(!err, "{e2}");
    let r = admin.post(format!("{}/ui/proposals/{label}/promote", h.base)).form(&[("csrf", token.as_str()), ("head", head.as_str())]).send().await.unwrap();
    assert_eq!(r.status(), 409);
    assert!(r.text().await.unwrap().contains("recipe_conflict"));
    // no trial and no reason: refused; with a reason: promoted and shown
    let new_head = e2["commit"].as_str().unwrap();
    let r = admin.post(format!("{}/ui/proposals/{label}/promote", h.base)).form(&[("csrf", token.as_str()), ("head", new_head)]).send().await.unwrap();
    assert_eq!(r.status(), 409);
    assert!(r.text().await.unwrap().contains("promote_conditions_unmet"));
    let r = admin
        .post(format!("{}/ui/proposals/{label}/promote", h.base))
        .form(&[("csrf", token.as_str()), ("head", new_head), ("override_reason", "출력 문구만 바뀐다 — 시험 생략")])
        .send()
        .await
        .unwrap();
    assert!(r.status().is_success(), "{}", r.status());
    let page = r.text().await.unwrap();
    assert!(page.contains("반영됨"), "{page}");
    assert!(page.contains("조건을 덮어쓰고 반영함"), "{page}");
    assert_eq!(h.app.recipes.main_commit().unwrap(), new_head);
    // history shows the override
    let hist = admin.get(format!("{}/ui/history", h.base)).send().await.unwrap().text().await.unwrap();
    assert!(hist.contains("조건 덮어씀"));
    // job page
    let t = start_task(&mcp, "build", json!({"project": "m9a"})).await;
    let (_, v, _) = wait_task(&mcp, &t, 120).await;
    let jp = admin.get(format!("{}/ui/jobs/{}", h.base, v["job_id"].as_str().unwrap())).send().await.unwrap().text().await.unwrap();
    assert!(jp.contains("pass"), "{jp}");
    mcp.cancel().await.unwrap();
}

#[tokio::test]
async fn login_code_expires_and_web_off() {
    let h = Harness::start("[web]\nenabled = true\nlogin_code_minutes = 1\n").await;
    let code = h.app.store.create_login_code("member-b", 1).unwrap();
    h.app.store.tx(|tx| {
        tx.execute("UPDATE login_codes SET expires_at = '2000-01-01T00:00:00Z'", [])?;
        Ok(())
    })
    .unwrap();
    let r = browser().get(format!("{}/ui/login?code={code}", h.base)).send().await.unwrap();
    assert_eq!(r.status(), 401);
    // disabling the user ends its sessions
    let b = login(&h, &h.member_token).await;
    assert_eq!(b.get(format!("{}/ui/", h.base)).send().await.unwrap().status(), 200);
    h.app.store.disable_user("member-b").unwrap();
    assert_eq!(b.get(format!("{}/ui/", h.base)).send().await.unwrap().status(), 401);

    let off = Harness::start("").await;
    let c = off.client(&off.member_token).await;
    let (v, err) = call(&c, "web_login", json!({})).await;
    assert!(err);
    assert_eq!(v["error"], "web_disabled");
    assert_eq!(reqwest::get(format!("{}/ui/", off.base)).await.unwrap().status(), 404);
    c.cancel().await.unwrap();
}

async fn call_any<S: rmcp::Service<rmcp::RoleClient>>(c: &rmcp::service::RunningService<rmcp::RoleClient, S>, name: &str, args: serde_json::Value) -> (serde_json::Value, bool) {
    let mut p = rmcp::model::CallToolRequestParams::new(name.to_string());
    if let serde_json::Value::Object(m) = args {
        p = p.with_arguments(m);
    }
    match c.call_tool_once(p).await.unwrap() {
        rmcp::model::CallToolResponse::Complete(r) => (r.structured_content.unwrap_or_default(), r.is_error.unwrap_or(false)),
        other => panic!("{other:?}"),
    }
}
