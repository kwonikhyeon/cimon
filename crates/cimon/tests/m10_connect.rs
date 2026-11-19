//! Team-member onboarding (design §16.3): invite codes, the connect scripts the server
//! serves, and redeeming a code for a new user's token.

mod common;

use cimon_core::Role;
use common::*;
use serde_json::{json, Value};

/// Attempts are limited per source: a noisy caller cannot lock everyone else out.
#[tokio::test]
async fn redeem_attempts_are_limited_per_source() {
    let h = Harness::start("").await;
    let http = reqwest::Client::new();
    let try_as = |login: &'static str| http.post(format!("{}/connect/redeem", h.base)).header("Tailscale-User-Login", login).json(&json!({"code": "ZZZZ-ZZZZ"})).send();
    let mut codes = vec![];
    for _ in 0..21 {
        codes.push(try_as("noisy@example.invalid").await.unwrap().status().as_u16());
    }
    assert!(codes[..20].iter().all(|c| *c == 403), "{codes:?}");
    assert_eq!(codes[20], 429, "the 21st attempt from that source");
    assert_eq!(try_as("someone-else@example.invalid").await.unwrap().status(), 403, "another source still gets an answer");
    let (code, _) = h.app.store.create_invite("dev-other", Role::Member, "admin-a", 10).unwrap();
    let r = http.post(format!("{}/connect/redeem", h.base)).header("Tailscale-User-Login", "other@example.invalid").json(&json!({"code": code})).send().await.unwrap();
    assert_eq!(r.status(), 200);
}

#[tokio::test]
async fn invite_is_redeemed_once_and_the_scripts_carry_the_address() {
    let h = Harness::start("").await;
    let http = reqwest::Client::new();
    let redeem = |body: Value| http.post(format!("{}/connect/redeem", h.base)).json(&body).send();

    // the scripts are served with this server's address in them
    for path in ["connect.ps1", "connect.sh"] {
        let r = http.get(format!("{}/{path}", h.base)).send().await.unwrap();
        assert_eq!(r.status(), 200, "{path}");
        let text = r.text().await.unwrap();
        assert!(text.contains(&format!("'{}'", h.base)), "{path} lacks the address");
        assert!(!text.contains("{{URL}}"), "{path}");
    }
    // the Windows header helper the connect script fetches; the address is in its hint
    let r = http.get(format!("{}/header.ps1", h.base)).send().await.unwrap();
    assert_eq!(r.status(), 200);
    let text = r.text().await.unwrap();
    assert!(text.contains(&format!("{}/connect.ps1", h.base)) && !text.contains("{{URL}}"), "{text}");

    // one user per device: a name that already exists is not invited
    assert!(h.app.store.create_invite("member-b", Role::Member, "admin-a", 10).is_err());
    let (code, _) = h.app.store.create_invite("dev-laptop", Role::Member, "admin-a", 10).unwrap();
    assert_eq!(code.len(), 9, "XXXX-XXXX: {code}");

    let r = redeem(json!({"code": "ZZZZ-ZZZZ"})).await.unwrap();
    assert_eq!(r.status(), 403, "a wrong code");

    // typed loosely: lower case, a space for the dash, O for zero
    let typed = code.to_lowercase().replace('-', " ").replace('0', "o");
    let r = redeem(json!({"code": typed, "device": "LAPTOP-1"})).await.unwrap();
    assert_eq!(r.status(), 200);
    let v: Value = r.json().await.unwrap();
    assert_eq!(v["user"], "dev-laptop");
    assert_eq!(v["role"], "member");
    assert_eq!(v["url"], h.base.as_str());
    let token = v["token"].as_str().unwrap().to_string();

    // the token signs in
    let hz: Value = http.get(format!("{}/healthz", h.base)).bearer_auth(&token).send().await.unwrap().json().await.unwrap();
    assert!(hz.get("queued").is_some(), "{hz}");

    // once only
    let r = redeem(json!({"code": code})).await.unwrap();
    assert_eq!(r.status(), 403, "a used code");

    // an expired code
    let (old, _) = h.app.store.create_invite("old-pc", Role::Member, "admin-a", 0).unwrap();
    tokio::time::sleep(std::time::Duration::from_millis(1100)).await;
    let r = redeem(json!({"code": old})).await.unwrap();
    assert_eq!(r.status(), 403, "an expired code");
    assert!(h.app.store.user("old-pc").unwrap().is_none(), "no user for an expired invite");
}

