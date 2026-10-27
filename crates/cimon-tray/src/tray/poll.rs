//! The status thread: the service's state every 3 seconds, `/healthz` every 10 while it
//! runs (sooner after an action), handed to the UI thread with a message.

use crate::model::{Health, Probe};
use cimon_ctl::State;
use std::sync::mpsc::{Receiver, RecvTimeoutError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

#[derive(Debug, Clone, PartialEq)]
pub struct Snapshot {
    pub svc: State,
    pub probe: Option<Probe>,
}

impl Default for Snapshot {
    fn default() -> Self {
        Snapshot { svc: State::Other, probe: None }
    }
}

/// One `/healthz` probe with this user's token (when the address may carry it).
pub fn probe(client: &reqwest::blocking::Client) -> Probe {
    let url = super::settings::url();
    let mut req = client.get(format!("{url}/healthz"));
    if crate::model::token_allowed(&url) {
        if let Some(t) = super::token::read() {
            req = req.bearer_auth(t);
        }
    }
    match req.send().and_then(|r| r.error_for_status()).and_then(|r| r.json::<serde_json::Value>()) {
        Ok(v) => Health::from_json(&v).map(Probe::Answered).unwrap_or(Probe::NoCounts),
        Err(e) => Probe::NoAnswer(e.to_string()),
    }
}

pub fn client() -> reqwest::blocking::Client {
    reqwest::blocking::Client::builder().timeout(Duration::from_secs(3)).build().unwrap_or_default()
}

pub fn spawn(hwnd: usize, shared: Arc<Mutex<Snapshot>>, poke: Receiver<()>) {
    std::thread::spawn(move || {
        let client = client();
        let mut last_probe: Option<Instant> = None;
        let mut prev_svc = State::Other;
        loop {
            let svc = cimon_ctl::query().unwrap_or(State::Other);
            let probe = if svc == State::Running {
                let due = svc != prev_svc || last_probe.is_none_or(|t| t.elapsed() >= Duration::from_secs(10));
                if due {
                    last_probe = Some(Instant::now());
                    Some(probe(&client))
                } else {
                    shared.lock().unwrap_or_else(|p| p.into_inner()).probe.clone()
                }
            } else {
                last_probe = None;
                None
            };
            prev_svc = svc;
            let snap = Snapshot { svc, probe };
            let changed = {
                let mut s = shared.lock().unwrap_or_else(|p| p.into_inner());
                let c = *s != snap;
                *s = snap;
                c
            };
            if changed {
                // SAFETY: posting to the tray's window; it outlives this thread's use of it
                unsafe { PostMessageW(hwnd as _, super::WM_STATUS, 0, 0) };
            }
            match poke.recv_timeout(Duration::from_secs(3)) {
                // an action: probe at the next turn, not 10 seconds later
                Ok(()) => last_probe = None,
                Err(RecvTimeoutError::Timeout) => {}
                Err(RecvTimeoutError::Disconnected) => return,
            }
        }
    });
}
