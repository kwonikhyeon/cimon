//! Per-user settings under `HKCU\Software\cimon\tray` (docs/tray.md §6).

use super::wide;
use windows_sys::Win32::System::Registry::{RegGetValueW, HKEY_CURRENT_USER, RRF_RT_REG_SZ};

pub const KEY: &str = r"Software\cimon\tray";
pub const DEFAULT_URL: &str = "http://127.0.0.1:8770";

/// A string value, or `None` when it is not set.
pub fn get_str(name: &str) -> Option<String> {
    let key = wide(KEY);
    let val = wide(name);
    let mut buf = vec![0u16; 1024];
    let mut bytes = (buf.len() * 2) as u32;
    // SAFETY: NUL-terminated names; buf holds `bytes` bytes
    let rc = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_SZ, std::ptr::null_mut(), buf.as_mut_ptr().cast(), &mut bytes) };
    if rc != 0 {
        return None;
    }
    let s = String::from_utf16_lossy(&buf[..(bytes as usize / 2)]);
    Some(s.trim_end_matches('\0').to_string())
}

/// The cimon server this tray reads `/healthz` from.
pub fn url() -> String {
    get_str("Url").filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_URL.to_string()).trim_end_matches('/').to_string()
}
