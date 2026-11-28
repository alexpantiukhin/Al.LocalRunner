//! `down [names] [--drain | --cancel]` (spec 3.9, 11.1): runners through their local control
//! channel, then the hub through its CLI channel, then the broker through its admin channel.
//! Without names every role of this installation that runs stops; runners told to drain finish
//! their job before the broker goes, so the result still reaches it. Ctrl+C ends only that
//! wait: the draining runners and the broker keep running. With `--cancel` the wait is at
//! most 30 s, also before a broker run by the OS service manager, which answers that it stays.

use super::confirm::{proceed, Console, Stdio};
use crate::ipc_client::IpcClient;
use crate::os_service::state::{ask_manager, restart_command, service_running, status_command};
use crate::os_service::{service_installed, Os};
use crate::paths::Layout;
use async_trait::async_trait;
use lr_proto::{
    AdminReply, AdminRequest, CliReply, CliRequest, ControlReply, DownMode, DrainReason,
};
use lr_proto::{IpcChannel, RunnerControl};
use std::future::Future;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

const POLL: Duration = Duration::from_millis(200);
const ROLE_GONE_WITHIN: Duration = Duration::from_secs(15);
const STOPPED_WITHIN: Duration = Duration::from_secs(3);
const CANCELLED_EXIT_WITHIN: Duration = Duration::from_secs(30);

/// What `down` stops.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A runner of this installation.
    Runner(String),
    /// The hub of this OS user.
    Hub,
    /// The broker of this installation.
    Broker,
}

/// The targets of `down names`, runners first; without names every runner of the
/// installation, the hub and the broker. The word `runner` stands for every runner.
pub fn down_targets(layout: &Layout, names: &[String]) -> Vec<Target> {
    let mut out: Vec<Target> = if names.is_empty() {
        let mut all: Vec<Target> = layout
            .runner_names()
            .into_iter()
            .map(Target::Runner)
            .collect();
        all.extend([Target::Hub, Target::Broker]);
        all
    } else {
        names
            .iter()
            .flat_map(|n| match n.as_str() {
                "hub" => vec![Target::Hub],
                "broker" => vec![Target::Broker],
                "runner" => layout
                    .runner_names()
                    .into_iter()
                    .map(Target::Runner)
                    .collect(),
                _ => vec![Target::Runner(n.clone())],
            })
            .collect()
    };
    out.sort_by_key(|t| match t {
        Target::Runner(_) => 0,
        Target::Hub => 1,
        Target::Broker => 2,
    });
    out.dedup();
    out
}

/// What a role that did not open its channel means for `down`: `Ok(false)` when it does not
/// run, the report of a role that runs on another major protocol version (it cannot be told
/// to stop from here) as an error.
pub fn not_reached(error: &str) -> Result<bool, String> {
    match crate::ipc_client::protocol_refusal(error) {
        true => Err(format!(
            "it is running and answers on the old protocol, so `down` cannot stop it: {error}"
        )),
        false => Ok(false),
    }
}

async fn answers(address: &str, channel: IpcChannel) -> bool {
    match IpcClient::connect(address, channel).await {
        Err(_) => false,
        Ok(c) => {
            c.close().await;
            true
        }
    }
}

async fn until_gone(address: &str, channel: IpcChannel, within: Duration) -> bool {
    let start = tokio::time::Instant::now();
    while answers(address, channel).await {
        if start.elapsed() >= within {
            return false;
        }
        tokio::time::sleep(POLL).await;
    }
    true
}

/// Polls `still_running` until it says no; `false` when `interrupt` came first.
pub async fn drained<F, Fut>(mut still_running: F, interrupt: impl Future<Output = ()>) -> bool
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let all = async {
        while still_running().await {
            tokio::time::sleep(POLL).await;
        }
    };
    tokio::select! {
        _ = all => true,
        _ = interrupt => false,
    }
}

async fn runner(
    layout: &Layout,
    principal: &str,
    name: &str,
    mode: DownMode,
    reason: Option<DrainReason>,
) -> Result<bool, String> {
    crate::runner_config::check_runner_name(name)?;
    let address = layout.runner_ipc(principal, name);
    let c = match IpcClient::connect(&address, IpcChannel::RunnerControl).await {
        Ok(c) => c,
        Err(e) => return not_reached(&e),
    };
    match c
        .call::<_, ControlReply>(&RunnerControl::Down { mode, reason })
        .await?
    {
        ControlReply::Ok => Ok(true),
        ControlReply::Failed { message } => Err(message),
        ControlReply::Link { .. }
        | ControlReply::Reloaded { .. }
        | ControlReply::Dropped { .. }
        | ControlReply::Cleaned { .. }
        | ControlReply::Variables { .. } => Err("unexpected answer to down".into()),
    }
}

