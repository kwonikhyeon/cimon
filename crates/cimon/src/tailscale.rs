//! Tailscale checks (design §15.3): the service refuses to start when Funnel would
//! expose it to the internet, or when it cannot tell.

use anyhow::{bail, Result};
use serde_json::Value;

/// Reads `tailscale serve status --json` and fails if any Funnel entry is on.
pub fn ensure_funnel_off() -> Result<()> {
    let out = match std::process::Command::new("tailscale").args(["serve", "status", "--json"]).output() {
        Ok(o) => o,
        Err(e) => bail!(
            "cannot run `tailscale serve status --json` ({e}). The service only starts when it can confirm Funnel is off; \
             on a development machine without Tailscale set [tailscale].require_funnel_off = false"
        ),
    };
    if !out.status.success() {
        bail!("`tailscale serve status --json` failed: {}", String::from_utf8_lossy(&out.stderr).trim());
    }
    let text = String::from_utf8_lossy(&out.stdout);
    if funnel_enabled(&text)? {
        bail!("Tailscale Funnel is on — cimon must not be reachable from the internet. Run `tailscale funnel reset` and start again");
    }
    Ok(())
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
    fn detects_funnel() {
        assert!(!funnel_enabled("{}").unwrap());
        assert!(!funnel_enabled(r#"{"TCP":{"443":{"HTTPS":true}}}"#).unwrap());
        assert!(funnel_enabled(r#"{"TCP":{"443":{"HTTPS":true}},"AllowFunnel":{"cimon.x.ts.net:443":true}}"#).unwrap());
        assert!(!funnel_enabled(r#"{"AllowFunnel":{"cimon.x.ts.net:443":false}}"#).unwrap());
    }
}
