//! The other runners of the pool, as `upgrade` shows them: before the change as machines it does
//! not touch, after it as the runners that are still on another version. Each machine runs its
//! own `upgrade`; nothing here changes a remote node.

use super::read_cmd::fetch_runners;
use super::upgrade_cmd::Version;
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

/// The runners of the brokers the hub knows, without those of this installation: a row is this
/// installation's when its name is one of `own` and its machine is this host's (`local:...`) or
/// not reported.
pub fn rows(infos: impl IntoIterator<Item = RunnerInfo>, own: &[String]) -> Vec<PoolRunner> {
    infos
        .into_iter()
        .filter_map(|r| {
            let machine = r.caps.machine.map(|m| m.0);
            let here = machine.as_deref().is_none_or(|m| m.starts_with("local:"));
            (!(here && own.contains(&r.name))).then_some(PoolRunner {
                name: r.name,
                machine,
                version: r.caps.version,
                online: r.online,
                kind: Kind::Unknown,
            })
        })
        .collect()
}

/// The pool as the hub of this OS user sees it; empty without a hub or a broker.
pub async fn pool(layout: &Layout) -> Vec<PoolRunner> {
    let Ok(client) = super::jobs::hub(layout).await else {
        return Vec::new();
    };
    let fetched = fetch_runners(&client, None).await;
    client.close().await;
    match fetched {
        Ok((per, _)) => rows(
            per.into_iter().flat_map(|(_, runners)| runners),
            &layout.runner_names(),
        ),
        Err(_) => Vec::new(),
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

/// The report's lines after the replacement: the runners that are not on the new version and
/// what to do about each.
pub fn report_lines(pool: &[PoolRunner], new: &Version, protocol_changed: bool) -> Vec<String> {
    let mut out = Vec::new();
    for r in pool {
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
    out
}