async fn hub(layout: &Layout, principal: &str) -> Result<bool, String> {
    let address = layout.hub_ipc(principal);
    let c = match IpcClient::connect(&address, IpcChannel::Cli).await {
        Ok(c) => c,
        Err(e) => return not_reached(&e),
    };
    match c.call::<_, CliReply>(&CliRequest::Shutdown).await {
        Ok(CliReply::Stopping) | Err(_) => {}
        Ok(other) => return Err(format!("unexpected answer: {other:?}")),
    }
    c.close().await;
    match until_gone(&address, IpcChannel::Cli, ROLE_GONE_WITHIN).await {
        true => Ok(true),
        false => Err("still answers after it was told to stop".into()),
    }
}

/// Tells the broker of `layout` to stop and waits until neither its IPC nor its admin channel
/// answers. `None`
/// when no broker answers; otherwise the broker's warnings (a broker run as an OS service
/// keeps running and says so). A broker that stops before its answer leaves counts as
/// stopping.
pub async fn stop_broker(layout: &Layout) -> Result<Option<Vec<String>>, String> {
    let address = layout.admin_ipc();
    let c = match IpcClient::connect_broker_admin(&address).await {
        Ok(c) => c,
        Err(e) => return not_reached(&e).map(|_| None),
    };
    let warnings = match c.call::<_, AdminReply>(&AdminRequest::Shutdown).await {
        Ok(AdminReply::Ok { warnings }) => warnings,
        Ok(AdminReply::Failed { message }) => return Err(message),
        Ok(other) => return Err(format!("unexpected answer: {other:?}")),
        Err(_) => Vec::new(),
    };
    c.close().await;
    if warnings.is_empty() {
        let start = tokio::time::Instant::now();
        while super::up_cmd::broker_answers(layout).await {
            if start.elapsed() >= ROLE_GONE_WITHIN {
                return Err("still answers after it was told to stop".into());
            }
            tokio::time::sleep(POLL).await;
        }
    }
    Ok(Some(warnings))
}

/// How the wait for stopping runners ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Waited {
    /// Every runner went.
    Gone,
    /// After `--cancel` a runner still answered 30 s later.
    TimedOut,
    /// The `--wait-timeout` of `upgrade` ran out first.
    WaitTimedOut,
    /// Ctrl+C came first.
    Interrupted,
}

/// Waits until `still_running` says no: with `cancel` at most 30 s, otherwise until
/// `interrupt` or, when `limit` is set, until it has passed.
pub async fn wait_for_runners<F, Fut>(
    still_running: F,
    cancel: bool,
    limit: Option<Duration>,
    interrupt: impl Future<Output = ()>,
) -> Waited
where
    F: FnMut() -> Fut,
    Fut: Future<Output = bool>,
{
    let timed_out = AtomicBool::new(false);
    let wait_timed_out = AtomicBool::new(false);
    let stop = async {
        match (cancel, limit) {
            (true, _) => tokio::select! {
                _ = interrupt => {}
                _ = tokio::time::sleep(CANCELLED_EXIT_WITHIN) => {
                    timed_out.store(true, Ordering::Relaxed);
                }
            },
            (false, Some(limit)) => tokio::select! {
                _ = interrupt => {}
                _ = tokio::time::sleep(limit) => {
                    wait_timed_out.store(true, Ordering::Relaxed);
                }
            },
            (false, None) => interrupt.await,
        }
    };
    match drained(still_running, stop).await {
        true => Waited::Gone,
        false if timed_out.load(Ordering::Relaxed) => Waited::TimedOut,
        false if wait_timed_out.load(Ordering::Relaxed) => Waited::WaitTimedOut,
        false => Waited::Interrupted,
    }
}

async fn ctrl_c() {
    if tokio::signal::ctrl_c().await.is_err() {
        std::future::pending::<()>().await;
    }
}

