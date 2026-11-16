use super::keys::{ed25519_key, Fields};
use super::VerifyError;
use base64::engine::general_purpose::STANDARD;
use base64::Engine;
use ed25519_dalek::{Signature, VerifyingKey};
use sha2::{Digest, Sha256, Sha512};

const MAGIC: &[u8] = b"SSHSIG";
const BEGIN: &str = "-----BEGIN SSH SIGNATURE-----";
const END: &str = "-----END SSH SIGNATURE-----";

fn unarmor(text: &str) -> Result<Vec<u8>, VerifyError> {
    let body = text
        .trim()
        .strip_prefix(BEGIN)
        .and_then(|rest| rest.trim_end().strip_suffix(END))
        .ok_or(VerifyError::BadArmor)?;
    let joined: String = body.split_whitespace().collect();
    STANDARD.decode(joined).map_err(|_| VerifyError::BadArmor)
}

fn put(out: &mut Vec<u8>, field: &[u8]) {
    out.extend_from_slice(&u32::try_from(field.len()).unwrap_or(u32::MAX).to_be_bytes());
    out.extend_from_slice(field);
}

/// Checks that `armored_sig` is an OpenSSH signature of `signed` in `namespace` by one of
/// `keys` (OpenSSH public key lines; any of them may have signed, so keys can rotate).
pub fn verify_signature(
    keys: &[String],
    namespace: &str,
    signed: &[u8],
    armored_sig: &str,
) -> Result<(), VerifyError> {
    if keys.iter().all(|k| k.trim().is_empty()) {
        return Err(VerifyError::NoKeys);
    }
    let blob = unarmor(armored_sig)?;
    let rest = blob.strip_prefix(MAGIC).ok_or(VerifyError::BadArmor)?;
    let mut fields = Fields(rest);
    if fields.u32() != Some(1) {
        return Err(VerifyError::BadArmor);
    }
    let public = fields.string().ok_or(VerifyError::BadArmor)?;
    let sig_namespace = fields.string().ok_or(VerifyError::BadArmor)?;
    let reserved = fields.string().ok_or(VerifyError::BadArmor)?;
    let algorithm = fields.string().ok_or(VerifyError::BadArmor)?;
    let signature = fields.string().ok_or(VerifyError::BadArmor)?;
    if !fields.0.is_empty() {
        return Err(VerifyError::BadArmor);
    }
    let mut key_fields = Fields(public);
    let (kind, key) = (key_fields.string(), key_fields.string());
    if kind != Some(b"ssh-ed25519".as_slice()) || !key_fields.0.is_empty() {
        return Err(VerifyError::BadArmor);
    }
    let key: [u8; 32] = key
        .and_then(|k| k.try_into().ok())
        .ok_or(VerifyError::BadArmor)?;
    let mut sig_fields = Fields(signature);
    let (sig_kind, sig) = (sig_fields.string(), sig_fields.string());
    if sig_kind != Some(b"ssh-ed25519".as_slice()) || !sig_fields.0.is_empty() {
        return Err(VerifyError::BadArmor);
    }
    let sig: [u8; 64] = sig
        .and_then(|s| s.try_into().ok())
        .ok_or(VerifyError::BadArmor)?;
    let digest = match algorithm {
        b"sha256" => Sha256::digest(signed).to_vec(),
        b"sha512" => Sha512::digest(signed).to_vec(),
        _ => return Err(VerifyError::BadArmor),
    };
    if sig_namespace != namespace.as_bytes() {
        return Err(VerifyError::BadNamespace);
    }
    if !keys.iter().any(|k| ed25519_key(k) == Some(key)) {
        return Err(VerifyError::UnknownKey);
    }
    let mut message = MAGIC.to_vec();
    put(&mut message, sig_namespace);
    put(&mut message, reserved);
    put(&mut message, algorithm);
    put(&mut message, &digest);
    let verifying = VerifyingKey::from_bytes(&key).map_err(|_| VerifyError::BadSignature)?;
    verifying
        .verify_strict(&message, &Signature::from_bytes(&sig))
        .map_err(|_| VerifyError::BadSignature)
}
