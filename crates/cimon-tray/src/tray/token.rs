//! The user's cimon token from `%USERPROFILE%\.cimon\token.dpapi` — what the connect
//! script wrote with `ConvertFrom-SecureString`: hex of a DPAPI blob (this Windows user)
//! of the UTF-16 token.

use windows_sys::Win32::Foundation::LocalFree;
use windows_sys::Win32::Security::Cryptography::{CryptUnprotectData, CRYPT_INTEGER_BLOB};

pub fn read() -> Option<String> {
    let p = std::path::PathBuf::from(std::env::var_os("USERPROFILE")?).join(".cimon").join("token.dpapi");
    let text = std::fs::read(&p).ok()?;
    let blob = crate::model::decode_hex(&String::from_utf8_lossy(&text))?;
    let input = CRYPT_INTEGER_BLOB { cbData: blob.len() as u32, pbData: blob.as_ptr() as *mut u8 };
    let mut out = CRYPT_INTEGER_BLOB { cbData: 0, pbData: std::ptr::null_mut() };
    // SAFETY: input points at `blob`; the output buffer is LocalFree'd
    let plain = unsafe {
        if CryptUnprotectData(&input, std::ptr::null_mut(), std::ptr::null(), std::ptr::null(), std::ptr::null(), 0, &mut out) == 0 {
            return None;
        }
        let v = std::slice::from_raw_parts(out.pbData, out.cbData as usize).to_vec();
        LocalFree(out.pbData.cast());
        v
    };
    let units: Vec<u16> = plain.chunks_exact(2).map(|c| u16::from_le_bytes([c[0], c[1]])).collect();
    let t = String::from_utf16(&units).ok()?;
    let t = t.trim().to_string();
    (!t.is_empty()).then_some(t)
}