fn still_running(name: &str, has_job: Option<bool>) -> String {
    match has_job {
        Some(false) => format!(
            "{name}: still running with no job: it is delivering its outbox or stopping its services; it leaves on its own within {} s of the stop, or once its services have stopped (what it could not deliver is sent on its next start)",
            lr_runner::STOP_BOUND.as_secs()
        ),
        _ => format!(
            "{name}: still draining its job, it stops when the job ends (`localrunner status` shows the job)"
        ),
    }
}

/// Waits up to [`STOPPED_WITHIN`] for the runners `names` to stop answering and reports each:
/// `stopped`, or `still running` (it is stopping: it finishes or cancels its job first, so
/// this is not an error). A runner known to run no job is told apart: it is delivering its
/// outbox or stopping its services and leaves on its own.
async fn report_stopped(roles: &dyn Roles, names: &[String]) {
    let start = tokio::time::Instant::now();
    let mut left: Vec<&String> = names.iter().collect();
    loop {
        let mut still = Vec::new();
        for n in left {
            if roles.runner_answers(n).await {
                still.push(n);
            } else {
                roles.say(&format!("{n}: stopped"));
            }
        }
        left = still;
        if left.is_empty() {
            return;
        }
        if start.elapsed() >= STOPPED_WITHIN {
            for n in left {
                let has_job = roles.runner_has_job(n).await;
                roles.say(&still_running(n, has_job));
            }
            return;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// What runners are told: `--cancel` cancels the current job, otherwise (`--drain` or no
/// flag) they finish it.
pub fn down_mode(cancel: bool) -> DownMode {
    if cancel {
        DownMode::Cancel
    } else {
        DownMode::Drain
    }
}

/// How `down` reaches the roles of an installation.
#[async_trait]
pub trait Roles: Send + Sync {
    /// Tells runner `name` to stop in `mode` (a drain for `reason`); `false` when it does not
    /// run.
    async fn stop_runner(
        &self,
        name: &str,
        mode: DownMode,
        reason: Option<DrainReason>,
    ) -> Result<bool, String>;
    /// The running jobs as lines, or `None` when they are not known.
    async fn running_jobs(&self) -> Option<Vec<String>> {
        None
    }
    /// How many orders the hub holds (running and waiting), or `None` when that is not known.
    async fn hub_orders(&self) -> Option<usize> {
        self.running_jobs().await.map(|l| l.len())
    }
    /// The running jobs with their runners and run times, or `None` when they are not known.
    async fn running_job_list(&self) -> Option<Vec<super::upgrade_jobs::RunningJob>> {
        None
    }
    /// Whether runner `name` runs a job, from [`Roles::running_jobs`]; `None` when not known.
    async fn runner_has_job(&self, name: &str) -> Option<bool> {
        let on = format!(" on {name} (");
        let local = format!(" on local:{name} (");
        self.running_jobs()
            .await
            .map(|lines| lines.iter().any(|l| l.contains(&on) || l.contains(&local)))
    }
    /// Prints a line of the report of `down`.
    fn say(&self, line: &str) {
        println!("{line}");
    }
    /// Whether runner `name` still answers on its control channel.
    async fn runner_answers(&self, name: &str) -> bool;
    /// Stops the hub; `false` when it does not run.
    async fn stop_hub(&self) -> Result<bool, String>;
    /// Asks the dashboard of this user to stop; `false` when none runs.
    async fn stop_dashboard(&self) -> bool {
        false
    }
    /// Stops the broker as [`stop_broker`] does.
    async fn stop_broker(&self) -> Result<Option<Vec<String>>, String>;
    /// Whether the broker runs under the OS service manager.
    fn broker_is_service(&self) -> bool;
    /// Prints an error line of the report of `down`.
    fn say_err(&self, line: &str) {
        eprintln!("{line}");
    }
    /// What is known of the broker service when its admin channel could not be reached.
    async fn broker_service_state(&self) -> ServiceState {
        ServiceState::NotInstalled
    }
    /// Every runner the brokers list now, this installation's included; empty when that is not
    /// known (the hub may still be starting).
    async fn pool_after_restart(&self) -> Vec<super::upgrade_pool::PoolRunner> {
        Vec::new()
    }
    /// The OS service of runner `name` when one is installed: whether it runs (`None`: the
    /// service manager did not say); `None` without a service.
    async fn runner_service(&self, _name: &str) -> Option<Option<bool>> {
        None
    }
}

/// The broker service as seen without administrator rights.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ServiceState {
    /// It runs: it answers on the normal IPC or the service manager says so.
    Answers,
    /// It is installed and does not run.
    InstalledNotAnswering,
    /// It is installed and neither its IPC nor the service manager told whether it runs.
    InstalledStateUnknown,
    /// No service is installed.
    NotInstalled,
}

/// The state of the broker service from what could be learned without administrator rights:
/// whether its normal IPC answered, what the service manager said (`None`: it could not be
/// asked) and whether the service is installed.
pub fn service_state(ipc_answers: bool, manager: Option<bool>, installed: bool) -> ServiceState {
    match (ipc_answers, manager, installed) {
        (true, _, _) | (_, Some(true), _) => ServiceState::Answers,
        (_, _, false) => ServiceState::NotInstalled,
        (_, Some(false), true) => ServiceState::InstalledNotAnswering,
        (_, None, true) => ServiceState::InstalledStateUnknown,
    }
}

/// What `down broker` says of a broker it could not reach on the admin channel; `None` when
/// nothing is installed and the plain "not running" applies.
pub fn unreachable_broker(state: ServiceState, os: Os) -> Option<String> {
    match state {
        ServiceState::Answers => Some(format!(
            "broker: running as an OS service; its admin channel is open only to an administrator, so down cannot stop it from here. Restart it from an elevated shell: {}",
            restart_command(os)
        )),
        ServiceState::InstalledNotAnswering => Some(
            "broker: installed as an OS service and not running; `down` has nothing to stop"
                .into(),
        ),
        ServiceState::InstalledStateUnknown => Some(format!(
            "broker: installed as an OS service; down cannot tell whether it runs (the service manager did not answer an unprivileged query) and its admin channel is open only to an administrator. Check it from an elevated shell: {}",
            status_command(os)
        )),
        ServiceState::NotInstalled => None,
    }
}

/// The roles of `layout` of OS principal `principal`, over their local channels.
pub struct IpcRoles<'a> {
    /// The installation.
    pub layout: &'a Layout,
    /// The OS principal the runner and hub channels are named after.
    pub principal: String,
}

#[async_trait]
impl Roles for IpcRoles<'_> {
    async fn stop_runner(
        &self,
        name: &str,
        mode: DownMode,
        reason: Option<DrainReason>,
    ) -> Result<bool, String> {
        runner(self.layout, &self.principal, name, mode, reason).await
    }

    async fn running_jobs(&self) -> Option<Vec<String>> {
        let seen = super::upgrade_jobs::activity(self.layout).await;
        seen.note.is_none().then_some(seen.running)
    }

    async fn running_job_list(&self) -> Option<Vec<super::upgrade_jobs::RunningJob>> {
        let seen = super::upgrade_jobs::activity(self.layout).await;
        seen.note.is_none().then_some(seen.running_jobs)
    }

    async fn hub_orders(&self) -> Option<usize> {
        let seen = super::upgrade_jobs::activity(self.layout).await;
        seen.note
            .is_none()
            .then_some(seen.running.len() + seen.queued.len())
    }

    async fn runner_answers(&self, name: &str) -> bool {
        let address = self.layout.runner_ipc(&self.principal, name);
        answers(&address, IpcChannel::RunnerControl).await
    }

    async fn stop_hub(&self) -> Result<bool, String> {
        hub(self.layout, &self.principal).await
    }

    async fn stop_dashboard(&self) -> bool {
        crate::dashboard_channel::stop_running(self.layout).await
    }

    async fn stop_broker(&self) -> Result<Option<Vec<String>>, String> {
        stop_broker(self.layout).await
    }

    fn broker_is_service(&self) -> bool {
        self.layout.broker_is_service()
    }

    async fn pool_after_restart(&self) -> Vec<super::upgrade_pool::PoolRunner> {
        super::upgrade_pool::all_runners(self.layout).await
    }

    async fn runner_service(&self, name: &str) -> Option<Option<bool>> {
        let os = Os::current();
        let name = name.to_string();
        ask_manager(Some(None), move || {
            crate::os_service::runner_service::installed(os, &name)
                .then(|| crate::os_service::runner_service::running(os, &name))
        })
        .await
    }

    async fn broker_service_state(&self) -> ServiceState {
        if !self.layout.broker_is_service() {
            return ServiceState::NotInstalled;
        }
        let os = Os::current();
        let ipc = super::admin_cmd::service_running(self.layout).await;
        let (running, installed) = ask_manager((None, true), move || {
            (service_running(os), service_installed(os))
        })
        .await;
        service_state(ipc, running, installed)
    }
}

