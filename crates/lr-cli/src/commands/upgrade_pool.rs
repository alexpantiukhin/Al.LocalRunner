//! The other runners of the pool, as `upgrade` shows them: before the change as machines it does
//! not touch, after it as the runners that are still on another version. Each machine runs its
//! own `upgrade`; nothing here changes a remote node.

use super::read_cmd::fetch_runners;
use super::upgrade_cmd::{Version, RECONNECT_WITHIN};
use crate::paths::Layout;
use lr_proto::RunnerInfo;

/// How a runner of the pool can be updated.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    /// A process on a machine with its own `localrunner upgrade`.
    Native,
    /// A runner in a container: its image is rebuilt or replaced by hand.
    Container,
    /// The runner row does not say; the broker keeps no container flag the CLI can read.
    Unknown,
}

/// A runner of another installation, as the brokers list it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PoolRunner {
    /// Its name.
    pub name: String,
    /// The machine it reports.
    pub machine: Option<String>,
    /// Its `localrunner` version, when it reports one.
    pub version: Option<String>,
    /// Whether it is connected.
    pub online: bool,
    /// How it is updated.
    pub kind: Kind,
}

const LOCAL: &str = "local:";

/// Whether `row` is runner `name` of this installation: the broker lists it as `name`, or as
/// `local:name` when it connected over local IPC, and its machine is this host's (`local:...`)
/// or not reported.
pub fn is_own(row: &PoolRunner, name: &str) -> bool {
    let here = row.machine.as_deref().is_none_or(|m| m.starts_with(LOCAL));
    here && (row.name == name || row.name.strip_prefix(LOCAL) == Some(name))
}

/// The runners of the brokers the hub knows, without those of this installation named in `own`
/// (see [`is_own`]).
pub fn rows(infos: impl IntoIterator<Item = RunnerInfo>, own: &[String]) -> Vec<PoolRunner> {
    infos
        .into_iter()
        .map(|r| PoolRunner {
            name: r.name,
            machine: r.caps.machine.map(|m| m.0),
            version: r.caps.version,
            online: r.online,
            kind: Kind::Unknown,
        })
        .filter(|r| !own.iter().any(|n| is_own(r, n)))
        .collect()
}

/// Every runner the brokers known to the hub of this OS user list, this installation's
/// included; empty without a hub or a broker.
pub async fn all_runners(layout: &Layout) -> Vec<PoolRunner> {
    let Ok(client) = super::jobs::hub(layout).await else {
        return Vec::new();
    };
    let fetched = fetch_runners(&client, None).await;
    client.close().await;
    match fetched {
        Ok((per, _)) => rows(per.into_iter().flat_map(|(_, runners)| runners), &[]),
        Err(_) => Vec::new(),
    }
}

/// The pool as the hub of this OS user sees it, without the runners of this installation;
/// empty without a hub or a broker.
pub async fn pool(layout: &Layout) -> Vec<PoolRunner> {
    let own = layout.runner_names();
    let mut all = all_runners(layout).await;
    all.retain(|r| !own.iter().any(|n| is_own(r, n)));
    all
}

/// Whether restarted runner `name` is back: listed in `now` online on version `new`.
pub fn reconnected(now: &[PoolRunner], name: &str, new: &str) -> Reconnected {
    let back = now
        .iter()
        .any(|r| is_own(r, name) && r.online && r.version.as_deref() == Some(new));
    match back {
        true => Reconnected::Yes,
        false => Reconnected::No,
    }
}

fn describe(r: &PoolRunner) -> String {
    let machine = r.machine.as_deref().unwrap_or("unknown machine");
    let version = r.version.as_deref().unwrap_or("unknown");
    let online = if r.online { "" } else { ", offline" };
    format!("  {} on {machine}: version {version}{online}", r.name)
}

/// The plan's lines about the other runners: who they are and that this command leaves them
/// alone. `new` is the version being installed.
pub fn plan_lines(pool: &[PoolRunner], new: &str) -> Vec<String> {
    if pool.is_empty() {
        return Vec::new();
    }
    let mut out = vec![format!(
        "other runners of the pool (not touched; each machine runs its own `localrunner upgrade` to {new}):"
    )];
    out.extend(pool.iter().map(describe));
    out
}

/// Whether a runner that `upgrade` restarted came back to its broker on the new version.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reconnected {
    /// It is online on the new version.
    Yes,
    /// It did not come back within the wait.
    No,
}

/// The report's lines after the replacement: the runners of this installation that `upgrade`
/// restarted and that did not reconnect within the wait, then the runners of `pool` that are
/// not on the new version and what to do about each. A runner of `restarted` is left out of
/// `pool`, which may have been read before the restart.
pub fn report_lines(
    pool: &[PoolRunner],
    restarted: &[(String, Reconnected)],
    new: &Version,
    protocol_changed: bool,
) -> Vec<String> {
    let mut lost: Vec<String> = restarted
        .iter()
        .filter(|(_, back)| *back == Reconnected::No)
        .map(|(name, _)| {
            format!(
                "{name}: did not reconnect within {} s after the restart; check it: localrunner doctor",
                RECONNECT_WITHIN.as_secs()
            )
        })
        .collect();
    let mut out = Vec::new();
    for r in pool
        .iter()
        .filter(|r| !restarted.iter().any(|(n, _)| is_own(r, n)))
    {
        let machine = r.machine.as_deref().unwrap_or("its machine");
        let current = r.version.as_deref() == Some(new.text.as_str());
        if current {
            continue;
        }
        if r.kind == Kind::Container {
            out.push(format!(
                "{} on {machine}: not updated: runs in a container, manual step (replace its image)",
                r.name
            ));
        } else if !r.online {
            out.push(format!(
                "{} on {machine}: not updated: offline; run `localrunner upgrade` there when it is back",
                r.name
            ));
        } else {
            let was = r.version.as_deref().unwrap_or("an unknown version");
            out.push(format!(
                "{} on {machine}: still on {was}; run `localrunner upgrade` there",
                r.name
            ));
        }
    }
    if protocol_changed && !out.is_empty() {
        out.push(
            "the major protocol version changed: these runners cannot work with this node until they are upgraded"
                .into(),
        );
    }
    lost.append(&mut out);
    lost
}
