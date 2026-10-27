//! Running something elevated: `ShellExecuteExW` with the `runas` verb, so the user
//! sees UAC and approves each one (docs/tray.md §5.1). The tray never holds the
//! rights itself and waits for nothing — the elevated window shows its own result.

use super::wide;
use std::path::PathBuf;
use windows_sys::Win32::Foundation::GetLastError;
use windows_sys::Win32::UI::Shell::{ShellExecuteExW, SHELLEXECUTEINFOW};
use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;

const ERROR_CANCELLED: u32 = 1223;

/// `cimon.exe` next to this tray — never one found on PATH.
pub fn cimon_exe() -> Result<PathBuf, String> {
    let exe = std::env::current_exe().map_err(|e| e.to_string())?;
    let p = exe.parent().ok_or("no folder")?.join("cimon.exe");
    if p.is_file() {
        Ok(p)
    } else {
        Err(format!("{} 가 없다 — 트레이는 cimon.exe 와 같은 폴더에서 돌아야 한다", p.display()))
    }
}

/// Runs `exe args…` elevated in its own window. `args` are fixed strings or values
/// the caller checked; each is quoted for the C runtime.
pub fn run(exe: &std::path::Path, args: &[&str]) -> Result<(), String> {
    let params: Vec<String> = args.iter().map(|a| cimon_ctl::quote_arg(a)).collect();
    let (verb, file, params) = (wide("runas"), wide(&exe.to_string_lossy()), wide(&params.join(" ")));
    // SAFETY: a zeroed SHELLEXECUTEINFOW with the size and strings set is valid
    let ok = unsafe {
        let mut info: SHELLEXECUTEINFOW = std::mem::zeroed();
        info.cbSize = std::mem::size_of::<SHELLEXECUTEINFOW>() as u32;
        info.lpVerb = verb.as_ptr();
        info.lpFile = file.as_ptr();
        info.lpParameters = params.as_ptr();
        info.nShow = SW_SHOWNORMAL;
        ShellExecuteExW(&mut info)
    };
    if ok != 0 {
        return Ok(());
    }
    match unsafe { GetLastError() } {
        ERROR_CANCELLED => Err("UAC 확인을 취소했다".into()),
        e => Err(format!("관리자 권한으로 실행하지 못했다(오류 {e})")),
    }
}

/// `cimon.exe <args> --pause` elevated.
pub fn cimon(args: &[&str]) -> Result<(), String> {
    let exe = cimon_exe()?;
    let mut all = args.to_vec();
    all.push("--pause");
    run(&exe, &all)
}