/// `down [names] [--drain | --cancel]`; the exit code is 1 when a named role does not run, a
/// role refused to stop or the wait for draining runners was interrupted.
pub async fn down(layout: &Layout, names: Vec<String>, cancel: bool) -> Result<i32, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    down_with(layout, names, cancel, &roles, ctrl_c()).await
}

/// `down` as the command line runs it: `--cancel` asks first (see [`down_asked`]).
pub async fn down_cli(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    yes: bool,
    wait: Option<Option<Duration>>,
) -> Result<i32, String> {
    let wait = wait.map(exit_limit);
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    let mut stdio = Stdio::new();
    let mut console = stdio.console();
    if !cancel_confirmed(cancel, yes, &mut console)? {
        return Ok(1);
    }
    let hub_named = names.is_empty() || normalized(names.clone()).iter().any(|n| n == "hub");
    if let (Some(within), false, true) = (wait, cancel, hub_named) {
        match limited(within) {
            true => eprintln!(
                "waiting for the hub to finish its orders (up to {} s)",
                within.as_secs()
            ),
            false => eprintln!("waiting for the hub to finish its orders"),
        }
        let left = wait_for_hub_orders(&roles, within).await;
        if left > 0 {
            eprintln!(
                "{left} order(s) still on the hub after {} s; nothing was stopped (raise --wait-timeout or use --cancel)",
                within.as_secs()
            );
            return Ok(1);
        }
    }
    let code = down_with(layout, names.clone(), cancel, &roles, ctrl_c()).await?;
    match wait {
        Some(within) => Ok(wait_for_exit(layout, names, code, within, &roles).await),
        None => Ok(code),
    }
}

