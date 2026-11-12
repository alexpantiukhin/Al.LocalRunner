//! Wire protocol 3.9: messages per channel, frames of at most 4 MiB, version negotiation and
//! reason codes. Depends on `lr-core` only; the role crates convert messages to their rows.

mod api;
mod auth;
mod caps;
mod common;
mod dashboard;
mod frame;
mod hub;
mod ipc;
mod job;
mod reason;
mod runner;
mod seq;
mod stats_view;
mod version;

pub use api::*;
pub use auth::{AuthFromBroker, AuthToBroker};
pub use caps::{WireCapabilities, WireLoad, WireServiceStatus};
pub use common::*;
pub use dashboard::*;
pub use frame::{
    decode, decode_for_peer, encode, encode_frame, parse_prefix, read_frame, receive, write_frame,
    DecodeError, FrameError, Message, PeerKind, Received, ADMIN_TYPES, FRAME_LIMIT, PREFIX_LEN,
};
pub use hub::*;
pub use ipc::*;
pub use job::*;
pub use reason::{ReasonCode, RejectReason};
pub use runner::{BrokerToRunner, RunnerToBroker};
pub use seq::{classify_seq, DedupKey, Direction, SeqCheck, SeqStream, Sequenced};
pub use stats_view::*;
pub use version::{
    negotiate, negotiate_between, Version, VersionRefused, HEARTBEAT_INTERVAL_S,
    MISSED_HEARTBEATS_OFFLINE, PROTOCOL_VERSION,
};
