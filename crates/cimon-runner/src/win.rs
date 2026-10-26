//! Win32 pieces: Job Objects, starting suspended processes, the build account's token
//! and profile, DPAPI for the stored build-account password.

#![cfg(windows)]
#![allow(clippy::missing_safety_doc, clippy::not_unsafe_ptr_arg_deref)] // HANDLEs are opaque kernel handles, never dereferenced

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
    /// `HKEY_USERS\<SID>` when this process loaded the hive itself (see `new`).
    hive: Option<Vec<u16>>,
    pub profile_dir: String,
}

unsafe impl Send for Logon {}
unsafe impl Sync for Logon {}

impl Logon {
    /// `LogonUser(batch)` + the account's profile — loaded for its HKCU (account_env)
    /// and for tools that read the user's settings.
    ///
    /// `LoadUserProfile` works for an administrator (`cimon setup`, `cimon doctor`). The
    /// User Profile Service refuses it to the service account even with the backup and
    /// restore privileges (ERROR_ACCESS_DENIED, seen on a real machine), so then the
    /// service loads the profile's hive itself — `RegLoadKey` into `HKEY_USERS\<SID>`
    /// needs exactly those two privileges, and a process of that user then finds its
    /// HKCU there. The profile must exist already: `cimon setup` creates it as an
    /// administrator.
    pub fn new(user: &str, password: &str) -> std::io::Result<Logon> {
        let missing: Vec<&str> = ["SeBackupPrivilege", "SeRestorePrivilege"].into_iter().filter(|p| enable_privilege(p).is_err()).collect();
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
            pi.dwFlags = 1; // PI_NOUI — never show a dialog from a service
            pi.lpUserName = name.as_mut_ptr();
            let mut logon = Logon { token, profile: null_mut(), hive: None, profile_dir: String::new() };
            if LoadUserProfileW(logon.token.0, &mut pi) != 0 {
                logon.profile = pi.hProfile;
            } else {
                let code = GetLastError();
                if !missing.is_empty() {
                    return Err(last_error(&format!("LoadUserProfileW — the service account lacks {} (run `cimon setup` again)", missing.join(" and "))));
                }
                if code != windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED {
                    return Err(last_error("LoadUserProfileW"));
                }
            }
            let mut buf = vec![0u16; 1024];
            let mut len = buf.len() as u32;
            if GetUserProfileDirectoryW(logon.token.0, buf.as_mut_ptr(), &mut len) == 0 {
                // without it USERPROFILE/APPDATA would be empty and tools would write anywhere
                return Err(last_error(&format!("GetUserProfileDirectoryW — {user} has no profile yet; run `cimon setup` (as an administrator it creates the profile)")));
            }
            logon.profile_dir = String::from_utf16_lossy(&buf[..(len as usize).saturating_sub(1)]);
            if logon.profile.is_null() {
                logon.hive = load_hive(&logon.token, &logon.profile_dir)?;
            }
            Ok(logon)
        }
    }
}

/// Enables a privilege the process token holds (it may be present but disabled).
pub fn enable_privilege(name: &str) -> std::io::Result<()> {
    use windows_sys::Win32::Foundation::{ERROR_NOT_ALL_ASSIGNED, LUID};
    use windows_sys::Win32::Security::{AdjustTokenPrivileges, LookupPrivilegeValueW, LUID_AND_ATTRIBUTES, SE_PRIVILEGE_ENABLED, TOKEN_ADJUST_PRIVILEGES, TOKEN_PRIVILEGES, TOKEN_QUERY};
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut tok: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_ADJUST_PRIVILEGES | TOKEN_QUERY, &mut tok) == 0 {
            return Err(last_error("OpenProcessToken"));
        }
        let tok = Handle(tok);
        let mut luid: LUID = std::mem::zeroed();
        let n = wide(name);
        if LookupPrivilegeValueW(null(), n.as_ptr(), &mut luid) == 0 {
            return Err(last_error("LookupPrivilegeValueW"));
        }
        let tp = TOKEN_PRIVILEGES { PrivilegeCount: 1, Privileges: [LUID_AND_ATTRIBUTES { Luid: luid, Attributes: SE_PRIVILEGE_ENABLED }] };
        if AdjustTokenPrivileges(tok.0, 0, &tp, 0, null_mut(), null_mut()) == 0 || GetLastError() == ERROR_NOT_ALL_ASSIGNED {
            return Err(std::io::Error::new(std::io::ErrorKind::PermissionDenied, format!("{name} is not held")));
        }
        Ok(())
    }
}

