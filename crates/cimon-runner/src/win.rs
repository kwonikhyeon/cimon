//! Win32 pieces: Job Objects, starting suspended processes, the build account's token
//! and profile, DPAPI for the stored build-account password.

#![cfg(windows)]
#![allow(clippy::missing_safety_doc)]

use std::ffi::OsStr;
use std::os::windows::ffi::OsStrExt;
use std::ptr::{null, null_mut};
use windows_sys::Win32::Foundation::{CloseHandle, GetLastError, LocalFree, HANDLE, INVALID_HANDLE_VALUE};
use windows_sys::Win32::Security::Cryptography::{CryptProtectData, CryptUnprotectData, CRYPTPROTECT_LOCAL_MACHINE, CRYPT_INTEGER_BLOB};
use windows_sys::Win32::Security::{LogonUserW, LOGON32_LOGON_BATCH, LOGON32_PROVIDER_DEFAULT};
use windows_sys::Win32::System::Diagnostics::ToolHelp::{CreateToolhelp32Snapshot, Thread32First, Thread32Next, TH32CS_SNAPTHREAD, THREADENTRY32};
use windows_sys::Win32::System::JobObjects::*;
use windows_sys::Win32::System::Threading::{OpenThread, ResumeThread, THREAD_SUSPEND_RESUME};
use windows_sys::Win32::UI::Shell::{GetUserProfileDirectoryW, LoadUserProfileW, UnloadUserProfile, PROFILEINFOW};

pub fn wide(s: &str) -> Vec<u16> {
    OsStr::new(s).encode_wide().chain(std::iter::once(0)).collect()
}

fn last_error(what: &str) -> std::io::Error {
    let code = unsafe { GetLastError() };
    std::io::Error::new(std::io::ErrorKind::Other, format!("{what} failed (Win32 error {code})"))
}

/// An owned Win32 handle.
#[derive(Debug)]
pub struct Handle(pub HANDLE);

unsafe impl Send for Handle {}
unsafe impl Sync for Handle {}

impl Drop for Handle {
    fn drop(&mut self) {
        if !self.0.is_null() && self.0 != INVALID_HANDLE_VALUE {
            unsafe { CloseHandle(self.0) };
        }
    }
}

/// A Job Object that kills everything in it when closed and forbids breakaway.
pub struct Job(Handle);

impl Job {
    pub fn new() -> std::io::Result<Job> {
        unsafe {
            let h = CreateJobObjectW(null(), null());
            if h.is_null() {
                return Err(last_error("CreateJobObjectW"));
            }
            let job = Job(Handle(h));
            let mut info: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
            info.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE;
            let ok = SetInformationJobObject(
                h,
                JobObjectExtendedLimitInformation,
                &info as *const _ as *const _,
                std::mem::size_of::<JOBOBJECT_EXTENDED_LIMIT_INFORMATION>() as u32,
            );
            if ok == 0 {
                return Err(last_error("SetInformationJobObject"));
            }
            Ok(job)
        }
    }

    pub fn assign(&self, process: HANDLE) -> std::io::Result<()> {
        if unsafe { AssignProcessToJobObject(self.0 .0, process) } == 0 {
            return Err(last_error("AssignProcessToJobObject"));
        }
        Ok(())
    }

    pub fn active_processes(&self) -> u32 {
        unsafe {
            let mut info: JOBOBJECT_BASIC_ACCOUNTING_INFORMATION = std::mem::zeroed();
            let ok = QueryInformationJobObject(
                self.0 .0,
                JobObjectBasicAccountingInformation,
                &mut info as *mut _ as *mut _,
                std::mem::size_of::<JOBOBJECT_BASIC_ACCOUNTING_INFORMATION>() as u32,
                null_mut(),
            );
            if ok == 0 {
                0
            } else {
                info.ActiveProcesses
            }
        }
    }

    pub fn terminate(&self) {
        unsafe { TerminateJobObject(self.0 .0, 1) };
    }
}

/// Resumes every thread of a process started with `CREATE_SUSPENDED` (std does not
/// expose the main thread handle).
pub fn resume_process(pid: u32) -> std::io::Result<()> {
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPTHREAD, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateToolhelp32Snapshot"));
        }
        let snap = Handle(snap);
        let mut te: THREADENTRY32 = std::mem::zeroed();
        te.dwSize = std::mem::size_of::<THREADENTRY32>() as u32;
        let mut resumed = 0;
        if Thread32First(snap.0, &mut te) != 0 {
            loop {
                if te.th32OwnerProcessID == pid {
                    let th = OpenThread(THREAD_SUSPEND_RESUME, 0, te.th32ThreadID);
                    if !th.is_null() {
                        ResumeThread(th);
                        CloseHandle(th);
                        resumed += 1;
                    }
                }
                if Thread32Next(snap.0, &mut te) == 0 {
                    break;
                }
            }
        }
        if resumed == 0 {
            return Err(std::io::Error::new(std::io::ErrorKind::Other, "no thread to resume"));
        }
        Ok(())
    }
}

