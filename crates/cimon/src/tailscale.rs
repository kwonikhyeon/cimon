//! Tailscale checks (design §15.3): the service refuses to start when Funnel would
//! expose it to the internet, and — unless `[tailscale].allow_unverified_funnel` —
//! when it cannot tell.

use anyhow::{bail, Result};
use serde_json::Value;

/// What `tailscale serve status --json` showed.
#[derive(Debug, PartialEq, Eq)]
pub enum Funnel {
    Off,
    /// The status could not be read — the reason. On Windows the service account gets
    /// "401 Unauthorized: Tailscale already in use by <user>": Tailscale answers only the
    /// user who owns it.
    Unobservable(String),
}

/// Reads `tailscale serve status --json`. An error means Funnel is on.
pub fn check_funnel() -> Result<Funnel> {
    match std::process::Command::new("tailscale").args(["serve", "status", "--json"]).output() {
        Ok(o) => classify(o.status.success(), &String::from_utf8_lossy(&o.stdout), &String::from_utf8_lossy(&o.stderr)),
        Err(e) => Ok(Funnel::Unobservable(format!("cannot run `tailscale` ({e})"))),
    }
}

fn classify(success: bool, stdout: &str, stderr: &str) -> Result<Funnel> {
    if !success {
        return Ok(Funnel::Unobservable(format!("`tailscale serve status --json` failed: {}", stderr.trim())));
    }
    match funnel_enabled(stdout) {
        Ok(true) => bail!("Tailscale Funnel is on — cimon must not be reachable from the internet. Run `tailscale funnel reset` and start again"),
        Ok(false) => Ok(Funnel::Off),
        Err(e) => Ok(Funnel::Unobservable(format!("unreadable `tailscale serve status --json` output: {e}"))),
    }
}

/// For `cimon doctor` (run by the user who owns Tailscale): Funnel must be seen off.
pub fn ensure_funnel_off() -> Result<()> {
    match check_funnel()? {
        Funnel::Off => Ok(()),
        Funnel::Unobservable(why) => bail!("{why} — run this as the user Tailscale is logged in with; on a development machine without Tailscale set [tailscale].require_funnel_off = false"),
    }
}

/// True when the status JSON enables Funnel for any host.
pub fn funnel_enabled(json: &str) -> Result<bool> {
    let json = json.trim();
    if json.is_empty() || json == "{}" {
        return Ok(false);
    }
    let v: Value = serde_json::from_str(json)?;
    Ok(match v.get("AllowFunnel") {
        Some(Value::Object(m)) => m.values().any(|b| b.as_bool().unwrap_or(false)),
        _ => false,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unreadable_status_is_not_taken_for_off() {
        let denied = classify(false, "", "getting serve config: 401 Unauthorized: Tailscale already in use by HOST\\user, pid 1").unwrap();
        assert!(matches!(denied, Funnel::Unobservable(ref w) if w.contains("already in use")), "{denied:?}");
        assert!(matches!(classify(true, "not json", "").unwrap(), Funnel::Unobservable(_)));
        assert_eq!(classify(true, "{}", "").unwrap(), Funnel::Off);
        assert!(classify(true, r#"{"AllowFunnel":{"cimon.x.ts.net:443":true}}"#, "").is_err());
    }

    #[test]
    fn detects_funnel() {
        assert!(!funnel_enabled("{}").unwrap());
        assert!(!funnel_enabled(r#"{"TCP":{"443":{"HTTPS":true}}}"#).unwrap());
        assert!(funnel_enabled(r#"{"TCP":{"443":{"HTTPS":true}},"AllowFunnel":{"cimon.x.ts.net:443":true}}"#).unwrap());
        assert!(!funnel_enabled(r#"{"AllowFunnel":{"cimon.x.ts.net:443":false}}"#).unwrap());
    }
}