/// The Windows header helper reads the token with .NET alone, so it works where Windows
/// PowerShell 5.1 cannot load its own modules — started from PowerShell 7, it inherits
/// 7's module path — and when it cannot read the token it fails (exit 1, nothing on
/// stdout) rather than send an empty one that comes back as a 401.
#[cfg(windows)]
#[tokio::test]
async fn header_helper_needs_no_modules_and_fails_instead_of_sending_no_token() {
    use std::path::Path;
    use std::process::{Command, Output};

    let h = Harness::start("").await;
    let text = reqwest::get(format!("{}/header.ps1", h.base)).await.unwrap().text().await.unwrap();
    let d = tempfile::tempdir().unwrap();
    let helper = d.path().join("header.ps1");
    std::fs::write(&helper, &text).unwrap();
    let profile = d.path().join("profile");
    std::fs::create_dir_all(profile.join(".cimon")).unwrap();
    let token_file = profile.join(".cimon").join("token.dpapi");

    // a module path on which 5.1 finds a Microsoft.PowerShell.Security it cannot load,
    // as on PowerShell 7's
    let poison = d.path().join("modules");
    let m = poison.join("Microsoft.PowerShell.Security");
    std::fs::create_dir_all(&m).unwrap();
    std::fs::write(
        m.join("Microsoft.PowerShell.Security.psd1"),
        "@{ ModuleVersion = '7.0.0.0'; PowerShellVersion = '7.0'; CompatiblePSEditions = @('Core'); RootModule = 'missing.dll'; CmdletsToExport = @('ConvertTo-SecureString', 'ConvertFrom-SecureString') }",
    )
    .unwrap();
    let poisoned = format!("{};{}", poison.display(), std::env::var("PSModulePath").unwrap_or_default());

    let powershell = Path::new(&std::env::var("SystemRoot").unwrap()).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    let run = |args: &[&str], module_path: Option<&str>, env: &[(&str, &str)]| -> Output {
        let mut c = Command::new(&powershell);
        c.args(["-NoProfile", "-ExecutionPolicy", "Bypass"]).args(args).env("USERPROFILE", &profile);
        if let Some(p) = module_path {
            c.env("PSModulePath", p);
        }
        c.envs(env.iter().copied()).output().unwrap()
    };
    let helper_arg = helper.to_str().unwrap();
    let stdout = |o: &Output| String::from_utf8_lossy(&o.stdout).trim().to_string();
    let stderr = |o: &Output| String::from_utf8_lossy(&o.stderr).to_string();

    // the token stored the way the connect script stores it
    let token = "0123456789abcdef".repeat(4);
    let o = run(
        &["-Command", "ConvertTo-SecureString $env:T -AsPlainText -Force | ConvertFrom-SecureString | Set-Content $env:P"],
        None,
        &[("T", &token), ("P", token_file.to_str().unwrap())],
    );
    assert!(o.status.success(), "{}", stderr(&o));
    let want = format!(r#"{{"Authorization":"Bearer {token}"}}"#);

    let o = run(&["-File", helper_arg], None, &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), want);

    // the poisoned path does break the cmdlet the old helper used — and not this one
    let o = run(&["-Command", "$ErrorActionPreference = 'Stop'; ConvertTo-SecureString 'x' -AsPlainText -Force | Out-Null"], Some(&poisoned), &[]);
    // (the error id, not the message: that is in the system's language)
    assert!(!o.status.success() && stderr(&o).contains("CouldNotAutoloadMatchingModule"), "{}", stderr(&o));
    let o = run(&["-File", helper_arg], Some(&poisoned), &[]);
    assert!(o.status.success(), "{}", stderr(&o));
    assert_eq!(stdout(&o), want);

    // a blob this Windows user cannot decrypt
    std::fs::write(&token_file, "0123456789abcdef").unwrap();
    let o = run(&["-File", helper_arg], None, &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "");
    assert!(stderr(&o).contains("cimon header helper: cannot decrypt"), "{}", stderr(&o));

    // no token file
    std::fs::remove_file(&token_file).unwrap();
    let o = run(&["-File", helper_arg], None, &[]);
    assert_eq!(o.status.code(), Some(1));
    assert_eq!(stdout(&o), "");
    assert!(stderr(&o).contains("cimon header helper: no ") && stderr(&o).contains(&format!("{}/connect.ps1", h.base)), "{}", stderr(&o));
}
