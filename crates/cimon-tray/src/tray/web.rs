//! 「웹 화면 열기」: a one-time login link from `POST /login-link` with this user's token,
//! opened in the default browser (docs/tray.md §5.4).

use super::wide;
use windows_sys::Win32::UI::Shell::ShellExecuteW;
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

/// The link, or what to tell the user.
pub fn login_link() -> Result<String, String> {
    let url = super::settings::url();
    if !crate::model::token_allowed(&url) {
        return Err(format!("{url} 로는 토큰을 보내지 않는다 — 127.0.0.1 또는 https 주소여야 한다"));
    }
    let token = super::token::read().ok_or("이 PC 에 cimon 토큰이 없다 — connect 스크립트로 연결하면 웹 화면을 열 수 있다")?;
    let r = super::poll::client().post(format!("{url}/login-link")).bearer_auth(token).send().map_err(|e| format!("cimon 에 닿지 않는다: {e}"))?;
    let status = r.status();
    let v: serde_json::Value = r.json().unwrap_or_default();
    if !status.is_success() {
        return Err(v["message"].as_str().map(str::to_string).unwrap_or_else(|| format!("cimon 이 거절했다({status})")));
    }
    let link = v["url"].as_str().ok_or("cimon 이 링크를 주지 않았다")?.to_string();
    // only a page of the server the token went to
    if !link.starts_with("http://") && !link.starts_with("https://") {
        return Err("cimon 이 http(s) 가 아닌 링크를 줬다".into());
    }
    Ok(link)
}

pub fn open(link: &str) {
    let (verb, target) = (wide("open"), wide(link));
    // SAFETY: NUL-terminated strings
    unsafe { ShellExecuteW(std::ptr::null_mut(), verb.as_ptr(), target.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL) };
}