impl Drop for Logon {
    fn drop(&mut self) {
        if !self.profile.is_null() {
            unsafe { UnloadUserProfile(self.token.0, self.profile) };
        }
        if let Some(key) = &self.hive {
            use windows_sys::Win32::System::Registry::{RegUnLoadKeyW, HKEY_USERS};
            // fails while a process of the account still holds a key — the sweep ends them
            unsafe { RegUnLoadKeyW(HKEY_USERS, key.as_ptr()) };
        }
    }
}

/// Loads `<profile_dir>\NTUSER.DAT` as `HKEY_USERS\<SID of token>`. Returns the key this
/// call loaded, or None when the hive is loaded already (someone else unloads it).
fn load_hive(token: &Handle, profile_dir: &str) -> std::io::Result<Option<Vec<u16>>> {
    use windows_sys::Win32::System::Registry::{RegLoadKeyW, HKEY_USERS};
    let sid = token_sid(token)?;
    let key = wide(&sid);
    let file = wide(&format!("{profile_dir}\\NTUSER.DAT"));
    match unsafe { RegLoadKeyW(HKEY_USERS, key.as_ptr(), file.as_ptr()) } {
        0 => Ok(Some(key)),
        // ERROR_SHARING_VIOLATION: the hive file is open — loaded already (a logged-on
        // session of the account, or a profile another caller loaded)
        32 => Ok(None),
        e => Err(std::io::Error::other(
            format!("RegLoadKeyW HKEY_USERS\\{sid} from {profile_dir}\\NTUSER.DAT failed (Win32 error {e}) — the service account needs the backup and restore privileges (`cimon setup`)"),
        )),
    }
}

