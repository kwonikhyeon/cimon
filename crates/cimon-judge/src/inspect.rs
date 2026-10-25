//! Built-in artifact inspections: `apk-signer` (APK Signature Scheme v2/v3 signer
//! certificate) and `pe-version` (PE version resource).

use serde_json::{json, Value};
use std::path::Path;

pub fn run_builtin(name: &str, file: &Path) -> Result<Value, String> {
    match name {
        "apk-signer" => apk_signer(file),
        "pe-version" => pe_version(file),
        other => Err(format!("unknown inspection {other}")),
    }
}

/// Finds the APK Signing Block and returns the first signer's certificate fingerprint
/// and subject.
pub fn apk_signer(file: &Path) -> Result<Value, String> {
    let data = std::fs::read(file).map_err(|e| e.to_string())?;
    let eocd = find_eocd(&data).ok_or("not a ZIP/APK (no end of central directory)")?;
    let cd_offset = u32::from_le_bytes(data[eocd + 16..eocd + 20].try_into().unwrap()) as usize;
    if cd_offset < 32 || cd_offset > data.len() {
        return Err("bad central directory offset".into());
    }
    let magic = &data[cd_offset - 16..cd_offset];
    if magic != b"APK Sig Block 42" {
        return Err("no APK Signing Block — unsigned or v1-only APK".into());
    }
    let size_footer = u64::from_le_bytes(data[cd_offset - 24..cd_offset - 16].try_into().unwrap()) as usize;
    let block_start = cd_offset.checked_sub(size_footer + 8).ok_or("bad signing block size")?;
    let mut pos = block_start + 8;
    let end = cd_offset - 24;
    let mut found: Option<(u32, &[u8])> = None;
    while pos + 12 <= end {
        let len = u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap()) as usize;
        if len < 4 || pos + 8 + len > end + 24 {
            break;
        }
        let id = u32::from_le_bytes(data[pos + 8..pos + 12].try_into().unwrap());
        let value = &data[pos + 12..pos + 8 + len];
        if id == 0xf05368c0 || id == 0x7109871a {
            // prefer v3 when present
            if found.is_none() || id == 0xf05368c0 {
                found = Some((id, value));
            }
        }
        pos += 8 + len;
    }
    let (id, value) = found.ok_or("signing block has no v2/v3 signature")?;
    let cert = first_certificate(value).ok_or("could not read the signer certificate")?;
    let fingerprint = cimon_core::sha256_hex(cert);
    let subject = x509_parser::parse_x509_certificate(cert).map(|(_, c)| c.subject().to_string()).unwrap_or_default();
    Ok(json!({
        "scheme": if id == 0xf05368c0 { "v3" } else { "v2" },
        "cert_sha256": fingerprint,
        "subject": subject,
    }))
}

fn find_eocd(data: &[u8]) -> Option<usize> {
    if data.len() < 22 {
        return None;
    }
    let min = data.len().saturating_sub(22 + 65535);
    (min..=data.len() - 22).rev().find(|&i| data[i..i + 4] == [0x50, 0x4b, 0x05, 0x06])
}

/// signers(len-prefixed seq) → signer → signed data → digests, certificates → first cert.
fn first_certificate(v: &[u8]) -> Option<&[u8]> {
    fn lp(b: &[u8]) -> Option<(&[u8], &[u8])> {
        if b.len() < 4 {
            return None;
        }
        let n = u32::from_le_bytes(b[..4].try_into().ok()?) as usize;
        (b.len() >= 4 + n).then(|| (&b[4..4 + n], &b[4 + n..]))
    }
    let (signers, _) = lp(v)?;
    let (signer, _) = lp(signers)?;
    let (signed_data, _) = lp(signer)?;
    let (_digests, rest) = lp(signed_data)?;
    let (certs, _) = lp(rest)?;
    let (cert, _) = lp(certs)?;
    Some(cert)
}

pub fn pe_version(file: &Path) -> Result<Value, String> {
    use pelite::FileMap;
    let map = FileMap::open(file).map_err(|e| e.to_string())?;
    let pe = pelite::PeFile::from_bytes(&map).map_err(|e| e.to_string())?;
    let res = pe.resources().map_err(|e| format!("no resources: {e}"))?;
    let vi = res.version_info().map_err(|e| format!("no version resource: {e}"))?;
    let fixed = vi.fixed();
    let mut out = json!({});
    if let Some(f) = fixed {
        out["file_version"] = json!(format!("{}.{}.{}.{}", f.dwFileVersion.Major, f.dwFileVersion.Minor, f.dwFileVersion.Patch, f.dwFileVersion.Build));
        out["product_version"] =
            json!(format!("{}.{}.{}.{}", f.dwProductVersion.Major, f.dwProductVersion.Minor, f.dwProductVersion.Patch, f.dwProductVersion.Build));
    }
    if let Some(lang) = vi.translation().first() {
        for key in ["ProductName", "FileDescription", "CompanyName", "ProductVersion", "FileVersion"] {
            if let Some(v) = vi.value(*lang, key) {
                out[key] = json!(v);
            }
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pe_version_reads_a_system_binary() {
        let cmd = std::path::PathBuf::from(std::env::var("ComSpec").unwrap());
        let v = pe_version(&cmd).unwrap();
        assert!(v["file_version"].as_str().unwrap().contains('.'), "{v}");
    }

    #[test]
    fn apk_signer_rejects_non_apk() {
        let d = tempfile::tempdir().unwrap();
        let f = d.path().join("x.apk");
        std::fs::write(&f, b"not a zip").unwrap();
        assert!(apk_signer(&f).is_err());
    }
}
