//! Per-user settings under `HKCU\Software\cimon\tray` (docs/tray.md §6).

use super::wide;
use windows_sys::Win32::System::Registry::{RegDeleteKeyValueW, RegGetValueW, RegSetKeyValueW, HKEY_CURRENT_USER, REG_DWORD, REG_SZ, RRF_RT_REG_DWORD, RRF_RT_REG_SZ};

pub const KEY: &str = r"Software\cimon\tray";
pub const DEFAULT_URL: &str = "http://127.0.0.1:8770";

const RUN: &str = r"Software\Microsoft\Windows\CurrentVersion\Run";
const RUN_VALUE: &str = "cimon-tray";

/// A string value, or `None` when it is not set.
pub fn get_str(name: &str) -> Option<String> {
    get_str_at(KEY, name)
}

fn get_str_at(key: &str, name: &str) -> Option<String> {
    let key = wide(key);
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

pub fn get_bool(name: &str, default: bool) -> bool {
    let (key, val) = (wide(KEY), wide(name));
    let mut v = 0u32;
    let mut bytes = 4u32;
    // SAFETY: a DWORD read into `v`
    let rc = unsafe { RegGetValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), RRF_RT_REG_DWORD, std::ptr::null_mut(), (&mut v as *mut u32).cast(), &mut bytes) };
    if rc == 0 {
        v != 0
    } else {
        default
    }
}

pub fn set_bool(name: &str, on: bool) {
    let (key, val) = (wide(KEY), wide(name));
    let v: u32 = on as u32;
    // SAFETY: a DWORD from `v`; the key is created when missing
    unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), REG_DWORD, (&v as *const u32).cast(), 4) };
}

/// 「로그인할 때 시작」: the Run value points at this executable.
pub fn autostart() -> bool {
    let Ok(exe) = std::env::current_exe() else { return false };
    get_str_at(RUN, RUN_VALUE).is_some_and(|v| v.trim_matches('"').eq_ignore_ascii_case(&exe.to_string_lossy()))
}

pub fn set_autostart(on: bool) {
    let (key, val) = (wide(RUN), wide(RUN_VALUE));
    if on {
        let Ok(exe) = std::env::current_exe() else { return };
        let data = wide(&format!("\"{}\"", exe.display()));
        // SAFETY: a NUL-terminated string value
        unsafe { RegSetKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr(), REG_SZ, data.as_ptr().cast(), (data.len() * 2) as u32) };
    } else {
        unsafe { RegDeleteKeyValueW(HKEY_CURRENT_USER, key.as_ptr(), val.as_ptr()) };
    }
}

/// The cimon server this tray reads `/healthz` from.
pub fn url() -> String {
    get_str("Url").filter(|u| !u.trim().is_empty()).unwrap_or_else(|| DEFAULT_URL.to_string()).trim_end_matches('/').to_string()
}
