//! `cimon upgrade --from <exe>` (admin): design §16.6 in one command — check the new
//! executable, stop once running jobs end, replace, start, and see the new build answer
//! — and back to the previous executable when it does not (docs/tray.md §5.3).

use anyhow::{bail, Context, Result};
use cimon_core::Config;
use cimon_ctl::State;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

fn wait_state(want: State, secs: u64) -> Result<()> {
    let deadline = Instant::now() + Duration::from_secs(secs);
    loop {
        let s = cimon_ctl::query().map_err(|e| anyhow::anyhow!(e))?;
        if s == want {
            return Ok(());
        }
        if Instant::now() >= deadline {
            bail!("the service is still {} after {secs}s", s.as_str());
        }
        std::thread::sleep(Duration::from_millis(500));
    }
}

/// Stops the service: now, or once running jobs end (the stop-when-idle control, else
/// `drain` and a stop).
fn stop(cfg: &Config, now: bool, wait_secs: u64) -> Result<()> {
    if cimon_ctl::query().map_err(|e| anyhow::anyhow!(e))? != State::Running {
        return Ok(());
    }
    if !now {
        // an older cimon ignores the control and the SCM still says it went through:
        // only /healthz turning stopping_when_idle on shows it was taken
        let taken = match cimon_ctl::notify(cimon_ctl::STOP_WHEN_IDLE) {
            Ok(()) => crate::ctl::confirm_stopping(&format!("http://{}/healthz", cfg.server.bind), true),
            Err(e) => {
                println!("stop-when-idle failed: {e}");
                false
            }
        };
        if taken {
            println!("waiting for running jobs to end (up to {wait_secs}s) — new jobs are refused");
            return wait_state(State::Stopped, wait_secs);
        }
        println!("the running cimon did not take stop-when-idle (an older version, or its DACL) — draining instead");
        crate::ops::drain(cfg, false, wait_secs)?;
    }
    cimon_ctl::stop().map_err(|e| anyhow::anyhow!(e))?;
    wait_state(State::Stopped, 120)
}

/// `target` → `prev`, `new` → `target`. A running executable may be renamed, not
/// overwritten — this works while the tray (or this very cimon.exe) runs.
fn replace(target: &Path, new: &Path, prev: &Path) -> Result<()> {
    if prev.exists() {
        std::fs::remove_file(prev).with_context(|| format!("removing {}", prev.display()))?;
    }
    if target.exists() {
        std::fs::rename(target, prev).with_context(|| format!("moving {} aside", target.display()))?;
    }
    if let Err(e) = std::fs::copy(new, target) {
        let _ = std::fs::rename(prev, target);
        bail!("copying {} to {}: {e}", new.display(), target.display());
    }
    Ok(())
}

fn restore(target: &Path, prev: &Path) {
    if prev.exists() {
        let _ = std::fs::remove_file(target);
        let _ = std::fs::rename(prev, target);
    }
}

/// Starts the service and waits until `/healthz` answers with `build` = `want`.
fn start_and_check(healthz: &str, want: &str) -> Result<()> {
    cimon_ctl::start().map_err(|e| anyhow::anyhow!("starting: {e}"))?;
    wait_state(State::Running, 60)?;
    let client = reqwest::blocking::Client::builder().timeout(Duration::from_secs(3)).build()?;
    let deadline = Instant::now() + Duration::from_secs(60);
    let mut last = String::new();
    while Instant::now() < deadline {
        match client.get(healthz).send().and_then(|r| r.json::<serde_json::Value>()) {
            Ok(v) if v["build"].as_str() == Some(want) => return Ok(()),
            Ok(v) => last = format!("it answers build {}", v["build"].as_str().unwrap_or("?")),
            Err(e) => last = e.to_string(),
        }
        if cimon_ctl::query().ok() == Some(State::Stopped) {
            bail!("the service stopped again");
        }
        std::thread::sleep(Duration::from_secs(1));
    }
    bail!("{healthz} did not answer build {want} within 60s ({last})")
}

pub fn run(config_path: &Path, from: &Path, now: bool, wait_secs: u64) -> Result<()> {
    let from = std::path::absolute(from)?;
    if !from.is_file() {
        bail!("{} is not a file", from.display());
    }
    let target = crate::service_exe().context("the cimon service is not installed — cimon setup")?;
    let dir = target.parent().context("the service executable has no folder")?.to_path_buf();
    if std::fs::canonicalize(&from).ok() == std::fs::canonicalize(&target).ok() {
        bail!("{} is the installed executable — build a new one elsewhere", from.display());
    }
    let sha = cimon_engine::artifacts::sha256_file(&from).map_err(|e| anyhow::anyhow!(e))?;
    let meta = std::fs::metadata(&from)?;
    let version = std::process::Command::new(&from).arg("--version").output().map(|o| String::from_utf8_lossy(&o.stdout).trim().to_string()).unwrap_or_default();
    let tray_from = from.with_file_name("cimon-tray.exe");
    let tray = tray_from.is_file().then(|| (dir.join("cimon-tray.exe"), tray_from));
    println!("new:       {}", from.display());
    println!("           {version}, {} bytes, sha256 {sha}", meta.len());
    if let Ok(m) = meta.modified() {
        let age = m.elapsed().map(|d| d.as_secs() / 60).unwrap_or(0);
        println!("           built {age} minutes ago");
    }
    println!("replaces:  {}{}", target.display(), if tray.is_some() { " (and cimon-tray.exe next to it)" } else { "" });
    // an administrator is about to run a file from a folder anyone in this session can write
    if !crate::ops::ask_yes("upgrade to this file?")? {
        bail!("not upgraded");
    }

    // the new executable must accept the recipes on main (design §16.6 1)
    println!("checking the recipes on main with the new executable…");
    let st = std::process::Command::new(&from).args(["recipe", "check", "--main", "--config"]).arg(config_path).status().context("running the new executable")?;
    if !st.success() {
        bail!("the new cimon rejects the recipes on main — fix them first (design §16.6)");
    }
    let cfg = Config::load(config_path)?;
    stop(&cfg, now, wait_secs)?;

    let prev = PathBuf::from(format!("{}.prev", target.display()));
    replace(&target, &from, &prev)?;
    let tray_prev = tray.as_ref().map(|(t, _)| PathBuf::from(format!("{}.prev", t.display())));
    if let (Some((t, n)), Some(p)) = (&tray, &tray_prev) {
        if let Err(e) = replace(t, n, p) {
            println!("cimon-tray.exe not replaced: {e:#}");
        }
    }
    let healthz = format!("http://{}/healthz", cfg.server.bind);
    match start_and_check(&healthz, &sha[..12]) {
        Ok(()) => {
            println!("upgraded — {healthz} answers build {} (the previous executable is {})", &sha[..12], prev.display());
            Ok(())
        }
        Err(e) => {
            println!("the new cimon did not come up: {e:#} — going back to the previous executable");
            let _ = cimon_ctl::stop();
            let _ = wait_state(State::Stopped, 120);
            restore(&target, &prev);
            if let (Some((t, _)), Some(p)) = (&tray, &tray_prev) {
                restore(t, p);
            }
            match cimon_ctl::start() {
                Ok(()) => println!("the previous executable is back and starting"),
                Err(e2) => println!("the previous executable is back but did not start: {e2}"),
            }
            bail!("not upgraded: {e:#} — the service log is under {}", cimon_core::Layout::new(&cfg.paths.root).service_logs().display())
        }
    }
}
