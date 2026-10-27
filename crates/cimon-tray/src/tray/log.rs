//! `%LOCALAPPDATA%\cimon\tray.log` — the only place a GUI app without a console can say
//! what went wrong. Kept small: past 256 KB it moves to `tray.log.1`.

use std::io::Write;
use std::path::PathBuf;

fn path() -> Option<PathBuf> {
    std::env::var_os("LOCALAPPDATA").map(|d| PathBuf::from(d).join("cimon").join("tray.log"))
}

pub fn line(msg: &str) {
    let Some(p) = path() else { return };
    if let Some(dir) = p.parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    if std::fs::metadata(&p).is_ok_and(|m| m.len() > 256 * 1024) {
        let _ = std::fs::rename(&p, p.with_extension("log.1"));
    }
    let secs = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(&p) {
        let _ = writeln!(f, "{secs} {msg}");
    }
}