/// [`down_with`] that, with `cancel`, first asks (`yes` answers it) because running jobs are
/// killed; a refusal or a "no" ends with code 1 before any role is told to stop. Without
/// `cancel` it never asks.
pub async fn down_asked(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    yes: bool,
    roles: &dyn Roles,
    interrupt: impl Future<Output = ()>,
    console: &mut Console<'_>,
) -> Result<i32, String> {
    if !cancel_confirmed(cancel, yes, console)? {
        return Ok(1);
    }
    down_with(layout, names, cancel, roles, interrupt).await
}

/// Whether `down` may go on: always without `cancel`; with it the user (or `yes`) confirms
/// that the running jobs are killed.
fn cancel_confirmed(cancel: bool, yes: bool, console: &mut Console<'_>) -> Result<bool, String> {
    if !cancel {
        return Ok(true);
    }
    proceed(
        "down --cancel kills the running jobs",
        "cancel the running jobs and stop the roles? The jobs are lost.",
        yes,
        console,
    )
}

/// `down` of the roles `roles` reaches; `interrupt` (Ctrl+C) ends the wait for the runners.
/// Before the broker stops, the runners told to stop are waited for: when they drain (so their
/// results still reach the broker) and after `--cancel` at most 30 s, also when the broker is
/// an OS service that stays (spec 10.5). A runner still answering ends `down` with code 1 and
/// leaves the broker running.
pub async fn down_with(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    roles: &dyn Roles,
    interrupt: impl Future<Output = ()>,
) -> Result<i32, String> {
    down_for(layout, names, cancel, None, roles, interrupt).await
}

/// The names as `down` takes them: a runner printed as `local:<name>` by `runners` is `<name>`.
fn normalized(names: Vec<String>) -> Vec<String> {
    names
        .into_iter()
        .map(|n| match n.strip_prefix("local:") {
            Some(rest) if !rest.is_empty() => rest.to_string(),
            _ => n,
        })
        .collect()
}

/// How long `down --wait` waits for the stopped runners to exit when `--wait-timeout` is not given.
pub const DEFAULT_EXIT_WAIT: Duration = Duration::from_secs(60);

