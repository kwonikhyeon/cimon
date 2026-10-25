//! Pages. maud escapes every interpolation; the only raw HTML is the comment
//! allow-list from `md::render`.

use crate::Session;
use cimon_core::{job_label, proposal_label};
use cimon_engine::App;
use cimon_recipe::diff::Risk;
use cimon_store::ProposalRow;
use maud::{html, Markup, PreEscaped, DOCTYPE};
use serde_json::Value;

pub const CSS: &str = r#"
:root{--bg:#f7f7f5;--panel:#fff;--ink:#1d1d1b;--muted:#6b6b66;--line:#e2e1dc;--accent:#2f5d8a;--pass:#1f7a3f;--fail:#b3261e;--warn:#9a6700;--na:#7a7a75;--code:#f2f1ec}
@media (prefers-color-scheme: dark){:root{--bg:#161615;--panel:#1f1f1d;--ink:#ecebe6;--muted:#a3a29c;--line:#34332f;--accent:#8ab4e0;--pass:#5cc882;--fail:#ff8a80;--warn:#e6b450;--na:#9a9993;--code:#262624}}
*{box-sizing:border-box}body{margin:0;background:var(--bg);color:var(--ink);font:15px/1.5 system-ui,-apple-system,"Segoe UI","Malgun Gothic",sans-serif}
a{color:var(--accent);text-decoration:none}a:hover{text-decoration:underline}
header{display:flex;flex-wrap:wrap;gap:16px;align-items:center;padding:10px 16px;border-bottom:1px solid var(--line);background:var(--panel)}
header .brand{font-weight:700}header nav{display:flex;gap:14px;flex-wrap:wrap}header .who{margin-left:auto;display:flex;gap:10px;align-items:center;color:var(--muted)}
main{max-width:1100px;margin:0 auto;padding:16px}
.panel{background:var(--panel);border:1px solid var(--line);border-radius:8px;padding:14px 16px;margin:12px 0}
h1{font-size:20px;margin:4px 0 8px}h2{font-size:16px;margin:4px 0 10px}
.meta{color:var(--muted);font-size:13px;display:flex;flex-wrap:wrap;gap:12px}
.mono,code,pre{font-family:ui-monospace,"Cascadia Code",Consolas,monospace;font-size:13px}
code{background:var(--code);padding:1px 4px;border-radius:4px}
pre{background:var(--code);padding:10px;border-radius:6px;overflow:auto;white-space:pre-wrap;word-break:break-word}
table{border-collapse:collapse;width:100%}th,td{text-align:left;padding:6px 8px;border-bottom:1px solid var(--line);vertical-align:top}th{color:var(--muted);font-weight:500;font-size:13px}
.tbl{overflow-x:auto}
.st{display:inline-block;padding:0 8px;border-radius:10px;font-size:12px;font-weight:600;border:1px solid currentColor}
.st.pass{color:var(--pass)}.st.fail,.st.error{color:var(--fail)}.st.na{color:var(--na)}.st.warn{color:var(--warn)}
.badge{display:inline-block;padding:0 6px;margin:0 3px;border-radius:4px;font-size:12px;background:var(--code)}
.badge.weaken{color:var(--fail);font-weight:700}.badge.perm{color:var(--warn);font-weight:700}
.banner{border-left:4px solid var(--warn);background:var(--panel);padding:10px 14px;margin:12px 0}.banner.fail{border-color:var(--fail)}.banner.ok{border-color:var(--pass)}
.tabs{display:flex;gap:4px;border-bottom:1px solid var(--line);margin:8px 0 12px;flex-wrap:wrap}.tabs a{padding:6px 12px;border:1px solid transparent;border-bottom:none;border-radius:6px 6px 0 0}.tabs a.on{border-color:var(--line);background:var(--panel);font-weight:600}
form.inline{display:inline}textarea,input[type=text],input[type=password]{width:100%;padding:8px;border:1px solid var(--line);border-radius:6px;background:var(--panel);color:var(--ink);font:inherit}
button{padding:6px 14px;border-radius:6px;border:1px solid var(--line);background:var(--panel);color:var(--ink);cursor:pointer;font:inherit}button.primary{background:var(--accent);border-color:var(--accent);color:#fff}button.danger{color:var(--fail)}button:disabled{opacity:.5;cursor:not-allowed}
.cond{display:grid;grid-template-columns:110px 1fr;gap:8px;padding:6px 0;border-bottom:1px dashed var(--line)}
.diff .add{color:var(--pass)}.diff .del{color:var(--fail)}.diff .hunk{color:var(--accent)}
.comment{border-bottom:1px solid var(--line);padding:8px 0}.comment.old{opacity:.65}
.from{color:var(--fail);text-decoration:line-through}.to{color:var(--pass)}
@media (max-width:640px){.cond{grid-template-columns:1fr}main{padding:12px 16px}}
"#;

fn layout(app: &App, s: Option<&Session>, title: &str, body: Markup) -> Markup {
    let warnings = cimon_engine::backup::warnings(app);
    html! {
        (DOCTYPE)
        html lang="ko" {
            head {
                meta charset="utf-8";
                meta name="viewport" content="width=device-width, initial-scale=1";
                title { (title) " · cimon" }
                link rel="stylesheet" href="/ui/style.css";
            }
            body {
                header {
                    a.brand href="/ui/" { "cimon" }
                    @if s.is_some() {
                        nav {
                            a href="/ui/proposals" { "제안" }
                            a href="/ui/history" { "이력" }
                            a href="/ui/recipes/main/tree" { "레시피" }
                        }
                    }
                    @if let Some(s) = s {
                        div.who {
                            span { (s.user.name) " · " (if s.user.is_admin() { "관리자" } else { "팀원" }) }
                            form.inline method="post" action="/ui/logout" {
                                input type="hidden" name="csrf" value=(s.csrf);
                                button type="submit" { "로그아웃" }
                            }
                        }
                    }
                }
                main {
                    @if s.is_some() {
                        @for w in &warnings { div.banner { strong { "백업 " } (w) } }
                    }
                    (body)
                }
            }
        }
    }
}

pub fn login_page(err: Option<&str>) -> Markup {
    html! {
        (DOCTYPE)
        html lang="ko" {
            head { meta charset="utf-8"; meta name="viewport" content="width=device-width, initial-scale=1"; title { "로그인 · cimon" } link rel="stylesheet" href="/ui/style.css"; }
            body { main {
                div.panel {
                    h1 { "cimon 리뷰 화면" }
                    @if let Some(e) = err { div.banner.fail { (e) } }
                    p { "Claude 에게 " code { "리뷰 화면 로그인 링크" } " 를 요청하라(" code { "web_login" } "). 링크는 5분 동안 한 번만 쓸 수 있다." }
                    details {
                        summary { "토큰으로 들어가기" }
                        form method="post" action="/ui/login" {
                            input type="password" name="token" placeholder="cimon token" autocomplete="off";
                            p { button.primary type="submit" { "들어가기" } }
                        }
                    }
                }
            } }
        }
    }
}

pub fn message_page(msg: &str) -> Markup {
    html! {
        (DOCTYPE)
        html lang="ko" { head { meta charset="utf-8"; title { "cimon" } link rel="stylesheet" href="/ui/style.css"; }
            body { main { div.panel { pre { (msg) } p { a href="/ui/" { "처음으로" } } } } } }
    }
}

fn status_pill(s: &str) -> Markup {
    let cls = match s {
        "pass" | "promoted" => "pass",
        "fail" | "error" | "inconclusive" => "fail",
        "open" => "warn",
        _ => "na",
    };
    html! { span.st.(cls) { (s) } }
}

fn rref_seg(r: &str) -> String {
    r.replace('/', "~")
}

fn short(c: &str) -> &str {
    &c[..c.len().min(7)]
}

pub fn home(app: &App, s: &Session) -> Markup {
    let open = app.store.proposals(Some("open"), 50).unwrap_or_default();
    let promoted = app.store.proposals(Some("promoted"), 10).unwrap_or_default();
    let jobs = app.store.recent_jobs(15).unwrap_or_default();
    layout(app, Some(s), "처음", html! {
        div.panel {
            h2 { "열린 제안 " (open.len()) }
            (proposal_table(&open))
        }
        div.panel {
            h2 { "최근 반영" }
            (proposal_table(&promoted))
        }
        div.panel {
            h2 { "최근 작업" }
            div.tbl { table {
                tr { th { "작업" } th { "프로젝트" } th { "도구" } th { "상태" } th { "판정" } th { "요청" } }
                @for j in &jobs {
                    tr {
                        td { a.mono href=(format!("/ui/jobs/{}", job_label(j.id))) { (job_label(j.id)) } @if j.trial { " " span.badge { "시도" } } }
                        td { (j.project) }
                        td { (j.tool) }
                        td { (j.state.as_str()) }
                        td { @if let Some(v) = &j.verdict { (status_pill(v)) } }
                        td { (j.created_by) }
                    }
                }
            } }
        }
    })
}

fn proposal_table(rows: &[ProposalRow]) -> Markup {
    html! {
        div.tbl { table {
            tr { th { "제안" } th { "제목" } th { "작성" } th { "상태" } th { "갱신" } }
            @for p in rows {
                tr {
                    td { a.mono href=(format!("/ui/proposals/{}", proposal_label(p.id))) { (proposal_label(p.id)) } br; span.mono { (p.branch) } }
                    td { (p.title) }
                    td { (p.author) }
                    td { (status_pill(&p.status)) }
                    td.mono { (p.updated_at) }
                }
            }
        } }
    }
}

pub fn proposals(app: &App, s: &Session, status: Option<&str>) -> Markup {
    let filter = match status {
        Some("all") => None,
        Some(x) => Some(x),
        None => Some("open"),
    };
    let rows = app.store.proposals(filter, 200).unwrap_or_default();
    layout(app, Some(s), "제안", html! {
        div.panel {
            h1 { "제안" }
            p.meta { "시도 브랜치(recipe/*) 하나가 제안 하나. 편집은 Claude 의 recipe_edit 으로, 반영은 여기서." }
            div.tabs {
                @for (k, label) in [("open", "열림"), ("promoted", "반영됨"), ("closed", "닫힘"), ("all", "전체")] {
                    a class=(if filter.unwrap_or("all") == k { "on" } else { "" }) href=(format!("/ui/proposals?status={k}")) { (label) }
                }
            }
            (proposal_table(&rows))
        }
    })
}

fn risk_badge(r: &str) -> Markup {
    let cls = match r {
        "동작·완화" => "weaken",
        "권한" => "perm",
        _ => "",
    };
    html! { span.badge.(cls) { "[" (r) "]" } }
}

fn risk_of(v: &Value) -> Option<String> {
    v.as_str().and_then(|r| match r {
        "permission" => Some(Risk::Permission.label()),
        "exec" => Some(Risk::Exec.label()),
        "supply" => Some(Risk::Supply.label()),
        "behavior" => Some(Risk::Behavior.label()),
        "weaken" => Some(Risk::Weaken.label()),
        _ => None,
    }).map(str::to_string)
}

fn jv(v: &Value) -> String {
    match v {
        Value::Null => "—".into(),
        Value::String(s) => s.clone(),
        other => other.to_string(),
    }
}

pub fn proposal(app: &App, s: &Session, p: &ProposalRow, tab: &str, msg: Option<&str>) -> Markup {
    let v = cimon_engine::proposals::view(app, p);
    let head = v["head"].as_str().map(str::to_string);
    let conds = v["conditions"].as_array().cloned().unwrap_or_default();
    let all_pass = conds.iter().all(|c| matches!(c["state"].as_str(), Some("pass") | Some("na")));
    let hard_fail = conds.iter().any(|c| !matches!(c["state"].as_str(), Some("pass") | Some("na")) && c["overridable"] == false);
    let label = proposal_label(p.id);
    let risks: Vec<String> = v["risks"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    layout(app, Some(s), &label, html! {
        p.meta { a href="/ui/proposals" { "제안" } " / " span.mono { (label) " " (p.branch) } }
        @if msg == Some("promoted") { div.banner.ok { strong { "반영됨 — main " (p.promoted_commit.as_deref().map(short).unwrap_or("")) } " 다음 작업부터 이 레시피로 돈다." } }
        @if p.status == "promoted" && p.override_reason.is_some() { div.banner.fail { strong { "조건을 덮어쓰고 반영함 — 사유: " } (p.override_reason.clone().unwrap_or_default()) } }
        @if p.status == "closed" { div.banner { strong { "닫힘" } " — " (p.close_reason.clone().unwrap_or_default()) " 커밋은 refs/cimon/closed/" (label) " 에 30일 남는다. 같은 브랜치 이름으로 다시 고치면 새 제안이 열린다." } }
        div.panel {
            h1 { (p.title) " " (status_pill(&p.status)) }
            div.meta {
                span { "작성 " strong { (p.author) } }
                span { (p.created_at) }
                @if let Some(h) = &head { span { "head " span.mono { (short(h)) } } }
                span { "기준 main " span.mono { (short(&p.base_commit)) } }
            }
            @if let Some(d) = &p.description { p { (d) } }
            @if risks.iter().any(|r| r == "동작·완화") { div.banner.fail { strong { "판정을 느슨하게 하는 변경이 있다" } " — 의미 차이의 [동작·완화] 를 먼저 보라." } }
        }
        @if p.status == "open" {
            div.panel {
                h2 { "반영 조건" }
                @for c in &conds {
                    div.cond {
                        div { (status_pill(c["state"].as_str().unwrap_or("?"))) " " span.mono { (c["name"].as_str().unwrap_or("")) } }
                        div { (c["detail"].as_str().unwrap_or("")) }
                    }
                }
                @if let Some(h) = &head {
                    form method="post" action=(format!("/ui/proposals/{label}/promote")) {
                        input type="hidden" name="csrf" value=(s.csrf);
                        input type="hidden" name="head" value=(h);
                        @if !all_pass && !hard_fail && s.user.is_admin() {
                            p { label for="override_reason" { "조건을 덮어쓰는 사유(10자 이상, 이력에 영구히 남는다)" } textarea id="override_reason" name="override_reason" rows="2" {} }
                        }
                        p {
                            @if !s.user.is_admin() { button.primary type="submit" disabled { "반영(관리자만)" } }
                            @else if hard_fail { button.primary type="submit" disabled { "반영 불가 — 정적 검사·fast-forward" } }
                            @else if all_pass { button.primary type="submit" { "반영" } }
                            @else { button.primary type="submit" { "사유를 적고 반영" } }
                            " " span.meta { "반영할 head: " span.mono { (short(h)) } " — 서버가 반영 순간 조건을 다시 계산한다" }
                        }
                    }
                    @if p.author == s.user.name || s.user.is_admin() {
                        form method="post" action=(format!("/ui/proposals/{label}/close")) {
                            input type="hidden" name="csrf" value=(s.csrf);
                            p { input type="text" name="reason" placeholder="닫는 사유(선택)"; }
                            p { button.danger type="submit" { "닫기" } }
                        }
                    }
                }
            }
        }
        div.panel {
            div.tabs {
                @for (k, lbl) in [("semantic", "의미 차이"), ("files", "파일 차이"), ("trials", "시도 실행"), ("comments", "댓글")] {
                    a class=(if tab == k { "on" } else { "" }) href=(format!("/ui/proposals/{label}?tab={k}")) { (lbl) }
                }
            }
            @match tab {
                "files" => (files_tab(app, p, head.as_deref())),
                "trials" => (trials_tab(&v)),
                "comments" => (comments_tab(&v, s, &label)),
                _ => (semantic_tab(&v)),
            }
        }
    })
}

fn semantic_tab(v: &Value) -> Markup {
    let d = &v["semantic_diff"];
    if let Some(e) = d.get("error").and_then(|e| e.as_str()) {
        return html! { div.banner.fail { strong { "의미 차이 계산 실패: " } (e) " — 파일 차이를 보라." } };
    }
    let projects = d["projects"].as_array().cloned().unwrap_or_default();
    let catalog = d["catalog"].as_array().cloned().unwrap_or_default();
    let risks: Vec<String> = v["risks"].as_array().map(|a| a.iter().filter_map(|x| x.as_str().map(str::to_string)).collect()).unwrap_or_default();
    html! {
        p { "위험 표시 " @for r in &risks { (risk_badge(r)) } @if risks.is_empty() { span.meta { "없음" } } }
        @if projects.is_empty() && catalog.is_empty() {
            p.meta { "영향받는 프로젝트 없음 — 바뀐 파일: " (d["files"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()) }
        }
        p { "영향받는 프로젝트: " strong { (projects.iter().filter_map(|p| p["id"].as_str()).collect::<Vec<_>>().join(", ")) } }
        @for c in &catalog {
            p { span.badge { "카탈로그" } " " (c["kind"].as_str().unwrap_or("")) " " (c["change"].as_str().unwrap_or("")) " " (risk_badge(Risk::Supply.label())) }
        }
        @for pd in &projects {
            div.panel {
                h2 { (pd["id"].as_str().unwrap_or("")) " " span.meta { "(" (pd["change"].as_str().unwrap_or("")) ")" } }
                @if let Some(e) = pd["error"].as_str() { div.banner.fail { (e) } }
                @if pd["project_toml_changed"] == true {
                    p { span.badge.perm { "[권한]" } " project.toml 이 바뀌었다 — 시도 실행은 main 의 것을 쓰므로 시험할 수 없다" }
                    (fields(&pd["project_fields"]))
                }
                @if pd["settings"].as_array().is_some_and(|a| !a.is_empty()) { h2 { "설정" } (fields(&pd["settings"])) }
                @for st in pd["steps"].as_array().cloned().unwrap_or_default() {
                    div {
                        strong { (match st["change"].as_str() { Some("added") => "단계 추가", Some("removed") => "단계 삭제", _ => "단계 변경" }) }
                        " " code { (st["id"].as_str().unwrap_or("")) }
                        " " span.meta { (st["kind"].as_str().unwrap_or("")) }
                        @for r in st["risks"].as_array().cloned().unwrap_or_default() { @if let Some(l) = risk_of(&r) { (risk_badge(&l)) } }
                        (fields(&st["fields"]))
                    }
                }
                @if let Some(cs) = pd["changed_steps"].as_array() { @if !cs.is_empty() { p.meta { "시도 실행에 포함해야 할 단계: " (cs.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")) @if pd["requires_default_run"] == true { " + 기본 run" } } } }
            }
        }
    }
}

fn fields(v: &Value) -> Markup {
    let list = v.as_array().cloned().unwrap_or_default();
    html! {
        @if !list.is_empty() {
            div.tbl { table {
                @for f in &list {
                    tr {
                        td.mono { (f["field"].as_str().unwrap_or("")) }
                        td { span.from.mono { (jv(&f["from"])) } " → " span.to.mono { (jv(&f["to"])) } }
                        td { @if let Some(l) = risk_of(&f["risk"]) { (risk_badge(&l)) } }
                    }
                }
            } }
        }
    }
}

fn files_tab(app: &App, p: &ProposalRow, head: Option<&str>) -> Markup {
    let to = head.map(str::to_string).or(p.promoted_commit.clone());
    let Some(to) = to else { return html! { p.meta { "브랜치가 없다" } } };
    let base = app.recipes.git.merge_base(&p.base_commit, &to).unwrap_or(p.base_commit.clone());
    let diff = app.recipes.diff_text(&base, &to, None).unwrap_or_else(|e| e.to_string());
    let lines: Vec<&str> = diff.lines().take(5000).collect();
    html! {
        p.meta { span.mono { (short(&base)) " → " (short(&to)) } " · unified diff" }
        pre.diff {
            @for l in &lines {
                @let cls = if l.starts_with("+++") || l.starts_with("---") || l.starts_with("diff ") { "hunk" } else if l.starts_with('+') { "add" } else if l.starts_with('-') { "del" } else if l.starts_with("@@") { "hunk" } else { "" };
                span class=(cls) { (l) } "\n"
            }
        }
    }
}

fn trials_tab(v: &Value) -> Markup {
    let trials = v["trials"].as_array().cloned().unwrap_or_default();
    html! {
        p.meta { "반영 조건은 지금 head 로 돌았고 바뀐 단계를 모두 포함한 가장 최근 작업 하나만 본다." }
        div.tbl { table {
            tr { th { "작업" } th { "프로젝트" } th { "도구 · 단계" } th { "판정" } th { "레시피 커밋" } }
            @for t in &trials {
                tr style=(if t["current_head"] == true { "" } else { "opacity:.6" }) {
                    td { a.mono href=(format!("/ui/jobs/{}", t["job_id"].as_str().unwrap_or(""))) { (t["job_id"].as_str().unwrap_or("")) } }
                    td { (t["project"].as_str().unwrap_or("")) }
                    td { (t["tool"].as_str().unwrap_or("")) " " span.meta { (t["steps"].as_array().map(|a| a.iter().filter_map(|x| x.as_str()).collect::<Vec<_>>().join(", ")).unwrap_or_default()) } }
                    td { @if let Some(v) = t["verdict"].as_str() { (status_pill(v)) } @else { (t["state"].as_str().unwrap_or("")) } }
                    td.mono { (t["recipe_commit"].as_str().map(short).unwrap_or("")) @if t["current_head"] != true { " (이전 커밋)" } }
                }
            }
        } }
    }
}

fn comments_tab(v: &Value, s: &Session, label: &str) -> Markup {
    let comments = v["comments"].as_array().cloned().unwrap_or_default();
    html! {
        @for c in &comments {
            div class=(if c["outdated"] == true { "comment old" } else { "comment" }) {
                div.meta { strong { (c["author"].as_str().unwrap_or("")) } span { (c["at"].as_str().unwrap_or("")) } span.mono { (c["head"].as_str().map(short).unwrap_or("")) } @if c["outdated"] == true { span { "이전 커밋에 단 댓글" } } }
                div { (PreEscaped(crate::md::render(c["body"].as_str().unwrap_or("")))) }
            }
        }
        form method="post" action=(format!("/ui/proposals/{label}/comment")) {
            input type="hidden" name="csrf" value=(s.csrf);
            p { textarea name="body" rows="3" placeholder="리뷰 의견 — Claude 가 recipe_proposal 로 읽을 수 있다" {} }
            p { button.primary type="submit" { "댓글 달기" } }
        }
    }
}

pub fn history(app: &App, s: &Session) -> Markup {
    let commits = app.recipes.history("main", None, 50).unwrap_or_default();
    let promoted = app.store.proposals(Some("promoted"), 500).unwrap_or_default();
    layout(app, Some(s), "이력", html! {
        div.panel {
            h1 { "main 이력" }
            p.meta { "어느 제안으로 들어왔는지, 조건을 덮어썼는지, 되돌렸는지가 함께 남는다." }
            div.tbl { table {
                tr { th { "커밋" } th { "내용" } th { "작성" } th { "언제" } th {} }
                @for c in &commits {
                    @let prop = promoted.iter().find(|p| p.promoted_commit.as_deref() == Some(c.commit.as_str()));
                    tr {
                        td.mono { (short(&c.commit)) }
                        td {
                            (c.subject)
                            @if let Some(p) = prop {
                                div.meta { "제안 " a href=(format!("/ui/proposals/{}", proposal_label(p.id))) { (proposal_label(p.id)) } " " (p.branch) }
                                @if let Some(r) = &p.override_reason { div { span.badge.weaken { "조건 덮어씀" } " " (r) } }
                            }
                            @if c.subject.starts_with("되돌리기") { div.meta { (c.body) } }
                        }
                        td { (c.author) }
                        td.mono { (c.date) }
                        td {
                            @if s.user.is_admin() {
                                details {
                                    summary { "되돌리기" }
                                    form method="post" action="/ui/history/revert" {
                                        input type="hidden" name="csrf" value=(s.csrf);
                                        input type="hidden" name="commit" value=(c.commit);
                                        textarea name="reason" rows="2" placeholder="되돌리는 사유(필수)" {}
                                        button.danger type="submit" { "되돌리는 커밋 만들기" }
                                    }
                                }
                            }
                        }
                    }
                }
            } }
        }
    })
}

pub fn tree(app: &App, s: &Session, rref: &str, commit: &str, shown: &cimon_recipestore::ShowResult) -> Markup {
    layout(app, Some(s), "레시피", html! {
        div.panel {
            p.meta { span.mono { (rref) " @ " (short(commit)) } }
            @match shown {
                cimon_recipestore::ShowResult::Dir { path, entries } => {
                    h1 { "/" (path) }
                    ul {
                        @for e in entries {
                            @let p = if path.is_empty() { e.trim_end_matches('/').to_string() } else { format!("{path}/{}", e.trim_end_matches('/')) };
                            li { a href=(format!("/ui/recipes/{}/tree/{}", rref_seg(rref), p)) { (e) } }
                        }
                    }
                    @if path.starts_with("projects/") && path.matches('/').count() == 1 {
                        p { a href=(format!("/ui/recipes/{}/projects/{}", rref_seg(rref), path.trim_start_matches("projects/"))) { "겹친 레시피 보기" } }
                    }
                }
                cimon_recipestore::ShowResult::File { path, content } => {
                    h1.mono { (path) }
                    pre { (content) }
                }
            }
        }
    })
}

pub fn project(app: &App, s: &Session, rref: &str, commit: &str, r: &cimon_recipe::Resolved) -> Markup {
    layout(app, Some(s), &r.id, html! {
        div.panel {
            h1 { (r.id) }
            p.meta { span.mono { (rref) " @ " (short(commit)) } " · 프리셋·include 를 겹친 결과" }
            h2 { "툴체인" }
            div.tbl { table { @for (k, t) in &r.toolchains { tr { td.mono { (k) } td.mono { (serde_json::to_string(t).unwrap_or_default()) } } } } }
            h2 { "파라미터" }
            div.tbl { table { @for (k, p) in &r.params { tr { td.mono { (k) } td { (p.type_) } td { (p.applies_to.clone().unwrap_or_default().join(", ")) } td { (p.description.clone().unwrap_or_default()) } } } } }
            h2 { "단계" }
            div.tbl { table {
                tr { th { "id" } th { "종류" } th { "태그" } th { "기본" } th { "needs" } th { "after" } th { "판정" } th { "산출물" } th { "출처" } }
                @for st in &r.steps {
                    tr {
                        td.mono { (st.id()) }
                        td { (st.kind.as_str()) }
                        td { (st.def.tags.join(", ")) }
                        td { @if st.kind != cimon_recipe::Kind::Setup && st.is_default() { "●" } }
                        td { (st.def.needs.join(", ")) }
                        td { (st.def.after.join(", ")) }
                        td { (st.def.judge.as_ref().map(|j| j.type_.clone()).unwrap_or_else(|| "exit".into())) @if st.def.uncounted { " (uncounted)" } }
                        td { (st.def.artifact.iter().map(|a| a.name.clone()).collect::<Vec<_>>().join(", ")) }
                        td.mono { (st.origin) }
                    }
                }
            } }
            @if !r.fixtures.is_empty() { h2 { "fixture" } p { (r.fixtures.keys().cloned().collect::<Vec<_>>().join(", ")) } }
        }
    })
}

pub fn job(app: &App, s: &Session, j: &cimon_store::JobRow) -> Markup {
    let view = if j.state.is_terminal() && j.result_json.is_some() { cimon_engine::result::view(app, j, None) } else { cimon_engine::result::progress(app, j) };
    let steps = view["steps"].as_array().cloned().unwrap_or_default();
    let first_bad = steps.iter().find(|st| matches!(st["result"].as_str(), Some("fail") | Some("inconclusive"))).and_then(|st| st["id"].as_str().map(str::to_string));
    let log_tail = first_bad.as_ref().and_then(|sid| std::fs::read(app.layout.step_log(j.id, sid)).ok()).map(|b| {
        let t = String::from_utf8_lossy(&b).to_string();
        let lines: Vec<&str> = t.lines().collect();
        lines[lines.len().saturating_sub(200)..].iter().map(|l| cimon_runner_strip(l)).collect::<Vec<_>>().join("\n")
    });
    layout(app, Some(s), &job_label(j.id), html! {
        div.panel {
            h1 { (job_label(j.id)) " " @if let Some(v) = view["verdict"].as_str() { (status_pill(v)) } @else { (status_pill(j.state.as_str())) } @if j.trial { " " span.badge { "시도 실행" } } }
            p { (view["summary"].as_str().unwrap_or("")) }
            div.meta {
                span { "도구 " (j.tool) }
                span { "커밋 " span.mono { (j.ref_.clone().unwrap_or_default()) " " (j.commit_sha.as_deref().map(short).unwrap_or("")) } }
                span { "레시피 " span.mono { (j.recipe_ref) " @ " (j.recipe_commit.as_deref().map(short).unwrap_or("")) } }
                @if let Some(c) = view["failure_class"].as_str() { span { "실패 분류 " strong { (c) } } }
            }
        }
        @if let Some(ex) = view["excluded"].as_array() { @if !ex.is_empty() { div.banner { strong { "제외 " (ex.len()) } @for e in ex { br; span.mono { (e["step"].as_str().unwrap_or("")) } " — " (e["reason"].as_str().unwrap_or("")) } } } }
        @if let Some(ws) = view["warnings"].as_array() { @for w in ws { div.banner { strong { (w["source"].as_str().unwrap_or("")) " " } (w["message"].as_str().unwrap_or("")) } } }
        div.panel {
            h2 { "단계" }
            div.tbl { table {
                tr { th { "단계" } th { "결과" } th { "시간" } th { "내용" } }
                @for st in &steps {
                    tr {
                        td.mono { (st["id"].as_str().unwrap_or("")) }
                        td { (status_pill(st["result"].as_str().unwrap_or(""))) }
                        td { (st["seconds"].as_f64().map(|x| format!("{x:.0}s")).unwrap_or_default()) }
                        td { (st["reason"].as_str().unwrap_or("")) @if let Some(t) = st.get("tests") { " " span.meta { (t.to_string()) } } }
                    }
                }
            } }
        }
        @if let Some(arts) = view["artifacts"].as_array() { @if !arts.is_empty() {
            div.panel { h2 { "산출물" }
                div.tbl { table {
                    tr { th { "이름" } th { "크기" } th { "sha256" } th {} }
                    @for a in arts {
                        tr {
                            td { (a["name"].as_str().unwrap_or("")) }
                            td { (a["bytes"].as_i64().unwrap_or(0)) }
                            td.mono { (a["sha256"].as_str().map(|x| &x[..x.len().min(16)]).unwrap_or("")) "…" }
                            td { @if let Some(u) = a["url"].as_str() { a href=(u) { "받기" } } @else { (a["expired"].as_str().unwrap_or("")) } }
                        }
                    }
                } }
            }
        } }
        @if let (Some(sid), Some(t)) = (&first_bad, &log_tail) { div.panel { h2 { "로그 끝 — " (sid) } pre { (t) } } }
    })
}

/// Strips terminal colour codes from a log line.
fn cimon_runner_strip(l: &str) -> String {
    static RE: std::sync::OnceLock<regex::Regex> = std::sync::OnceLock::new();
    RE.get_or_init(|| regex::Regex::new(r"\x1b\[[0-9;?]*[ -/]*[@-~]").unwrap()).replace_all(l, "").into_owned()
}
