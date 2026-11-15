use base64::engine::general_purpose::STANDARD;
use base64::Engine;

/// The OpenSSH public key lines this build trusts: `LR_RELEASE_KEYS` at build time, one key
/// per line; empty when it was not set.
pub fn trusted_keys() -> Vec<String> {
    option_env!("LR_RELEASE_KEYS")
        .unwrap_or("")
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .map(String::from)
        .collect()
}

/// The 32 bytes of an `ssh-ed25519 <base64> [comment]` line; `None` for any other key type or a
/// malformed line.
pub(super) fn ed25519_key(line: &str) -> Option<[u8; 32]> {
    let mut words = line.split_whitespace();
    if words.next()? != "ssh-ed25519" {
        return None;
    }
    let blob = STANDARD.decode(words.next()?).ok()?;
    let mut fields = Fields(&blob);
    if fields.string()? != b"ssh-ed25519" {
        return None;
    }
    let key = fields.string()?;
    fields.0.is_empty().then_some(())?;
    key.try_into().ok()
}

/// Length-prefixed fields of the SSH wire format.
pub(super) struct Fields<'a>(pub &'a [u8]);

impl<'a> Fields<'a> {
    pub(super) fn u32(&mut self) -> Option<u32> {
        let (head, rest) = self.0.split_first_chunk::<4>()?;
        self.0 = rest;
        Some(u32::from_be_bytes(*head))
    }

    pub(super) fn string(&mut self) -> Option<&'a [u8]> {
        let len = usize::try_from(self.u32()?).ok()?;
        (self.0.len() >= len).then(|| {
            let (field, rest) = self.0.split_at(len);
            self.0 = rest;
            field
        })
    }
}