/// How long `down --wait` waits: the `--wait-timeout` when one is given, otherwise without a
/// limit, because a runner drains for as long as its job runs.
pub fn exit_limit(wait_timeout: Option<Duration>) -> Duration {
    wait_timeout.unwrap_or(Duration::from_secs(u64::MAX / 4))
}

fn limited(within: Duration) -> bool {
    within < exit_limit(None)
}

/// `down hub --drain --wait`: polls until the hub holds no order, at most `within`; the number
/// of orders left (0 when none, or when they are not known).
pub async fn wait_for_hub_orders(roles: &dyn Roles, within: Duration) -> usize {
    let start = tokio::time::Instant::now();
    loop {
        let left = roles.hub_orders().await.unwrap_or(0);
        if left == 0 || start.elapsed() >= within {
            return left;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// Polls until none of the runners `names` answers on its control channel any more; the runners
/// that still answered when `within` had passed (empty when all have exited).
pub async fn runners_still_answering(
    roles: &dyn Roles,
    names: &[String],
    within: Duration,
) -> Vec<String> {
    let start = tokio::time::Instant::now();
    loop {
        let mut left = Vec::new();
        for n in names {
            if roles.runner_answers(n).await {
                left.push(n.clone());
            }
        }
        if left.is_empty() || start.elapsed() >= within {
            return left;
        }
        tokio::time::sleep(POLL).await;
    }
}

/// `down --wait`: after [`down_asked`], waits up to `within` until the runners it stopped have
/// exited and names the ones that did not (exit code 1). The hub and the broker are already
/// waited for by `down` itself.
pub async fn wait_for_exit(
    layout: &Layout,
    names: Vec<String>,
    code: i32,
    within: Duration,
    roles: &dyn Roles,
) -> i32 {
    let runners: Vec<String> = down_targets(layout, &normalized(names))
        .into_iter()
        .filter_map(|t| match t {
            Target::Runner(n) => Some(n),
            _ => None,
        })
        .collect();
    let left = runners_still_answering(roles, &runners, within).await;
    if left.is_empty() {
        return code;
    }
    roles.say_err(&format!(
        "did not exit within {} s: {}",
        within.as_secs(),
        left.join(", ")
    ));
    1
}

/// [`down_with`] whose drain tells the runners why (`upgrade` says [`DrainReason::Upgrade`]).
pub async fn down_for(
    layout: &Layout,
    names: Vec<String>,
    cancel: bool,
    reason: Option<DrainReason>,
    roles: &dyn Roles,
    interrupt: impl Future<Output = ()>,
) -> Result<i32, String> {
    let names = normalized(names);
    let explicit = !names.is_empty();
    let known = layout.runner_names();
    let unknown: Vec<&str> = names
        .iter()
        .map(String::as_str)
        .filter(|n| !matches!(*n, "hub" | "broker" | "runner") && !known.contains(&n.to_string()))
        .collect();
    if !unknown.is_empty() {
        let available = if known.is_empty() {
            "none".to_string()
        } else {
            known.join(", ")
        };
        let hint = if unknown.iter().any(|n| n.contains(':')) {
            "; `down` stops the runners of this machine by their own name (`runners` prints `local:<name>`, the prefix is the broker's)"
        } else {
            ""
        };
        return Err(format!(
            "no runner with this name: {}; available: {available}{hint}",
            unknown.join(", ")
        ));
    }
    let targets = down_targets(layout, &names);
    if targets.is_empty() {
        return Err("nothing to stop".into());
    }
    stop_targets(targets, explicit, cancel, reason, roles, interrupt).await
}

/// Stops exactly `target`, typed, so a runner named `runner`, `hub` or `broker` is that runner
/// and not a group, and waits up to `within` until a stopped runner has exited. The exit code
/// is 1 when the role does not run, refused to stop or did not exit in time.
pub async fn stop_one(layout: &Layout, target: Target, within: Duration) -> Result<i32, String> {
    let principal = lr_net::current_os_principal().map_err(|e| e.to_string())?;
    let roles = IpcRoles { layout, principal };
    stop_one_with(target, within, &roles).await
}

/// [`stop_one`] through `roles`.
pub async fn stop_one_with(
    target: Target,
    within: Duration,
    roles: &dyn Roles,
) -> Result<i32, String> {
    let code = stop_targets(vec![target.clone()], true, false, None, roles, ctrl_c()).await?;
    if let (0, Target::Runner(name)) = (code, &target) {
        let left = runners_still_answering(roles, std::slice::from_ref(name), within).await;
        if !left.is_empty() {
            roles.say_err(&format!(
                "did not exit within {} s: {name}",
                within.as_secs()
            ));
            return Ok(1);
        }
    }
    Ok(code)
}

/// Stops `targets` as `down` does; `explicit` makes a role that does not run an error.
pub async fn stop_targets(
    targets: Vec<Target>,
    explicit: bool,
    cancel: bool,
    reason: Option<DrainReason>,
    roles: &dyn Roles,
    interrupt: impl Future<Output = ()>,
) -> Result<i32, String> {
    let mode = down_mode(cancel);
    let service = targets.contains(&Target::Broker) && roles.broker_is_service();
    let mut interrupt = Some(interrupt);
    let mut code = 0;
    let report = |line: &str, code: &mut i32| {
        if explicit {
            roles.say_err(line);
            *code = 1;
        } else {
            roles.say(line);
        }
    };
    let not_running = |what: &str, code: &mut i32| report(&format!("{what}: not running"), code);
    let mut stopping = Vec::new();
    let mut waited_for_runners = false;
    for t in &targets {
        match t {
            Target::Runner(name) => match roles.stop_runner(name, mode, reason).await {
                Ok(true) => {
                    println!("{name}: stopping ({mode:?})");
                    stopping.push(name.clone());
                }
                Ok(false) => match roles.runner_service(name).await {
                    Some(running) => {
                        let line = crate::os_service::runner_service::unreachable_line(
                            Os::current(),
                            name,
                            running,
                            "down",
                        );
                        if running == Some(false) {
                            report(&line, &mut code);
                        } else {
                            roles.say_err(&line);
                            code = 1;
                        }
                    }
                    None => not_running(name, &mut code),
                },
                Err(e) => {
                    eprintln!("{name}: {e}");
                    code = 1;
                }
            },
            Target::Hub => {
                match roles.stop_hub().await {
                    Ok(true) => println!("hub: stopped"),
                    Ok(false) => not_running("hub", &mut code),
                    Err(e) => {
                        eprintln!("hub: {e}");
                        code = 1;
                    }
                }
                if roles.stop_dashboard().await {
                    println!("dashboard: stopped");
                }
            }
            Target::Broker => {
                if !stopping.is_empty() && (cancel || !service) {
                    eprintln!(
                        "waiting for {} to {} before the broker stops (Ctrl+C stops waiting)",
                        stopping.join(", "),
                        if cancel { "exit" } else { "finish" }
                    );
                    waited_for_runners = true;
                    let names = &stopping;
                    let any_answers = move || async move {
                        for n in names {
                            if roles.runner_answers(n).await {
                                return true;
                            }
                        }
                        false
                    };
                    let waited = match interrupt.take() {
                        Some(i) => wait_for_runners(any_answers, cancel, None, i).await,
                        None => {
                            let never = std::future::pending::<()>();
                            wait_for_runners(any_answers, cancel, None, never).await
                        }
                    };
                    if waited != Waited::Gone {
                        let why = match waited {
                            Waited::TimedOut => format!(
                                "{} did not exit within {} s after the cancel",
                                stopping.join(", "),
                                CANCELLED_EXIT_WITHIN.as_secs()
                            ),
                            _ => {
                                format!("stopped waiting: {} keep running", stopping.join(", "))
                            }
                        };
                        eprintln!(
                            "{why}; the broker keeps running: run `localrunner down broker` later"
                        );
                        return Ok(1);
                    }
                }
                match roles.stop_broker().await {
                    Ok(Some(warnings)) if warnings.is_empty() => println!("broker: stopped"),
                    Ok(Some(warnings)) => {
                        for w in warnings {
                            println!("broker: {w}");
                        }
                    }
                    Ok(None) => {
                        let state = roles.broker_service_state().await;
                        match unreachable_broker(state, Os::current()) {
                            Some(line) => report(&line, &mut code),
                            None => not_running("broker", &mut code),
                        }
                    }
                    Err(e) => {
                        eprintln!("broker: {e}");
                        code = 1;
                    }
                }
            }
        }
    }
    if !waited_for_runners && !stopping.is_empty() {
        report_stopped(roles, &stopping).await;
    }
    Ok(code)
}
