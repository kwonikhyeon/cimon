//! Types shared by every cimon crate: configuration (`cimon.toml`), the disk layout,
//! verdicts and failure classes, identifiers and time helpers.

pub mod config;
pub mod layout;
pub mod types;

pub use config::Config;
pub use layout::Layout;
pub use types::*;

use sha2::{Digest, Sha256};

/// UTC now as RFC 3339 with seconds precision — the format stored everywhere.
pub fn now() -> String {
    chrono::Utc::now().to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// UTC time `secs` seconds from now, same format as [`now`].
pub fn after_secs(secs: i64) -> String {
    (chrono::Utc::now() + chrono::Duration::seconds(secs)).to_rfc3339_opts(chrono::SecondsFormat::Secs, true)
}

/// Parses a time produced by [`now`].
pub fn parse_time(s: &str) -> Option<chrono::DateTime<chrono::Utc>> {
    chrono::DateTime::parse_from_rfc3339(s).ok().map(|t| t.with_timezone(&chrono::Utc))
}

pub fn sha256_hex(data: &[u8]) -> String {
    hex::encode(Sha256::digest(data))
}

/// 128 random bits rendered as lowercase Crockford base32 (26 chars).
/// Task ids must be unguessable (Tasks extension), so they come from the OS RNG.
pub fn random_id128() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 16];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    base32(&bytes)
}

/// An invite code a person types: 8 characters of Crockford base32 (40 random bits),
/// shown as `XXXX-XXXX`. Short-lived and single-use (`Store::create_invite`).
pub fn invite_code() -> String {
    use rand::RngCore;
    const A: &[u8; 32] = b"0123456789ABCDEFGHJKMNPQRSTVWXYZ";
    let mut bytes = [0u8; 8];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    let s: String = bytes.iter().map(|b| A[(*b & 31) as usize] as char).collect();
    format!("{}-{}", &s[..4], &s[4..])
}

/// The form an invite code is hashed in: upper case, separators dropped, and the
/// letters people mistake for digits read as those digits (Crockford).
pub fn normalize_invite_code(code: &str) -> String {
    code.chars()
        .filter(|c| c.is_ascii_alphanumeric())
        .map(|c| match c.to_ascii_uppercase() {
            'O' => '0',
            'I' | 'L' => '1',
            c => c,
        })
        .collect()
}

/// 256 random bits as hex — tokens, session ids, login codes, keys.
pub fn random_secret_hex() -> String {
    use rand::RngCore;
    let mut bytes = [0u8; 32];
    rand::rngs::OsRng.fill_bytes(&mut bytes);
    hex::encode(bytes)
}

fn base32(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 32] = b"0123456789abcdefghjkmnpqrstvwxyz";
    let mut out = String::new();
    let mut buffer: u32 = 0;
    let mut bits = 0;
    for &b in bytes {
        buffer = (buffer << 8) | b as u32;
        bits += 8;
        while bits >= 5 {
            bits -= 5;
            out.push(ALPHABET[((buffer >> bits) & 31) as usize] as char);
        }
    }
    if bits > 0 {
        out.push(ALPHABET[((buffer << (5 - bits)) & 31) as usize] as char);
    }
    out
}

/// `j000123` style job label.
pub fn job_label(id: i64) -> String {
    format!("j{id:06}")
}

/// Parses `j000123`, `000123` or `123`.
pub fn parse_job_label(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits = s.strip_prefix('j').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

/// `p0012` style proposal label.
pub fn proposal_label(id: i64) -> String {
    format!("p{id:04}")
}

pub fn parse_proposal_label(s: &str) -> Option<i64> {
    let s = s.trim();
    let digits = s.strip_prefix('p').unwrap_or(s);
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    digits.parse().ok()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids() {
        let a = random_id128();
        assert_eq!(a.len(), 26);
        assert_ne!(a, random_id128());
        assert_eq!(job_label(123), "j000123");
        assert_eq!(parse_job_label("j000123"), Some(123));
        assert_eq!(parse_job_label("t_abc"), None);
        assert_eq!(proposal_label(12), "p0012");
        assert_eq!(parse_proposal_label("p0012"), Some(12));
    }

    #[test]
    fn times_roundtrip() {
        let t = now();
        assert!(parse_time(&t).is_some());
        assert!(after_secs(10) > t);
    }
}
