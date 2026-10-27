//! `cimon ctl` — the service through the service control manager: what the tray runs
//! elevated when its user may not start or stop cimon (docs/tray.md §4.3), and a
//! shorter way to type it than `sc.exe`.

use anyhow::{bail, Result};
use cimon_ctl::{CtlError, State};
use clap::Subcommand;
use std::time::{Duration, Instant};

#[derive(Subcommand, Clone, Copy)]
pub enum CtlCmd {
    /// Print the service's state.
    Status,
    /// Start the service and wait until it runs.
    Start,
    /// Stop now — running jobs are interrupted.
    Stop,
    /// Refuse new jobs and stop once nothing runs (control 128).
    DrainStop,
    /// Take jobs again (control 129).
    CancelDrain,
    /// Stop now, then start.
    Restart,
}

fn explain(e: CtlError) -> anyhow::Error {
    match e {
        CtlError::AccessDenied => anyhow::anyhow!("access denied — run this from an administrator PowerShell, or have an admin run `cimon setup --operator <user>` (docs/tray.md §4.1)"),
        e => anyhow::anyhow!(e),
    }
}

fn wait_for(want: State, secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let s = cimon_ctl::query().map_err(explain)?;
        if s == want {
            println!("{}: {}", cimon_ctl::SERVICE, s.as_str());
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("still {} after {secs}s (waited for {})", s.as_str(), want.as_str());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

fn start() -> Result<()> {
    match cimon_ctl::query().map_err(explain)? {
        State::Running => {
            println!("{}: already running", cimon_ctl::SERVICE);
            return Ok(());
        }
        State::NotInstalled => bail!("the {} service is not installed — cimon setup", cimon_ctl::SERVICE),
        _ => {}
    }
    cimon_ctl::start().map_err(explain)?;
    wait_for(State::Running, 60)
}

fn stop() -> Result<()> {
    if cimon_ctl::query().map_err(explain)? == State::Stopped {
        println!("{}: already stopped", cimon_ctl::SERVICE);
        return Ok(());
    }
    cimon_ctl::stop().map_err(explain)?;
    // a running job's processes are terminated and recorded as interrupted first
    wait_for(State::Stopped, 120)
}

pub fn run(cmd: CtlCmd) -> Result<i32> {
    match cmd {
        CtlCmd::Status => {
            let s = cimon_ctl::query().map_err(explain)?;
            println!("{}: {}", cimon_ctl::SERVICE, s.as_str());
            return Ok(if s == State::Running { 0 } else { 3 });
        }
        CtlCmd::Start => start()?,
        CtlCmd::Stop => stop()?,
        CtlCmd::Restart => {
            stop()?;
            start()?;
        }
        CtlCmd::DrainStop => {
            cimon_ctl::notify(cimon_ctl::STOP_WHEN_IDLE).map_err(explain)?;
            println!("{}: refusing new jobs — it stops once nothing runs", cimon_ctl::SERVICE);
        }
        CtlCmd::CancelDrain => {
            cimon_ctl::notify(cimon_ctl::CANCEL_STOP_WHEN_IDLE).map_err(explain)?;
            println!("{}: taking jobs again", cimon_ctl::SERVICE);
        }
    }
    Ok(0)
}
