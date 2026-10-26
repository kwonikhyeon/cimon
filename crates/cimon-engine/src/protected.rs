//! JSON kept DPAPI-encrypted in `config\` — the credentials build steps get
//! (`build_git`, `build_secrets`).

use serde::{de::DeserializeOwned, Serialize};
use std::path::Path;

/// No file: the default (empty) value.
pub fn load<T: DeserializeOwned + Default>(p: &Path) -> Result<T, String> {
    let blob = match std::fs::read(p) {
        Ok(b) => b,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(T::default()),
        Err(e) => return Err(format!("{}: {e}", p.display())),
    };
    #[cfg(windows)]
    let raw = cimon_runner::win::unprotect(&blob).map_err(|e| format!("{}: {e}", p.display()))?;
    #[cfg(not(windows))]
    let raw = blob;
    serde_json::from_slice(&raw).map_err(|e| format!("{}: {e}", p.display()))
}

/// `empty`: remove the file instead of keeping an encrypted nothing.
pub fn save<T: Serialize>(p: &Path, value: &T, empty: bool) -> Result<(), String> {
    if empty {
        return match std::fs::remove_file(p) {
            Err(e) if e.kind() != std::io::ErrorKind::NotFound => Err(format!("{}: {e}", p.display())),
            _ => Ok(()),
        };
    }
    let raw = serde_json::to_vec(value).map_err(|e| e.to_string())?;
    #[cfg(windows)]
    let blob = cimon_runner::win::protect(&raw).map_err(|e| e.to_string())?;
    #[cfg(not(windows))]
    let blob = raw;
    std::fs::write(p, blob).map_err(|e| format!("{}: {e}", p.display()))
}
