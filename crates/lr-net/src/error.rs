use std::fmt;

/// Why a network or identity operation of this crate failed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NetError {
    /// File or socket I/O failed.
    Io(String),
    /// TLS setup failed (certificate, key or configuration).
    Tls(String),
    /// A fingerprint string is not `sha256:` and 64 hex digits.
    BadFingerprint(String),
    /// A key string or key file is not a valid ed25519 key.
    BadKey(String),
    /// A signature does not verify.
    BadSignature,
    /// A listen address is refused: wildcard, `container` outside a container, or malformed.
    BadListenAddress(String),
    /// A secret file is readable by others than its owner.
    InsecureFile(String),
    /// The OS did not report the user of a local peer.
    PeerUser(String),
}

impl fmt::Display for NetError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            NetError::Io(e) => write!(f, "i/o: {e}"),
            NetError::Tls(e) => write!(f, "tls: {e}"),
            NetError::BadFingerprint(s) => write!(f, "not a certificate fingerprint: {s}"),
            NetError::BadKey(e) => write!(f, "bad key: {e}"),
            NetError::BadSignature => f.write_str("signature does not verify"),
            NetError::BadListenAddress(e) => write!(f, "listen address refused: {e}"),
            NetError::InsecureFile(p) => {
                write!(
                    f,
                    "{p} is accessible to other users; restrict it to its owner"
                )
            }
            NetError::PeerUser(e) => write!(f, "cannot tell the OS user of the peer: {e}"),
        }
    }
}

impl std::error::Error for NetError {}

impl From<std::io::Error> for NetError {
    fn from(e: std::io::Error) -> Self {
        NetError::Io(e.to_string())
    }
}