/// The user SID of `token` as a string (`S-1-5-21-…`).
fn token_sid(token: &Handle) -> std::io::Result<String> {
    use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
    use windows_sys::Win32::Security::{GetTokenInformation, TokenUser, TOKEN_USER};
    unsafe {
        let mut len = 0u32;
        GetTokenInformation(token.0, TokenUser, null_mut(), 0, &mut len);
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token.0, TokenUser, buf.as_mut_ptr() as *mut _, len, &mut len) == 0 {
            return Err(last_error("GetTokenInformation(TokenUser)"));
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let mut s: windows_sys::core::PWSTR = null_mut();
        if ConvertSidToStringSidW(tu.User.Sid, &mut s) == 0 {
            return Err(last_error("ConvertSidToStringSidW"));
        }
        let mut n = 0;
        while *s.add(n) != 0 {
            n += 1;
        }
        let out = String::from_utf16_lossy(std::slice::from_raw_parts(s, n));
        LocalFree(s as _);
        Ok(out)
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
        // Every handle starts non-inheritable: other threads spawn processes with std
        // (git, probes) that inherit all inheritable handles, and one of them holding a
        // write end would keep this step's pipe open after the step ends. The child's
        // three handles become inheritable only around CreateProcessAsUserW.
        let sa = SECURITY_ATTRIBUTES { nLength: std::mem::size_of::<SECURITY_ATTRIBUTES>() as u32, lpSecurityDescriptor: null_mut(), bInheritHandle: 0 };
        let pipe = || -> std::io::Result<(Handle, Handle)> {
            let (mut r, mut w): (HANDLE, HANDLE) = (null_mut(), null_mut());
            if CreatePipe(&mut r, &mut w, &sa, 0) == 0 {
                return Err(last_error("CreatePipe"));
            }
            Ok((Handle(r), Handle(w)))
        };
        let (out_r, out_w) = pipe()?;
        let (err_r, err_w) = pipe()?;
        let nul = wide("NUL");
        let nul_h = Handle(CreateFileW(nul.as_ptr(), FILE_GENERIC_READ, FILE_SHARE_READ | FILE_SHARE_WRITE, &sa, OPEN_EXISTING, 0, null_mut()));
        if nul_h.0.is_null() || nul_h.0 == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateFileW(NUL)"));
        }

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
        // empty, not NULL: the child does not inherit the service's window station (the
        // build account has no access to it); the system picks one for its logon session
        let mut desktop = wide("");
        si.StartupInfo.lpDesktop = desktop.as_mut_ptr();
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
        for h in handles {
            SetHandleInformation(h, HANDLE_FLAG_INHERIT, HANDLE_FLAG_INHERIT);
        }
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
        let create_err = (ok == 0).then(|| last_error("CreateProcessAsUserW (does the service account hold \"replace a process level token\"?)"));
        for h in handles {
            SetHandleInformation(h, HANDLE_FLAG_INHERIT, 0);
        }
        DeleteProcThreadAttributeList(attrs);
        if let Some(e) = create_err {
            return Err(e);
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

/// Marks the file behind `f` for deletion when its last handle closes. `f` must have
/// been opened with DELETE access.
pub fn delete_on_close(f: &std::fs::File) -> std::io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::Storage::FileSystem::{FileDispositionInfo, SetFileInformationByHandle, FILE_DISPOSITION_INFO};
    let info = FILE_DISPOSITION_INFO { DeleteFile: 1 };
    let ok = unsafe {
        SetFileInformationByHandle(f.as_raw_handle() as HANDLE, FileDispositionInfo, &info as *const _ as *const core::ffi::c_void, std::mem::size_of::<FILE_DISPOSITION_INFO>() as u32)
    };
    if ok == 0 {
        return Err(last_error("SetFileInformationByHandle"));
    }
    Ok(())
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


/// One process from a Toolhelp snapshot.
#[derive(Debug, Clone)]
pub struct ProcInfo {
    pub pid: u32,
    pub parent: u32,
    pub name: String,
}

pub fn processes() -> std::io::Result<Vec<ProcInfo>> {
    use windows_sys::Win32::System::Diagnostics::ToolHelp::{Process32FirstW, Process32NextW, PROCESSENTRY32W, TH32CS_SNAPPROCESS};
    unsafe {
        let snap = CreateToolhelp32Snapshot(TH32CS_SNAPPROCESS, 0);
        if snap == INVALID_HANDLE_VALUE {
            return Err(last_error("CreateToolhelp32Snapshot"));
        }
        let snap = Handle(snap);
        let mut pe: PROCESSENTRY32W = std::mem::zeroed();
        pe.dwSize = std::mem::size_of::<PROCESSENTRY32W>() as u32;
        let mut out = vec![];
        if Process32FirstW(snap.0, &mut pe) != 0 {
            loop {
                let len = pe.szExeFile.iter().position(|c| *c == 0).unwrap_or(pe.szExeFile.len());
                out.push(ProcInfo { pid: pe.th32ProcessID, parent: pe.th32ParentProcessID, name: String::from_utf16_lossy(&pe.szExeFile[..len]) });
                if Process32NextW(snap.0, &mut pe) == 0 {
                    break;
                }
            }
        }
        Ok(out)
    }
}

fn token_user_sid(token: HANDLE) -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::{GetLengthSid, GetTokenInformation, TokenUser, TOKEN_USER};
    unsafe {
        let mut len = 0u32;
        GetTokenInformation(token, TokenUser, null_mut(), 0, &mut len);
        if len == 0 {
            return None;
        }
        let mut buf = vec![0u8; len as usize];
        if GetTokenInformation(token, TokenUser, buf.as_mut_ptr() as _, len, &mut len) == 0 {
            return None;
        }
        let tu = &*(buf.as_ptr() as *const TOKEN_USER);
        let n = GetLengthSid(tu.User.Sid) as usize;
        Some(std::slice::from_raw_parts(tu.User.Sid as *const u8, n).to_vec())
    }
}

/// The SID of the user this process runs as.
pub fn current_user_sid() -> Option<Vec<u8>> {
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{GetCurrentProcess, OpenProcessToken};
    unsafe {
        let mut tok: HANDLE = null_mut();
        if OpenProcessToken(GetCurrentProcess(), TOKEN_QUERY, &mut tok) == 0 {
            return None;
        }
        let tok = Handle(tok);
        token_user_sid(tok.0)
    }
}

/// Does `pid` run as `sid`?
pub fn owned_by(pid: u32, sid: &[u8]) -> bool {
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{OpenProcess, OpenProcessToken, PROCESS_QUERY_LIMITED_INFORMATION};
    unsafe {
        let h = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            return false;
        }
        let h = Handle(h);
        let mut tok: HANDLE = null_mut();
        if OpenProcessToken(h.0, TOKEN_QUERY, &mut tok) == 0 {
            return false;
        }
        let tok = Handle(tok);
        token_user_sid(tok.0).as_deref() == Some(sid)
    }
}

