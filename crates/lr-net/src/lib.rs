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
mod dialer;
mod error;
mod files;
mod fingerprint;
mod ipc;
mod keys;
mod listen_addr;
mod listener;
mod local_users;
mod registry;
mod slots;
mod stream;
mod tls;

#[cfg(windows)]
mod acl_windows;
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
pub use dialer::{TlsDialer, TlsTransport};
pub use error::NetError;
pub use files::{read_private, write_private, write_public};
pub use fingerprint::Fingerprint;
pub use ipc::{
    current_os_user, local_machine_name, IpcAccess, IpcDialer, IpcListener, ServerIdentity,
    PREAMBLE, PREAMBLE_TIMEOUT,
};
pub use keys::{
    canonical_public_key, challenge_message, fresh_nonce, parse_public_key, verify_challenge,
    ClientKey, BINDING_LEN, NONCE_LEN, PUBLIC_KEY_PREFIX,
};
pub use listen_addr::{ListenAddr, DEFAULT_PORT};
pub use listener::TlsListener;
pub use local_users::{peer_kind, LocalClient, LocalUser, LocalUsers, LOCAL_CLIENT_PREFIX};
pub use registry::{check_name, CodeRequest, JoinRequest, Registry, RegistryError, MAX_NAME_LEN};
pub use slots::{Limits, HANDSHAKE_TIMEOUT, MAX_PENDING_HANDSHAKES};
pub use stream::StreamTransport;
pub use tls::{
    channel_binding, pinned_client_config, provider, server_name, BrokerIdentity, CERT_FILE,
    EXPORTER_LABEL, KEY_FILE,
};
