//! Starting and stopping the service: directly when this user may (cimon-operators,
//! docs/tray.md §4.1), otherwise `cimon ctl …` elevated through UAC (§4.3). Runs on a
//! worker thread; the answer is a line for a balloon.

use cimon_ctl::{CtlError, State};
use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Action {
    Start,
    StopNow,
    Restart,
    DrainStop,
    CancelDrain,
}

impl Action {
    fn ctl(self) -> &'static str {
        match self {
            Action::Start => "start",
            Action::StopNow => "stop",
            Action::Restart => "restart",
            Action::DrainStop => "drain-stop",
            Action::CancelDrain => "cancel-drain",
        }
    }
}

fn wait_for(want: State, secs: u64) -> Result<(), CtlError> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    while Instant::now() < deadline {
        if cimon_ctl::query()? == want {
            return Ok(());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    Err(CtlError::Other(format!("{}초 안에 {} 상태가 되지 않았다", secs, want.as_str())))
}

fn direct(a: Action) -> Result<(), CtlError> {
    match a {
        Action::Start => cimon_ctl::start(),
        Action::StopNow => cimon_ctl::stop(),
        Action::Restart => {
            cimon_ctl::stop()?;
            wait_for(State::Stopped, 120)?;
            cimon_ctl::start()
        }
        Action::DrainStop => cimon_ctl::notify(cimon_ctl::STOP_WHEN_IDLE),
        Action::CancelDrain => cimon_ctl::notify(cimon_ctl::CANCEL_STOP_WHEN_IDLE),
    }
}

/// A stop-when-idle control the service ignores still succeeds at the SCM: wait for
/// `/healthz` to show it (docs/tray.md §4.2).
fn taken(want: bool) -> bool {
    let client = super::poll::client();
    let url = format!("{}/healthz", super::settings::url());
    for _ in 0..10 {
        if let Ok(v) = client.get(&url).send().and_then(|r| r.json::<serde_json::Value>()) {
            if v["stopping_when_idle"].as_bool() == Some(want) {
                return true;
            }
        }
        std::thread::sleep(Duration::from_millis(500));
    }
    false
}

/// Ok: what to tell the user (or nothing — the icon shows it); Err: why it failed.
pub fn perform(a: Action) -> Result<Option<String>, String> {
    super::log::line(&format!("action {}", a.ctl()));
    match direct(a) {
        Ok(()) if matches!(a, Action::DrainStop | Action::CancelDrain) && !taken(a == Action::DrainStop) => {
            Err("cimon 이 이 제어를 받지 않았다 — 옛 버전이거나, 서비스 DACL 이 로그인한 사용자에게 제어를 허용한다(관리자가 cimon setup --service-acl)".into())
        }
        Ok(()) => Ok(None),
        Err(CtlError::AccessDenied) => {
            super::elevate::cimon(&["ctl", a.ctl()])?;
            Ok(Some("관리자 창에서 실행했다 — 결과는 그 창에 나온다".into()))
        }
        Err(e) => Err(e.to_string()),
    }
}
