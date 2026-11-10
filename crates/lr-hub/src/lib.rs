//! Hub role (spec 3.1, 13.3): the client side of one OS user. It takes CLI commands over
//! local IPC, turns profiles into orders, keeps them in the outbox until a broker accepts
//! them, chooses brokers by `hub-policy.toml`, copies job logs into the submitter's
//! directory and cancels the orders of dead submitters. It executes no job.

mod cli;
mod client;
mod decide;
mod engine;
mod handles;
mod inbound;
mod ipc_server;
mod logs;
mod orders;
mod outbox;
pub mod policy;
pub mod reaper;
pub mod select;
mod track;

pub use engine::{Hub, HubDeps, LOCAL_BROKER};
pub use policy::{
    parse_hub_policy, BrokerStrategy, BrokersPolicy, HubPolicy, HubPolicyError, OnExpire,
    OutboxPolicy, ZoneQuery,
};