/// Terminates `pid` when it runs as `sid`. Ok(false): another user's process (left alone).
pub fn terminate_if_owned(pid: u32, sid: &[u8]) -> std::io::Result<bool> {
    use windows_sys::Win32::Security::TOKEN_QUERY;
    use windows_sys::Win32::System::Threading::{OpenProcess, OpenProcessToken, TerminateProcess, PROCESS_QUERY_LIMITED_INFORMATION, PROCESS_TERMINATE};
    unsafe {
        let h = OpenProcess(PROCESS_TERMINATE | PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if h.is_null() {
            // not ours to open: another user's process
            return Ok(false);
        }
        let h = Handle(h);
        let mut tok: HANDLE = null_mut();
        if OpenProcessToken(h.0, TOKEN_QUERY, &mut tok) == 0 {
            return Ok(false);
        }
        let tok = Handle(tok);
        if token_user_sid(tok.0).as_deref() != Some(sid) {
            return Ok(false);
        }
        if TerminateProcess(h.0, 1) == 0 {
            return Err(last_error("TerminateProcess"));
        }
        Ok(true)
    }
}

/// Decodes console output in the OEM code page (cmd and older tools on a Korean
/// system write CP949, not UTF-8).
pub fn decode_oem(bytes: &[u8]) -> Option<String> {
    use windows_sys::Win32::Globalization::{GetOEMCP, MultiByteToWideChar};
    if bytes.is_empty() {
        return Some(String::new());
    }
    unsafe {
        let cp = GetOEMCP();
        let n = MultiByteToWideChar(cp, 0, bytes.as_ptr(), bytes.len() as i32, null_mut(), 0);
        if n <= 0 {
            return None;
        }
        let mut buf = vec![0u16; n as usize];
        let n = MultiByteToWideChar(cp, 0, bytes.as_ptr(), bytes.len() as i32, buf.as_mut_ptr(), n);
        (n > 0).then(|| String::from_utf16_lossy(&buf[..n as usize]))
    }
}

/// Cancels a blocking read on another thread (a pipe whose write end leaked into an
/// unrelated process never reaches end-of-file).
pub fn cancel_thread_io(thread: HANDLE) {
    use windows_sys::Win32::System::IO::CancelSynchronousIo;
    unsafe { CancelSynchronousIo(thread) };
}
