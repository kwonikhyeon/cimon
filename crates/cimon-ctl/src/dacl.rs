//! The cimon service's DACL (docs/tray.md §4.1). `sc.exe create` gives interactive users
//! `CR` (user-defined control) — harmless while the service answers none, but the
//! stop-when-idle control would let anyone logged on stop it. `cimon setup` writes the
//! DACL whole: SYSTEM and administrators as before, interactive and service logons may
//! only query, and `cimon-operators` may start, stop and send the controls — never
//! change the configuration (a new binPath runs anything as the service).

use crate::CtlError;

/// The local group whose members start and stop cimon without UAC.
pub const OPERATORS: &str = "cimon-operators";

const SYSTEM: &str = "(A;;CCLCSWRPWPDTLOCRRC;;;SY)";
const ADMINS: &str = "(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)";
const LOGGED_ON: &str = "(A;;CCLCSWLORC;;;IU)(A;;CCLCSWLORC;;;SU)";
const OPERATOR_RIGHTS: &str = "LCRPWPLOCRRC";

/// The DACL `cimon setup` writes, with the operators group's SID when the group exists.
pub fn service_sddl(operators_sid: Option<&str>) -> String {
    let mut s = format!("D:{SYSTEM}{ADMINS}{LOGGED_ON}");
    if let Some(sid) = operators_sid {
        s.push_str(&format!("(A;;{OPERATOR_RIGHTS};;;{sid})"));
    }
    s
}

// service access rights (winsvc.h, winnt.h)
const CHANGE_CONFIG: u32 = 0x2;
const START: u32 = 0x10;
const STOP: u32 = 0x20;
const PAUSE_CONTINUE: u32 = 0x40;
const USER_DEFINED_CONTROL: u32 = 0x100;
const DELETE: u32 = 0x1_0000;
const WRITE_DAC: u32 = 0x4_0000;
const WRITE_OWNER: u32 = 0x8_0000;
const GENERIC_WRITE: u32 = 0x4000_0000;
const GENERIC_ALL: u32 = 0x1000_0000;

/// An SDDL rights field (`CCLCRP…` or `0x…`) as an access mask; unknown letters make
/// the mask "everything" so they are reported, not trusted.
fn rights_mask(r: &str) -> u32 {
    if let Some(hex) = r.strip_prefix("0x").or_else(|| r.strip_prefix("0X")) {
        return u32::from_str_radix(hex, 16).unwrap_or(u32::MAX);
    }
    let mut m = 0;
    let b = r.as_bytes();
    for pair in b.chunks(2) {
        m |= match pair {
            b"CC" => 0x1,
            b"DC" => CHANGE_CONFIG,
            b"LC" => 0x4,
            b"SW" => 0x8,
            b"RP" => START,
            b"WP" => STOP,
            b"DT" => PAUSE_CONTINUE,
            b"LO" => 0x80,
            b"CR" => USER_DEFINED_CONTROL,
            b"SD" => DELETE,
            b"RC" => 0x2_0000,
            b"WD" => WRITE_DAC,
            b"WO" => WRITE_OWNER,
            b"GA" => GENERIC_ALL,
            b"GW" => GENERIC_WRITE,
            b"GR" => 0x8000_0000,
            b"GX" => 0x2000_0000,
            _ => u32::MAX,
        };
    }
    m
}

/// What is wrong with a service DACL for cimon: anyone but SYSTEM and administrators
/// able to change the service, or a broad group (interactive, service logon, everyone,
/// authenticated or local users) able to start, stop or send controls.
pub fn problems(sddl: &str) -> Vec<String> {
    let dacl = sddl.split("S:").next().unwrap_or(sddl);
    let dacl = dacl.split_once("D:").map(|(_, d)| d).unwrap_or(dacl);
    let mut out = vec![];
    for ace in dacl.split('(').skip(1) {
        let ace = ace.split(')').next().unwrap_or_default();
        let f: Vec<&str> = ace.split(';').collect();
        if f.len() < 6 || f[0] != "A" {
            continue;
        }
        let (rights, who) = (rights_mask(f[2]), f[5]);
        if matches!(who, "SY" | "BA") {
            continue;
        }
        if rights & (CHANGE_CONFIG | DELETE | WRITE_DAC | WRITE_OWNER | GENERIC_WRITE | GENERIC_ALL) != 0 {
            out.push(format!("{who} can change or delete the service ({})", f[2]));
        } else if matches!(who, "IU" | "SU" | "WD" | "AU" | "BU" | "BG") && rights & (START | STOP | PAUSE_CONTINUE | USER_DEFINED_CONTROL) != 0 {
            out.push(format!("{who} can start, stop or control the service ({})", f[2]));
        }
    }
    out
}

