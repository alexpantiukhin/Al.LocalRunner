use crate::error::NetError;
use crate::files::{read_private, write_private};
use crate::fingerprint::Fingerprint;
use base64::engine::general_purpose::URL_SAFE_NO_PAD;
use base64::Engine;
use ed25519_dalek::{Signature, Signer, SigningKey, VerifyingKey};
use std::fmt;
use std::path::Path;
use zeroize::Zeroizing;

/// Prefix of a public key string: `ed25519:` and the 32 key bytes in URL-safe base64.
pub const PUBLIC_KEY_PREFIX: &str = "ed25519:";

/// Bytes of a challenge nonce.
pub const NONCE_LEN: usize = 32;

const SIGN_CONTEXT: &[u8] = b"al-localrunner/auth/v1\0";
const SECRET_FILE_HEADER: &str = "al-localrunner ed25519 secret key v1";

/// The message a key signs to prove itself on one connection: a fixed context, the broker's
/// certificate fingerprint and the connection's fresh nonce. A signature is therefore good for
/// one broker and one connection only.
pub fn challenge_message(fingerprint: &Fingerprint, nonce: &[u8]) -> Vec<u8> {
    let mut msg = Vec::with_capacity(SIGN_CONTEXT.len() + 32 + nonce.len());
    msg.extend_from_slice(SIGN_CONTEXT);
    msg.extend_from_slice(fingerprint.as_bytes());
    msg.extend_from_slice(nonce);
    msg
}

/// Fills a fresh random nonce from the OS generator.
pub fn fresh_nonce() -> Result<[u8; NONCE_LEN], NetError> {
    let mut nonce = [0u8; NONCE_LEN];
    getrandom::getrandom(&mut nonce).map_err(|e| NetError::Io(format!("random: {e}")))?;
    Ok(nonce)
}

/// The ed25519 key pair of one participant (a hub or a runner), created by itself at join;
/// the public half goes to the broker, the secret half never leaves the participant's state
/// directory. `Debug` shows the public key only.
pub struct ClientKey {
    signing: SigningKey,
}

impl ClientKey {
    /// A new random key pair.
    pub fn generate() -> Result<ClientKey, NetError> {
        let mut seed = Zeroizing::new([0u8; 32]);
        getrandom::getrandom(&mut *seed).map_err(|e| NetError::Io(format!("random: {e}")))?;
        Ok(ClientKey {
            signing: SigningKey::from_bytes(&seed),
        })
    }

    /// The public key as stored by the broker: `ed25519:<base64url>`.
    pub fn public_key(&self) -> String {
        encode_public(&self.signing.verifying_key())
    }

    /// Signs the challenge of one connection, see [`challenge_message`].
    pub fn sign_challenge(&self, fingerprint: &Fingerprint, nonce: &[u8]) -> String {
        let sig = self.signing.sign(&challenge_message(fingerprint, nonce));
        URL_SAFE_NO_PAD.encode(sig.to_bytes())
    }

    /// Saves the secret key to `path`, readable by the current OS user only.
    pub fn save(&self, path: &Path) -> Result<(), NetError> {
        let text = Zeroizing::new(format!(
            "{SECRET_FILE_HEADER}\n{}\n",
            URL_SAFE_NO_PAD.encode(self.signing.to_bytes())
        ));
        write_private(path, text.as_bytes())
    }

    /// Loads a key saved by [`ClientKey::save`]; a file readable by others is refused.
    pub fn load(path: &Path) -> Result<ClientKey, NetError> {
        let bytes = Zeroizing::new(read_private(path)?);
        let text = std::str::from_utf8(&bytes)
            .map_err(|_| NetError::BadKey(format!("{} is not text", path.display())))?;
        let mut lines = text.lines();
        if lines.next() != Some(SECRET_FILE_HEADER) {
            return Err(NetError::BadKey(format!(
                "{} is not a key file",
                path.display()
            )));
        }
        let seed = Zeroizing::new(
            URL_SAFE_NO_PAD
                .decode(lines.next().unwrap_or_default().trim())
                .map_err(|_| NetError::BadKey(format!("{} is damaged", path.display())))?,
        );
        let seed: &[u8; 32] = seed
            .as_slice()
            .try_into()
            .map_err(|_| NetError::BadKey(format!("{} is damaged", path.display())))?;
        Ok(ClientKey {
            signing: SigningKey::from_bytes(seed),
        })
    }
}

impl fmt::Debug for ClientKey {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("ClientKey")
            .field("public_key", &self.public_key())
            .field("secret", &"<redacted>")
            .finish()
    }
}

fn encode_public(key: &VerifyingKey) -> String {
    format!(
        "{PUBLIC_KEY_PREFIX}{}",
        URL_SAFE_NO_PAD.encode(key.as_bytes())
    )
}

/// Parses a public key string, refusing anything but a valid ed25519 point.
pub fn parse_public_key(s: &str) -> Result<VerifyingKey, NetError> {
    let bad = || NetError::BadKey(format!("not an ed25519 public key: {s}"));
    let b64 = s.strip_prefix(PUBLIC_KEY_PREFIX).ok_or_else(bad)?;
    let bytes = URL_SAFE_NO_PAD.decode(b64).map_err(|_| bad())?;
    let bytes: [u8; 32] = bytes.as_slice().try_into().map_err(|_| bad())?;
    let key = VerifyingKey::from_bytes(&bytes).map_err(|_| bad())?;
    if key.is_weak() {
        return Err(bad());
    }
    Ok(key)
}

/// Checks that `signature` is `public_key`'s signature of this connection's challenge. Uses
/// strict verification: malleable or non-canonical signatures are refused.
pub fn verify_challenge(
    public_key: &str,
    fingerprint: &Fingerprint,
    nonce: &[u8],
    signature: &str,
) -> Result<(), NetError> {
    let key = parse_public_key(public_key)?;
    let raw = URL_SAFE_NO_PAD
        .decode(signature)
        .map_err(|_| NetError::BadSignature)?;
    let raw: [u8; 64] = raw
        .as_slice()
        .try_into()
        .map_err(|_| NetError::BadSignature)?;
    let sig = Signature::from_bytes(&raw);
    let msg = challenge_message(fingerprint, nonce);
    key.verify_strict(&msg, &sig)
        .map_err(|_| NetError::BadSignature)
}