/// A logged-on build account: its token and loaded profile.
pub struct Logon {
    pub token: Handle,
    profile: HANDLE,
    pub profile_dir: String,
}

unsafe impl Send for Logon {}
unsafe impl Sync for Logon {}

impl Logon {
    /// `LogonUser(batch)` + `LoadUserProfile` — the profile must be loaded for the
    /// account's HKCU (account_env) and for tools that read the user's settings.
    pub fn new(user: &str, password: &str) -> std::io::Result<Logon> {
        unsafe {
            let mut token: HANDLE = null_mut();
            let u = wide(user);
            let d = wide(".");
            let p = wide(password);
            if LogonUserW(u.as_ptr(), d.as_ptr(), p.as_ptr(), LOGON32_LOGON_BATCH, LOGON32_PROVIDER_DEFAULT, &mut token) == 0 {
                return Err(last_error("LogonUserW (does the build account have \"log on as a batch job\"?)"));
            }
            let token = Handle(token);
            let mut name = wide(user);
            let mut pi: PROFILEINFOW = std::mem::zeroed();
            pi.dwSize = std::mem::size_of::<PROFILEINFOW>() as u32;
            pi.lpUserName = name.as_mut_ptr();
            if LoadUserProfileW(token.0, &mut pi) == 0 {
                return Err(last_error("LoadUserProfileW"));
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            let profile_dir = if GetUserProfileDirectoryW(token.0, buf.as_mut_ptr(), &mut len) != 0 {
                String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)])
            } else {
                String::new()
            };
            Ok(Logon { token, profile: pi.hProfile, profile_dir })
        }
    }
}

impl Drop for Logon {
    fn drop(&mut self) {
        if !self.profile.is_null() {
            unsafe { UnloadUserProfile(self.token.0, self.profile) };
        }
    }
}

/// DPAPI (machine scope) — the build-account password file lives in `config\`, which
/// only the service account and admins can read.
pub fn protect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out: CRYPT_INTEGER_BLOB = std::mem::zeroed();
        if CryptProtectData(&input, null(), null(), null(), null(), CRYPTPROTECT_LOCAL_MACHINE, &mut out) == 0 {
            return Err(last_error("CryptProtectData"));
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Ok(v)
    }
}

pub fn unprotect(data: &[u8]) -> std::io::Result<Vec<u8>> {
    unsafe {
        let input = CRYPT_INTEGER_BLOB { cbData: data.len() as u32, pbData: data.as_ptr() as *mut u8 };
        let mut out: CRYPT_INTEGER_BLOB = std::mem::zeroed();
        if CryptUnprotectData(&input, null_mut(), null(), null(), null(), 0, &mut out) == 0 {
            return Err(last_error("CryptUnprotectData"));
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData as _);
        Ok(v)
    }
}