#[cfg(windows)]
mod win {
    use super::*;
    use windows_sys::Win32::Foundation::LocalFree;
    use windows_sys::Win32::Security::Authorization::{ConvertSecurityDescriptorToStringSecurityDescriptorW, ConvertStringSecurityDescriptorToSecurityDescriptorW};
    use windows_sys::Win32::Security::DACL_SECURITY_INFORMATION;
    use windows_sys::Win32::System::Services::{CloseServiceHandle, OpenSCManagerW, OpenServiceW, QueryServiceObjectSecurity, SetServiceObjectSecurity, SC_MANAGER_CONNECT};

    const READ_CONTROL: u32 = 0x2_0000;
    const WRITE_DAC_RIGHT: u32 = 0x4_0000;
    const SDDL_REVISION_1: u32 = 1;

    fn wide(s: &str) -> Vec<u16> {
        s.encode_utf16().chain(std::iter::once(0)).collect()
    }

    fn last() -> CtlError {
        let e = std::io::Error::last_os_error();
        match e.raw_os_error() {
            Some(5) => CtlError::AccessDenied,
            Some(1060) => CtlError::NotInstalled,
            _ => CtlError::Other(e.to_string()),
        }
    }

    /// Runs `f` on the service opened with `access`.
    fn with_service<T>(access: u32, f: impl FnOnce(windows_sys::Win32::System::Services::SC_HANDLE) -> Result<T, CtlError>) -> Result<T, CtlError> {
        // SAFETY: NUL-terminated names; handles are closed on every path
        unsafe {
            let m = OpenSCManagerW(std::ptr::null(), std::ptr::null(), SC_MANAGER_CONNECT);
            if m.is_null() {
                return Err(last());
            }
            let name = wide(crate::SERVICE);
            let s = OpenServiceW(m, name.as_ptr(), access);
            if s.is_null() {
                let e = last();
                CloseServiceHandle(m);
                return Err(e);
            }
            let r = f(s);
            CloseServiceHandle(s);
            CloseServiceHandle(m);
            r
        }
    }

