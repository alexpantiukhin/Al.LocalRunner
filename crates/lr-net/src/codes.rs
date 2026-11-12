use crate::error::NetError;
use crate::fingerprint::Fingerprint;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use sha2::{Digest, Sha256};
use std::fmt;
use zeroize::Zeroizing;

/// Default lifetime of a join code in minutes, `[join] code_ttl_min` (spec 3.8).
pub const DEFAULT_CODE_TTL_MIN: u64 = 60;

const PREFIX: &str = "lrj1";
const SECRET_LEN: usize = 32;
const CHECK_LEN: usize = 4;
const CHECK_CONTEXT: &[u8] = b"al-localrunner/join-code-check/v1\0";
const HASH_CONTEXT: &[u8] = b"al-localrunner/join-code-hash/v1\0";

/// Why a join code string is refused before any connection is made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CodeError {
    /// Not of the form `lrj1.<fingerprint>.<secret>.<check>`.
    Malformed(String),
    /// The parts do not match their check: a typo, a cut copy, or an altered fingerprint.
    Damaged,
}

impl fmt::Display for CodeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CodeError::Malformed(why) => write!(f, "not a join code: {why}"),
            CodeError::Damaged => {
                f.write_str("join code is damaged (typo, cut copy or altered fingerprint)")
            }
        }
    }
}

impl std::error::Error for CodeError {}

/// A one-time join code (spec 3.8): a compound token carrying the broker's certificate
/// fingerprint, so `join` needs only the address and the code, and 256 random bits.
///
/// String form: `lrj1.<fingerprint>.<secret>.<check>`, each part URL-safe base64 without
/// padding; `check` covers the fingerprint and the secret. The broker stores only
/// [`JoinCode::hash`]. `Debug` hides the secret; the full code is only produced by
/// [`JoinCode::reveal`], for printing it once to the broker owner.
#[derive(Clone, PartialEq, Eq)]
pub struct JoinCode {
    fingerprint: Fingerprint,
    secret: Zeroizing<[u8; SECRET_LEN]>,
}

impl JoinCode {
    /// A new random code for the broker with this certificate fingerprint.
    pub fn generate(fingerprint: Fingerprint) -> Result<JoinCode, NetError> {
        let mut secret = Zeroizing::new([0u8; SECRET_LEN]);
        getrandom::getrandom(&mut *secret).map_err(|e| NetError::Io(format!("random: {e}")))?;
        Ok(JoinCode {
            fingerprint,
            secret,
        })
    }

    /// Parses a code typed or pasted by the owner; surrounding whitespace is ignored.
    pub fn parse(s: &str) -> Result<JoinCode, CodeError> {
        let parts: Vec<&str> = s.trim().split('.').collect();
        let [prefix, fp, secret, check] = parts.as_slice() else {
            return Err(CodeError::Malformed(
                "expected four dot-separated parts".into(),
            ));
        };
        if *prefix != PREFIX {
            return Err(CodeError::Malformed(format!("expected prefix `{PREFIX}`")));
        }
        let fp: [u8; 32] = decode_exact(fp, "fingerprint")?;
        let secret: [u8; SECRET_LEN] = decode_exact(secret, "secret")?;
        let check: [u8; CHECK_LEN] = decode_exact(check, "check")?;
        let code = JoinCode {
            fingerprint: Fingerprint::from_bytes(fp),
            secret: Zeroizing::new(secret),
        };
        if code.check() != check {
            return Err(CodeError::Damaged);
        }
        Ok(code)
    }

    /// The broker certificate fingerprint the joiner pins.
    pub fn fingerprint(&self) -> Fingerprint {
        self.fingerprint
    }

    /// What the broker stores instead of the code: `sha256:` and hex of the fingerprint and
    /// secret under a fixed context. The secret has 256 random bits, so a fast hash suffices.
    pub fn hash(&self) -> String {
        let mut h = Sha256::new();
        h.update(HASH_CONTEXT);
        h.update(self.fingerprint.as_bytes());
        h.update(*self.secret);
        Fingerprint::from_bytes(h.finalize().into()).to_string()
    }

    /// The full code string, to be shown once to the broker owner.
    pub fn reveal(&self) -> String {
        format!(
            "{PREFIX}.{}.{}.{}",
            URL_SAFE_NO_PAD.encode(self.fingerprint.as_bytes()),
            URL_SAFE_NO_PAD.encode(*self.secret),
            URL_SAFE_NO_PAD.encode(self.check())
        )
    }

    fn check(&self) -> [u8; CHECK_LEN] {
        let mut h = Sha256::new();
        h.update(CHECK_CONTEXT);
        h.update(self.fingerprint.as_bytes());
        h.update(*self.secret);
        let digest = h.finalize();
        let mut out = [0u8; CHECK_LEN];
        out.copy_from_slice(&digest[..CHECK_LEN]);
        out
    }
}

impl fmt::Debug for JoinCode {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("JoinCode")
            .field("fingerprint", &self.fingerprint)
            .field("secret", &"<redacted>")
            .finish()
    }
}

fn decode_exact<const N: usize>(part: &str, what: &str) -> Result<[u8; N], CodeError> {
    let bytes = Zeroizing::new(
        URL_SAFE_NO_PAD
            .decode(part)
            .map_err(|_| CodeError::Malformed(format!("{what} is not base64url")))?,
    );
    bytes
        .as_slice()
        .try_into()
        .map_err(|_| CodeError::Malformed(format!("{what} has the wrong length")))
}
