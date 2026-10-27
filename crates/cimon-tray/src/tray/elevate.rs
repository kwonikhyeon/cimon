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
    // ShellExecuteEx may use COM for the consent UI; this is a worker thread of our own
    // SAFETY: initializes COM for this thread only
    unsafe { windows_sys::Win32::System::Com::CoInitializeEx(std::ptr::null(), windows_sys::Win32::System::Com::COINIT_APARTMENTTHREADED as u32) };
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

/// An administrator PowerShell with `$c` set to this `cimon.exe` (docs/tray.md §5.1) —
/// the one shell started with a command string, and nothing the user typed is in it.
pub fn powershell() -> Result<(), String> {
    let exe = cimon_exe()?;
    let path = exe.to_string_lossy().to_string();
    if path.contains('\'') {
        return Err(format!("{path} 에 작은따옴표가 있어 명령에 넣지 않는다"));
    }
    let dir = exe.parent().map(|d| d.to_string_lossy().to_string()).unwrap_or_default();
    let script = format!("$c = '{path}'; Set-Location '{dir}'; Write-Host ('$c = ' + $c + '   e.g. & $c ctl status')");
    let ps = std::path::PathBuf::from(std::env::var("SystemRoot").unwrap_or_else(|_| r"C:\Windows".into())).join(r"System32\WindowsPowerShell\v1.0\powershell.exe");
    run(&ps, &["-NoExit", "-NoProfile", "-Command", &script])
}

/// `cimon.exe <args> --pause` elevated.
pub fn cimon(args: &[&str]) -> Result<(), String> {
    let exe = cimon_exe()?;
    let mut all = args.to_vec();
    all.push("--pause");
    run(&exe, &all)
}