    /// The service's DACL as SDDL (`D:…`).
    pub fn read() -> Result<String, CtlError> {
        with_service(READ_CONTROL, |s| unsafe {
            let mut need = 0u32;
            QueryServiceObjectSecurity(s, DACL_SECURITY_INFORMATION, std::ptr::null_mut(), 0, &mut need);
            if need == 0 {
                return Err(last());
            }
            let mut buf = vec![0u8; need as usize];
            if QueryServiceObjectSecurity(s, DACL_SECURITY_INFORMATION, buf.as_mut_ptr().cast(), need, &mut need) == 0 {
                return Err(last());
            }
            let mut out: *mut u16 = std::ptr::null_mut();
            if ConvertSecurityDescriptorToStringSecurityDescriptorW(buf.as_mut_ptr().cast(), SDDL_REVISION_1, DACL_SECURITY_INFORMATION, &mut out, std::ptr::null_mut()) == 0 {
                return Err(last());
            }
            let mut n = 0;
            while *out.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, n));
            LocalFree(out.cast());
            Ok(s)
        })
    }

    /// The SID of the `cimon-operators` group as a string, if the group exists.
    pub fn operators_sid() -> Option<String> {
        use windows_sys::Win32::Security::Authorization::ConvertSidToStringSidW;
        use windows_sys::Win32::Security::LookupAccountNameW;
        let name = wide(OPERATORS);
        // SAFETY: sized buffers from the first call; the string SID is LocalFree'd
        unsafe {
            let (mut sid_len, mut dom_len, mut use_) = (0u32, 0u32, 0i32);
            LookupAccountNameW(std::ptr::null(), name.as_ptr(), std::ptr::null_mut(), &mut sid_len, std::ptr::null_mut(), &mut dom_len, &mut use_);
            if sid_len == 0 {
                return None;
            }
            let mut sid = vec![0u8; sid_len as usize];
            let mut dom = vec![0u16; dom_len.max(1) as usize];
            if LookupAccountNameW(std::ptr::null(), name.as_ptr(), sid.as_mut_ptr().cast(), &mut sid_len, dom.as_mut_ptr(), &mut dom_len, &mut use_) == 0 {
                return None;
            }
            let mut out: *mut u16 = std::ptr::null_mut();
            if ConvertSidToStringSidW(sid.as_mut_ptr().cast(), &mut out) == 0 {
                return None;
            }
            let mut n = 0;
            while *out.add(n) != 0 {
                n += 1;
            }
            let s = String::from_utf16_lossy(std::slice::from_raw_parts(out, n));
            LocalFree(out.cast());
            Some(s)
        }
    }

    /// Replaces the service's DACL (administrator).
    pub fn write(sddl: &str) -> Result<(), CtlError> {
        let w = wide(sddl);
        with_service(WRITE_DAC_RIGHT | READ_CONTROL, |s| unsafe {
            let mut sd: *mut core::ffi::c_void = std::ptr::null_mut();
            if ConvertStringSecurityDescriptorToSecurityDescriptorW(w.as_ptr(), SDDL_REVISION_1, &mut sd, std::ptr::null_mut()) == 0 {
                return Err(last());
            }
            let ok = SetServiceObjectSecurity(s, DACL_SECURITY_INFORMATION, sd);
            let e = if ok == 0 { Some(last()) } else { None };
            LocalFree(sd);
            e.map_or(Ok(()), Err)
        })
    }
}

#[cfg(windows)]
pub use win::{operators_sid, read, write};

#[cfg(not(windows))]
pub fn operators_sid() -> Option<String> {
    None
}

#[cfg(not(windows))]
pub fn read() -> Result<String, CtlError> {
    Err(CtlError::NotInstalled)
}

#[cfg(not(windows))]
pub fn write(_sddl: &str) -> Result<(), CtlError> {
    Err(CtlError::NotInstalled)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// What `sc.exe sdshow cimon` printed on the real machine before setup wrote it.
    const SC_CREATE_DEFAULT: &str = "D:(A;;CCLCSWRPWPDTLOCRRC;;;SY)(A;;CCDCLCSWRPWPDTLOCRSDRCWDWO;;;BA)(A;;CCLCSWLOCRRC;;;IU)(A;;CCLCSWLOCRRC;;;SU)";

    #[test]
    fn the_default_lets_logged_on_users_send_controls() {
        let p = problems(SC_CREATE_DEFAULT);
        assert_eq!(p.len(), 2, "{p:?}");
        assert!(p[0].starts_with("IU can start, stop or control"), "{p:?}");
    }

    #[test]
    fn the_written_dacl_has_no_problems() {
        assert!(problems(&service_sddl(None)).is_empty());
        let with = service_sddl(Some("S-1-5-21-1-2-3-1001"));
        assert!(with.ends_with("(A;;LCRPWPLOCRRC;;;S-1-5-21-1-2-3-1001)"), "{with}");
        assert!(problems(&with).is_empty(), "operators may start, stop and control");
    }

    #[test]
    fn changing_the_service_is_reported_for_anyone_but_system_and_admins() {
        let bad = format!("{}(A;;LCDC;;;S-1-5-21-9)", service_sddl(None));
        assert!(problems(&bad).iter().any(|p| p.contains("S-1-5-21-9 can change")));
        assert!(!problems("D:(A;;0x2;;;WD)").is_empty(), "hex rights are read");
        assert!(!problems("D:(A;;CCZZ;;;IU)").is_empty(), "unknown letters are not trusted");
        assert!(problems("D:(D;;CCLCRPWP;;;IU)").is_empty(), "deny entries are not grants");
    }
}