/// Starts `cmdline` as the logged-on user, suspended, with stdout/stderr on new pipes
/// and only those handles inherited; assigns it to `job` and resumes it.
/// Returns (process handle, stdout reader, stderr reader).
pub fn spawn_as_user(
    logon: &Logon,
    application: &str,
    cmdline: &str,
    env: &[(String, String)],
    cwd: &str,
    job: &Job,
) -> std::io::Result<(Handle, std::fs::File, std::fs::File)> {
    use std::os::windows::io::FromRawHandle;
    use windows_sys::Win32::Foundation::{SetHandleInformation, HANDLE_FLAG_INHERIT};
    use windows_sys::Win32::Security::SECURITY_ATTRIBUTES;
    use windows_sys::Win32::Storage::FileSystem::{CreateFileW, FILE_GENERIC_READ, FILE_SHARE_READ, FILE_SHARE_WRITE, OPEN_EXISTING};
    use windows_sys::Win32::System::Pipes::CreatePipe;
    use windows_sys::Win32::System::Threading::*;
    unsafe {
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: null_mut(), bInheritHandle: 1 };
        let (mut out_r, mut out_w, mut err_r, mut err_w): (HANDLE, HANDLE, HANDLE, HANDLE) = (null_mut(), null_mut(), null_mut(), null_mut());
        if CreatePipe(&mut out_r, &mut out_w, &sa, 0) == 0 || CreatePipe(&mut err_r, &mut err_w, &sa, 0) == 0 {
            return Err(last_error("CreatePipe"));
        }
        let (out_r, out_w, err_r, err_w) = (Handle(out_r), Handle(out_w), Handle(err_r), Handle(err_w));
        SetHandleInformation(out_r.0, HANDLE_FLAG_INHERIT, 0);
        SetHandleInformation(err_r.0, HANDLE_FLAG_INHERIT, 0);
        let nul = wide("NUL");
        let nul_h = Handle(CreateFileW(nul.as_ptr(), FILE_GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, null_mut()));

        let mut size: usize = 0;
        InitializeProcThreadAttributeList(null_mut(), 1, 0, &mut size);
        let mut attr_buf = vec![0u8; size];
        let attrs = attr_buf.as_mut_ptr() as LPPROC_THREAD_ATTRIBUTE_LIST;
        if InitializeProcThreadAttributeList(attrs, 1, 0, &mut size) == 0 {
            return Err(last_error("InitializeProcThreadAttributeList"));
        }
        let handles: [HANDLE; 3] = [out_w.0, err_w.0, nul_h.0];
        if UpdateProcThreadAttribute(
            attrs,
            0,
            PROC_THREAD_ATTRIBUTE_HANDLE_LIST as usize,
            handles.as_ptr() as *const _,
            std::mem::size_of_val(&handles),
            null_mut(),
            null(),
        ) == 0
        {
            DeleteProcThreadAttributeList(attrs);
            return Err(last_error("UpdateProcThreadAttribute"));
        }
        let mut si: STARTUPINFOEXW = std::mem::zeroed();
        si.StartupInfo.cb = std::mem::size_of::<STARTUPINFOEXW>() as u32;
        si.StartupInfo.dwFlags = STARTF_USESTDHANDLES;
        si.StartupInfo.hStdInput = nul_h.0;
        si.StartupInfo.hStdOutput = out_w.0;
        si.StartupInfo.hStdError = err_w.0;
        si.lpAttributeList = attrs;

        let mut block: Vec<u16> = vec![];
        let mut sorted: Vec<&(String, String)> = env.iter().collect();
        sorted.sort_by_key(|(k, _)| k.to_uppercase());
        for (k, v) in sorted {
            block.extend(OsStr::new(&format!("{k}={v}")).encode_wide());
            block.push(0);
        }
        block.push(0);
        let app = wide(application);
        let mut cmd = wide(cmdline);
        let dir = wide(cwd);
        let mut pi: PROCESS_INFORMATION = std::mem::zeroed();
        let ok = CreateProcessAsUserW(
            logon.token.0,
            app.as_ptr(),
            cmd.as_mut_ptr(),
            null(),
            null(),
            1,
            CREATE_SUSPENDED | CREATE_NO_WINDOW | CREATE_UNICODE_ENVIRONMENT | EXTENDED_STARTUPINFO_PRESENT,
            block.as_ptr() as *const _,
            dir.as_ptr(),
            &si.StartupInfo,
            &mut pi,
        );
        DeleteProcThreadAttributeList(attrs);
        if ok == 0 {
            return Err(last_error("CreateProcessAsUserW (does the service account hold \"replace a process level token\"?)"));
        }
        let process = Handle(pi.hProcess);
        let thread = Handle(pi.hThread);
        if let Err(e) = job.assign(process.0) {
            TerminateProcess(process.0, 1);
            return Err(e);
        }
        ResumeThread(thread.0);
        drop(out_w);
        drop(err_w);
        let out = std::fs::File::from_raw_handle(out_r.0 as _);
        std::mem::forget(out_r);
        let err = std::fs::File::from_raw_handle(err_r.0 as _);
        std::mem::forget(err_r);
        Ok((process, out, err))
    }
}

pub fn wait_process(h: HANDLE, ms: u32) -> bool {
    use windows_sys::Win32::Foundation::WAIT_OBJECT_0;
    use windows_sys::Win32::System::Threading::WaitForSingleObject;
    unsafe { WaitForSingleObject(h, ms) == WAIT_OBJECT_0 }
}

pub fn exit_code(h: HANDLE) -> Option<i64> {
    use windows_sys::Win32::System::Threading::GetExitCodeProcess;
    let mut code: u32 = 0;
    (unsafe { GetExitCodeProcess(h, &mut code) } != 0).then_some(code as i32 as i64)
}

/// Free bytes on the volume holding `path`.
pub fn free_bytes(path: &std::path::Path) -> Option<u64> {
    use windows_sys::Win32::Storage::FileSystem::GetDiskFreeSpaceExW;
    let p = wide(&path.to_string_lossy());
    let mut avail: u64 = 0;
    (unsafe { GetDiskFreeSpaceExW(p.as_ptr(), &mut avail, null_mut(), null_mut()) } != 0).then_some(avail)
}

/// The path a handle really points at, after every link and junction.
pub fn final_path(f: &std::fs::File) -> std::io::Result<std::path::PathBuf> {
    use std::os::windows::ffi::OsStringExt;
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{GetFinalPathNameByHandleW, FILE_NAME_NORMALIZED, VOLUME_NAME_DOS};
    let mut buf = vec![0u16; 1024];
    loop {
        let n = unsafe { GetFinalPathNameByHandleW(f.as_raw_handle() as HANDLE, buf.as_mut_ptr(), buf.len() as u32, FILE_NAME_NORMALIZED | VOLUME_NAME_DOS) } as usize;
        if n == 0 {
            return Err(last_error("GetFinalPathNameByHandleW"));
        }
        if n < buf.len() {
            return Ok(std::ffi::OsString::from_wide(&buf[..n]).into());
        }
        buf.resize(n + 1, 0);
    }
}
