//! Verification of a release inside the binary: the OpenSSH signature of `SHA256SUMS` against
//! the keys built into this binary, then the SHA-256 of the downloaded file against its line of
//! `SHA256SUMS`. No `ssh-keygen` is needed on the machine.

mod archive;
mod fetch;
mod keys;
mod sshsig;
mod sums;
mod update;

use std::fmt;

pub use fetch::{asset_name, asset_url, latest_version, valid_version, Fetch, UreqFetch};
pub use keys::trusted_keys;
pub use sshsig::verify_signature;
pub use sums::{check_sha256, sums_entry};
pub use update::{fetch_release, from_offline};

/// The namespace `release.yml` signs `SHA256SUMS` with.
pub const NAMESPACE: &str = "al-localrunner-release";

/// Why a release did not verify.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum VerifyError {
    /// This build carries no trusted key (`LR_RELEASE_KEYS` was empty at build time).
    NoKeys,
    /// The signature is not a well-formed OpenSSH `SSHSIG`.
    BadArmor,
    /// The signature was made by a key this build does not trust.
    UnknownKey,
    /// The signature is for another purpose than a release.
    BadNamespace,
    /// The signature does not match the signed bytes.
    BadSignature,
    /// The file does not hash to the value in `SHA256SUMS`.
    HashMismatch,
    /// `SHA256SUMS` has no line for the file.
    MissingEntry,
}

impl fmt::Display for VerifyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            VerifyError::NoKeys => {
                "this build trusts no release key, so it cannot verify a release"
            }
            VerifyError::BadArmor => "the signature is not a valid OpenSSH signature",
            VerifyError::UnknownKey => "the signature was made by a key this build does not trust",
            VerifyError::BadNamespace => "the signature is not a release signature",
            VerifyError::BadSignature => "the signature does not match SHA256SUMS",
            VerifyError::HashMismatch => "the downloaded file does not match its SHA-256",
            VerifyError::MissingEntry => "SHA256SUMS has no line for the file",
        })
    }
}

impl std::error::Error for VerifyError {}
