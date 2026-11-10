//! Network and local transports of the roles (spec 3.8, 3.9, 13.2, 17): the broker's TLS
//! listener with a self-signed certificate pinned by fingerprint, the signed-nonce key proof
//! on every connection, one-time join codes carrying the fingerprint, ed25519 participant
//! keys, the machine registry over [`lr_abstractions::BrokerStore`], the `[[local_user]]`
//! mapping, and local IPC (named pipe on Windows, Unix socket elsewhere) with the peer's OS
//! user taken from the kernel.
//!
//! TLS and IPC share one framing ([`codec`]): a 4-byte big-endian length, then one JSON
//! document of at most 4 MiB; the length is checked before the body is read or allocated.

pub mod codec;

mod auth;
mod codes;
mod error;
mod files;
mod fingerprint;
mod ipc;
mod keys;
mod listener;
mod local_users;
mod registry;
mod stream;
mod tls;

#[cfg(unix)]
mod ipc_unix;
#[cfg(windows)]
mod ipc_windows;
#[cfg(unix)]
mod os_unix;
#[cfg(windows)]
mod os_windows;

pub use auth::{refusal_code, refused, Admit, KeyGate, AUTH_FRAME_LIMIT};
pub use codes::{CodeError, JoinCode, DEFAULT_CODE_TTL_MIN};
pub use error::NetError;
pub use files::{read_private, write_private};
pub use fingerprint::Fingerprint;
pub use ipc::{current_os_user, IpcAccess, IpcDialer, IpcListener, PREAMBLE, PREAMBLE_TIMEOUT};
pub use keys::{
    challenge_message, fresh_nonce, parse_public_key, verify_challenge, ClientKey, NONCE_LEN,
    PUBLIC_KEY_PREFIX,
};
pub use listener::{
    ListenAddr, TlsDialer, TlsListener, TlsTransport, DEFAULT_PORT, HANDSHAKE_TIMEOUT,
    MAX_PENDING_HANDSHAKES,
};
pub use local_users::{peer_kind, LocalClient, LocalUser, LocalUsers, LOCAL_CLIENT_PREFIX};
pub use registry::{check_name, CodeRequest, JoinRequest, Registry, RegistryError, MAX_NAME_LEN};
pub use stream::StreamTransport;
pub use tls::{pinned_client_config, provider, server_name, BrokerIdentity, CERT_FILE, KEY_FILE};
