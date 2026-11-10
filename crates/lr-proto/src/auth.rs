use crate::common::ProtocolError;
use crate::frame::message;
use serde::{Deserialize, Serialize};

/// Messages of the broker in the key proof that opens every network connection, after TLS and
/// before [`crate::Hello`] or [`crate::Join`] (spec 3.9): a fresh nonce per connection, then
/// the verdict on the proof.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthFromBroker {
    /// A fresh random nonce the peer must sign with its key.
    Challenge {
        /// Nonce, URL-safe base64 without padding.
        nonce: String,
    },
    /// The proof holds and the key may go on; the protocol messages follow.
    Authenticated,
    /// The proof failed, or the key is revoked; the connection closes.
    Error(ProtocolError),
}

/// The peer's answer to [`AuthFromBroker::Challenge`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum AuthToBroker {
    /// The public key and its signature over the nonce bound to the broker certificate.
    Proof {
        /// Public key of the peer, as the broker stores it.
        public_key: String,
        /// Signature, URL-safe base64 without padding.
        signature: String,
    },
}

message!(
    AuthFromBroker,
    local: false,
    ["challenge", "authenticated", "error"]
);

message!(AuthToBroker, local: false, ["proof"]);
