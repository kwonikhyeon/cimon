//! What the tray shows, from what it observed (docs/tray.md §3.1) — no Win32 here, so
//! it is tested anywhere.

use cimon_ctl::State;
use serde_json::Value;

/// `/healthz` as a signed-in caller sees it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Health {
    pub queued: u64,
    pub running: u64,
    pub draining: bool,
    pub stopping_when_idle: bool,
    pub version: String,
    pub warnings: Vec<String>,
}

impl Health {
    /// `None` when the answer carries no counts — no token, or the token was refused.
    pub fn from_json(v: &Value) -> Option<Health> {
        Some(Health {
            queued: v.get("queued")?.as_u64()?,
            running: v.get("running")?.as_u64()?,
            draining: v.get("draining").and_then(Value::as_bool).unwrap_or(false),
            stopping_when_idle: v.get("stopping_when_idle").and_then(Value::as_bool).unwrap_or(false),
            version: v.get("version").and_then(Value::as_str).unwrap_or_default().to_string(),
            warnings: v.get("warnings").and_then(Value::as_array).map(|a| a.iter().filter_map(|w| w.as_str().map(str::to_string)).collect()).unwrap_or_default(),
        })
    }
}

/// The last `/healthz` probe (only while the service runs).
#[derive(Debug, Clone, PartialEq)]
pub enum Probe {
    Answered(Health),
    /// it answered, without counts: no token on this PC, or the token was refused
    NoCounts,
    NoAnswer(String),
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Color {
    Green,
    Blue,
    Yellow,
    Gray,
    Red,
}

#[derive(Debug, Clone, PartialEq)]
pub struct View {
    pub color: Color,
    /// the tooltip (Windows keeps 127 characters)
    pub tip: String,
}

pub fn view(svc: State, probe: Option<&Probe>) -> View {
    let (color, text) = match (svc, probe) {
        (State::NotInstalled, _) => (Color::Gray, "서비스 없음(cimon setup)".to_string()),
        (State::Stopped, _) => (Color::Gray, "중지됨".into()),
        (State::StartPending, _) => (Color::Yellow, "시작 중…".into()),
        (State::StopPending, _) => (Color::Yellow, "중지 중…".into()),
        (State::Other, _) => (Color::Yellow, "일시 정지됨".into()),
        (State::Running, None) => (Color::Green, "실행 중".into()),
        (State::Running, Some(Probe::NoAnswer(_))) => (Color::Red, "응답 없음(서비스는 실행 중)".into()),
        (State::Running, Some(Probe::NoCounts)) => (Color::Green, "실행 중 — 작업 수는 이 PC 를 연결하면 보인다".into()),
        (State::Running, Some(Probe::Answered(h))) => {
            let warn = if h.warnings.is_empty() { String::new() } else { format!(" · 경고 {}", h.warnings.len()) };
            if h.stopping_when_idle {
                let t = if h.running == 0 { "곧 중지".to_string() } else { format!("작업 {} 끝나면 중지", h.running) };
                (Color::Yellow, format!("{t}{warn}"))
            } else if h.draining {
                (Color::Yellow, format!("새 작업 거절 중(드레인) · 작업 {}{warn}", h.running))
            } else if h.running > 0 || h.queued > 0 {
                (Color::Blue, format!("작업 {} · 대기 {}{warn}", h.running, h.queued))
            } else {
                (Color::Green, format!("실행 중{warn}"))
            }
        }
    };
    let mut tip = format!("cimon — {text}");
    if tip.chars().count() > 127 {
        tip = tip.chars().take(126).collect::<String>() + "…";
    }
    View { color, tip }
}

/// The token goes only to this machine or over https — a mistyped address must not
/// carry it elsewhere in the clear.
pub fn token_allowed(url: &str) -> bool {
    let u = url.to_ascii_lowercase();
    if let Some(rest) = u.strip_prefix("https://") {
        return !rest.split('/').next().unwrap_or_default().contains('@');
    }
    let Some(rest) = u.strip_prefix("http://") else { return false };
    let host = rest.split('/').next().unwrap_or_default();
    let host = host.rsplit_once(':').map(|(h, p)| if p.chars().all(|c| c.is_ascii_digit()) { h } else { host }).unwrap_or(host);
    matches!(host, "127.0.0.1" | "localhost" | "[::1]")
}

/// `ConvertFrom-SecureString` writes the DPAPI blob as hex.
pub fn decode_hex(s: &str) -> Option<Vec<u8>> {
    let s = s.trim_start_matches('\u{feff}').trim();
    if !s.len().is_multiple_of(2) || s.is_empty() {
        return None;
    }
    (0..s.len()).step_by(2).map(|i| u8::from_str_radix(s.get(i..i + 2)?, 16).ok()).collect()
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn h(queued: u64, running: u64) -> Probe {
        Probe::Answered(Health { queued, running, ..Default::default() })
    }

    #[test]
    fn states_become_colors_and_tips() {
        assert_eq!(view(State::Stopped, None), View { color: Color::Gray, tip: "cimon — 중지됨".into() });
        assert_eq!(view(State::NotInstalled, None).color, Color::Gray);
        assert_eq!(view(State::StopPending, None).color, Color::Yellow);
        assert_eq!(view(State::Running, Some(&h(0, 0))).tip, "cimon — 실행 중");
        assert_eq!(view(State::Running, Some(&h(2, 1))), View { color: Color::Blue, tip: "cimon — 작업 1 · 대기 2".into() });
        assert_eq!(view(State::Running, Some(&Probe::NoAnswer("timeout".into()))).color, Color::Red);
        assert_eq!(view(State::Running, Some(&Probe::NoCounts)).color, Color::Green);
        let stopping = Probe::Answered(Health { running: 1, stopping_when_idle: true, draining: true, ..Default::default() });
        assert_eq!(view(State::Running, Some(&stopping)), View { color: Color::Yellow, tip: "cimon — 작업 1 끝나면 중지".into() });
        let warned = Probe::Answered(Health { warnings: vec!["a".into(), "b".into()], ..Default::default() });
        assert_eq!(view(State::Running, Some(&warned)).tip, "cimon — 실행 중 · 경고 2");
    }

    #[test]
    fn healthz_with_and_without_counts() {
        let signed = json!({"ok": true, "queued": 2, "running": 1, "draining": false, "version": "0.1.0", "warnings": ["x"]});
        assert_eq!(Health::from_json(&signed), Some(Health { queued: 2, running: 1, version: "0.1.0".into(), warnings: vec!["x".into()], ..Default::default() }));
        assert_eq!(Health::from_json(&json!({"ok": true, "detail": "send a user token"})), None);
    }

    #[test]
    fn the_token_stays_local_or_on_https() {
        for ok in ["http://127.0.0.1:8770", "http://localhost:8770/", "https://cimon.example.ts.net", "HTTP://127.0.0.1"] {
            assert!(token_allowed(ok), "{ok}");
        }
        for bad in ["http://cimon.example.ts.net", "http://127.0.0.1.evil.example", "ftp://127.0.0.1", "https://user@evil.example", "127.0.0.1:8770"] {
            assert!(!token_allowed(bad), "{bad}");
        }
    }

    #[test]
    fn hex_from_powershell() {
        assert_eq!(decode_hex("\u{feff}01ff\r\n"), Some(vec![1, 255]));
        assert_eq!(decode_hex("0"), None);
        assert_eq!(decode_hex("zz"), None);
    }
}
