use crate::common::{ProtocolError, ProtocolErrorCode};
use serde::{Deserialize, Serialize};
use std::fmt;

/// Seconds between heartbeats on a persistent connection (spec 3.9).
pub const HEARTBEAT_INTERVAL_S: u64 = 15;

/// Missed heartbeats after which a runner or a client counts as offline (spec 3.9, 15.4).
pub const MISSED_HEARTBEATS_OFFLINE: u32 = 3;

/// Protocol version carried by the handshake: roles with an equal `major` work together, the
/// `minor` only grows by adding messages and fields that an older peer may ignore.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct Version {
    /// Incompatible changes; a broker refuses a different major.
    pub major: u16,
    /// Compatible additions.
    pub minor: u16,
}

impl fmt::Display for Version {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}.{}", self.major, self.minor)
    }
}

/// The version this build speaks: wire version 1.0.
pub const PROTOCOL_VERSION: Version = Version { major: 1, minor: 0 };

/// The handshake was refused because the majors differ.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct VersionRefused {
    /// Version the peer announced.
    pub peer: Version,
    /// Version this side speaks.
    pub ours: Version,
}

impl VersionRefused {
    /// The protocol error to send back, naming the supported version.
    pub fn into_error(self) -> ProtocolError {
        ProtocolError {
            code: ProtocolErrorCode::UnsupportedVersion,
            message: self.to_string(),
            supported: Some(self.ours),
        }
    }
}

impl fmt::Display for VersionRefused {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "unsupported protocol version {}: this side speaks {}.x, update the older side",
            self.peer, self.ours.major
        )?;
        write!(f, " (exact version {})", self.ours)
    }
}

impl std::error::Error for VersionRefused {}

/// Agrees on a version with a peer that announced `peer`, against [`PROTOCOL_VERSION`].
pub fn negotiate(peer: Version) -> Result<Version, VersionRefused> {
    negotiate_between(PROTOCOL_VERSION, peer)
}

/// Agrees on a version between `ours` and `peer`: a different major is refused, the same major
/// works at the lower minor.
pub fn negotiate_between(ours: Version, peer: Version) -> Result<Version, VersionRefused> {
    if ours.major != peer.major {
        return Err(VersionRefused { peer, ours });
    }
    Ok(Version {
        major: ours.major,
        minor: ours.minor.min(peer.minor),
    })
}
