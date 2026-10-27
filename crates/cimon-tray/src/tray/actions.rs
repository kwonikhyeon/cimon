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

/// Ok: what to tell the user (or nothing — the icon shows it); Err: why it failed.
pub fn perform(a: Action) -> Result<Option<String>, String> {
    super::log::line(&format!("action {}", a.ctl()));
    match direct(a) {
        Ok(()) => Ok(None),
        Err(CtlError::AccessDenied) => {
            super::elevate::cimon(&["ctl", a.ctl()])?;
            Ok(Some("관리자 창에서 실행했다 — 결과는 그 창에 나온다".into()))
        }
        Err(e) => Err(e.to_string()),
    }
}
