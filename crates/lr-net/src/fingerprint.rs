use crate::error::NetError;
use sha2::{Digest, Sha256};
use std::fmt;
use std::str::FromStr;

/// SHA-256 of a broker certificate (DER). Shown and stored as `sha256:` and 64 lowercase hex
/// digits; carried inside the join code; pinned by every hub and runner that joined.
#[derive(Clone, Copy, PartialEq, Eq, Hash)]
pub struct Fingerprint([u8; 32]);

impl Fingerprint {
    /// The fingerprint of a DER-encoded certificate.
    pub fn of_der(der: &[u8]) -> Fingerprint {
        Fingerprint(Sha256::digest(der).into())
    }

    /// Wraps raw digest bytes.
    pub fn from_bytes(bytes: [u8; 32]) -> Fingerprint {
        Fingerprint(bytes)
    }

    /// The raw digest bytes.
    pub fn as_bytes(&self) -> &[u8; 32] {
        &self.0
    }

    /// Parses `sha256:<64 hex>`; hex case is ignored.
    pub fn parse(s: &str) -> Result<Fingerprint, NetError> {
        let bad = || NetError::BadFingerprint(s.to_string());
        let hex = s.trim().strip_prefix("sha256:").ok_or_else(bad)?;
        if hex.len() != 64 || !hex.is_ascii() {
            return Err(bad());
        }
        let mut out = [0u8; 32];
        for (i, byte) in out.iter_mut().enumerate() {
            *byte = u8::from_str_radix(&hex[2 * i..2 * i + 2], 16).map_err(|_| bad())?;
        }
        Ok(Fingerprint(out))
    }
}

impl fmt::Display for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("sha256:")?;
        for b in self.0 {
            write!(f, "{b:02x}")?;
        }
        Ok(())
    }
}

impl fmt::Debug for Fingerprint {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "Fingerprint({self})")
    }
}

impl FromStr for Fingerprint {
    type Err = NetError;

    fn from_str(s: &str) -> Result<Self, Self::Err> {
        Fingerprint::parse(s)
    }
}
