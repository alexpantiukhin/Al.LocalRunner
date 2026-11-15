use super::VerifyError;
use sha2::{Digest, Sha256};

/// The SHA-256 `sums` (the text of `SHA256SUMS`) lists for `file`; `None` when there is no such
/// line or its hash is not 64 hex digits. A leading `*` (binary mode) on the name is ignored.
pub fn sums_entry(sums: &str, file: &str) -> Option<[u8; 32]> {
    sums.lines().find_map(|line| {
        let (hash, name) = line.trim_end().split_once(char::is_whitespace)?;
        let name = name.trim_start().trim_start_matches('*');
        (name == file).then(|| decode_hex(hash)).flatten()
    })
}

fn decode_hex(text: &str) -> Option<[u8; 32]> {
    if text.len() != 64 || !text.is_ascii() {
        return None;
    }
    let mut out = [0u8; 32];
    for (i, byte) in out.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&text[2 * i..2 * i + 2], 16).ok()?;
    }
    Some(out)
}

/// Checks that `bytes` hash to `expected`.
pub fn check_sha256(bytes: &[u8], expected: [u8; 32]) -> Result<(), VerifyError> {
    match Sha256::digest(bytes).as_slice() == expected {
        true => Ok(()),
        false => Err(VerifyError::HashMismatch),
    }
}
