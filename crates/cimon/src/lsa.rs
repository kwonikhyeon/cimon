//! Account rights through the LSA policy (`cimon setup`).

use anyhow::{bail, Result};

#[cfg(windows)]
pub fn add_rights(account: &str, rights: &[&str]) -> Result<()> {
    use std::ptr::{null, null_mut};
    use windows_sys::Win32::Security::Authentication::Identity::*;
    use windows_sys::Win32::Security::{LookupAccountNameW, SID_NAME_USE};
    let wide = |s: &str| -> Vec<u16> { s.encode_utf16().chain(std::iter::once(0)).collect() };
    unsafe {
        // SID of the account
        let name = wide(account);
        let mut sid_len: u32 = 0;
        let mut dom_len: u32 = 0;
        let mut use_: SID_NAME_USE = 0;
        LookupAccountNameW(null(), name.as_ptr(), null_mut(), &mut sid_len, null_mut(), &mut dom_len, &mut use_);
        if sid_len == 0 {
            bail!("account {account} not found");
        }
        let mut sid = vec![0u8; sid_len as usize];
        let mut dom = vec![0u16; dom_len as usize + 1];
        if LookupAccountNameW(null(), name.as_ptr(), sid.as_mut_ptr() as _, &mut sid_len, dom.as_mut_ptr(), &mut dom_len, &mut use_) == 0 {
            bail!("LookupAccountNameW({account}) failed");
        }
        let attrs: LSA_OBJECT_ATTRIBUTES = std::mem::zeroed();
        let mut policy: LSA_HANDLE = 0;
        let st = LsaOpenPolicy(null(), &attrs, (POLICY_CREATE_ACCOUNT | POLICY_LOOKUP_NAMES) as u32, &mut policy);
        if st != 0 {
            bail!("LsaOpenPolicy failed (status {st:#x}) — run as administrator");
        }
        let mut bufs: Vec<Vec<u16>> = rights.iter().map(|r| r.encode_utf16().collect()).collect();
        let strings: Vec<LSA_UNICODE_STRING> = bufs
            .iter_mut()
            .map(|b| LSA_UNICODE_STRING { Length: (b.len() * 2) as u16, MaximumLength: (b.len() * 2) as u16, Buffer: b.as_mut_ptr() })
            .collect();
        let st = LsaAddAccountRights(policy, sid.as_mut_ptr() as _, strings.as_ptr(), strings.len() as u32);
        LsaClose(policy);
        if st != 0 {
            bail!("LsaAddAccountRights({account}) failed (status {st:#x})");
        }
    }
    Ok(())
}

#[cfg(not(windows))]
pub fn add_rights(_account: &str, _rights: &[&str]) -> Result<()> {
    bail!("account rights are Windows-only")
}
