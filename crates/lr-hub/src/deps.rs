use crate::policy::HubPolicy;
use lr_abstractions::{
    Clock, Dialer, Files, HubStore, Listener, LogSink, Meter, ProcessInspector, Repository,
};
use std::sync::Arc;

/// What the hub is built from; only `lr-cli` knows the implementations behind them.
pub struct HubDeps {
    /// Time source.
    pub clock: Arc<dyn Clock>,
    /// `hub.db` and the log copies.
    pub store: Arc<dyn HubStore>,
    /// Local IPC of the CLI.
    pub ipc: Arc<dyn Listener>,
    /// Connections to brokers: TLS to the joined ones, local IPC to the local one.
    pub dialer: Arc<dyn Dialer>,
    /// IPC address of the broker of this machine, listed as [`crate::LOCAL_BROKER`].
    pub local_broker: Option<String>,
    /// Liveness of submitter processes (reaper).
    pub inspector: Arc<dyn ProcessInspector>,
    /// Profile files of work trees and job handles.
    pub files: Arc<dyn Files>,
    /// Commits, push state and committed profiles.
    pub repository: Arc<dyn Repository>,
    /// Diagnostic events.
    pub log: Arc<dyn LogSink>,
    /// Metrics of spec 12.2.
    pub meter: Arc<dyn Meter>,
    /// `hub-policy.toml`.
    pub policy: HubPolicy,
    /// Name of this hub in `hello`.
    pub name: String,
}
