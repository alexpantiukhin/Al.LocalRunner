use super::admission::AdmissionSection;
use super::error::{header_line_at, line_of, PolicyError};
use super::listen::{networks, ListenSection};
use super::resources::ResourcesSection;
use super::rules::{build, default_rules, RawRule, RuleEntry};
use super::sections::{
    CacheSection, CancelSection, ClientsSection, CommandMode, CommandsSection, GraceSection,
    HistorySection, JoinSection, LimitsSection, LocalUserEntry, OrphansSection, PoolSection,
    QueueSection, RawLocalUser, ResultsSection, RoutingSection, SaturationSection, SecretsSection,
    ServicesSection, VisibilitySection,
};
use lr_core::{NamedNetworks, PoolLimits};
use lr_proto::SettingsUpdate;
use serde::Deserialize;
use std::ops::Range;
use toml::Spanned;

/// What the broker host has, for the parts of `[pool]` the owner leaves out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct HostResources {
    /// Total memory of the host in MB.
    pub mem_mb: u64,
    /// Number of CPU cores of the host.
    pub cpu: u32,
}

/// A loaded and validated `policy.toml` of the broker (spec 3.5): one field per table.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyFile {
    /// `[queue]`.
    pub queue: QueueSection,
    /// `[[rule]]` in file order; the default `ci-first` rule when the file has none, nothing
    /// for `rule = []`.
    pub rules: Vec<RuleEntry>,
    /// `[clients]`.
    pub clients: ClientsSection,
    /// `[join]`.
    pub join: JoinSection,
    /// `[orphans]`.
    pub orphans: OrphansSection,
    /// `[grace]`.
    pub grace: GraceSection,
    /// `[results]`.
    pub results: ResultsSection,
    /// `[listen]`, applied only at start.
    pub listen: ListenSection,
    /// `[networks]`: name to ranges, applied only at start; default none.
    pub networks: NamedNetworks,
    /// `[[local_user]]` in file order; default none (the installer writes the first entry).
    pub local_users: Vec<LocalUserEntry>,
    /// `[cancel]`.
    pub cancel: CancelSection,
    /// `[secrets]`.
    pub secrets: SecretsSection,
    /// `[pool]`.
    pub pool: PoolSection,
    /// `[limits]`.
    pub limits: LimitsSection,
    /// `[routing]`.
    pub routing: RoutingSection,
    /// `[history]`.
    pub history: HistorySection,
    /// `[saturation]`.
    pub saturation: SaturationSection,
    /// `[cache]`.
    pub cache: CacheSection,
    /// `[services]`.
    pub services: ServicesSection,
    /// `[visibility]`.
    pub visibility: VisibilitySection,
    /// `[commands]`.
    pub commands: CommandsSection,
    /// `[admission]`.
    pub admission: AdmissionSection,
    /// `[resources]`; `None` when the table is absent.
    pub resources: Option<ResourcesSection>,
}

#[derive(Default, Deserialize)]
#[serde(deny_unknown_fields, default)]
struct RawFile {
    queue: QueueSection,
    rule: Option<Vec<Spanned<RawRule>>>,
    clients: ClientsSection,
    join: JoinSection,
    orphans: OrphansSection,
    grace: GraceSection,
    results: ResultsSection,
    listen: ListenSection,
    #[serde(deserialize_with = "networks")]
    networks: NamedNetworks,
    local_user: Vec<Spanned<RawLocalUser>>,
    cancel: CancelSection,
    secrets: SecretsSection,
    pool: PoolSection,
    limits: LimitsSection,
    routing: RoutingSection,
    history: HistorySection,
    saturation: SaturationSection,
    cache: CacheSection,
    services: ServicesSection,
    visibility: VisibilitySection,
    commands: CommandsSection,
    admission: AdmissionSection,
    resources: Option<ResourcesSection>,
}

fn entry_line(text: &str, span: Range<usize>, table: &str) -> Option<usize> {
    let start = span.start.min(text.len());
    match header_line_at(text, start) {
        Some((line, name)) if name == table => Some(line),
        _ => Some(line_of(text, start)),
    }
}

/// Parses and checks `policy.toml`. An empty text gives the defaults. Unknown tables and keys,
/// wrong types and invalid values are refused with the table, key and line.
pub fn load(text: &str) -> Result<PolicyFile, PolicyError> {
    let raw: RawFile = toml::from_str(text).map_err(|e| PolicyError::from_toml(&e, text))?;
    let rules = match raw.rule {
        None => default_rules(),
        Some(rules) => build(
            rules
                .into_iter()
                .map(|s| {
                    let line = entry_line(text, s.span(), "rule");
                    (s.into_inner(), line)
                })
                .collect(),
        )?,
    };
    let mut local_users: Vec<LocalUserEntry> = Vec::with_capacity(raw.local_user.len());
    for spanned in raw.local_user {
        let line = entry_line(text, spanned.span(), "local_user");
        let user = LocalUserEntry::from(spanned.into_inner());
        if local_users.iter().any(|u| u.name == user.name) {
            return Err(PolicyError::at(
                "local_user",
                Some("name"),
                line,
                format!("local user `{}` is listed twice", user.name),
            ));
        }
        local_users.push(user);
    }
    Ok(PolicyFile {
        queue: raw.queue,
        rules,
        clients: raw.clients,
        join: raw.join,
        orphans: raw.orphans,
        grace: raw.grace,
        results: raw.results,
        listen: raw.listen,
        networks: raw.networks,
        local_users,
        cancel: raw.cancel,
        secrets: raw.secrets,
        pool: raw.pool,
        limits: raw.limits,
        routing: raw.routing,
        history: raw.history,
        saturation: raw.saturation,
        cache: raw.cache,
        services: raw.services,
        visibility: raw.visibility,
        commands: raw.commands,
        admission: raw.admission,
        resources: raw.resources,
    })
}

impl Default for PolicyFile {
    fn default() -> Self {
        load("").expect("the empty policy is valid")
    }
}

impl PoolSection {
    /// The pool budget: the owner's numbers, the host's totals where they are left out.
    pub fn limits(&self, host: &HostResources) -> PoolLimits {
        PoolLimits {
            mem_mb: self.mem_mb.map_or(host.mem_mb, |v| v.get()),
            cpu: self.cpu.map_or(host.cpu, |v| v.get()),
            exclusive_tags: self.exclusive_tags.clone(),
        }
    }
}

impl PolicyFile {
    /// Secret names `label` may request: the first `[[secrets.allow]]` entry of the label;
    /// none without an entry.
    pub fn secrets_allowed(&self, label: &str) -> &[String] {
        self.secrets
            .allow
            .iter()
            .find(|e| e.label == label)
            .map_or(&[], |e| e.names.as_slice())
    }

    /// Whether `label` may request service `name`: any service without `[[services.allow]]`,
    /// else only names of the first entry of the label.
    pub fn service_allowed(&self, label: &str, name: &str) -> bool {
        if self.services.allow.is_empty() {
            return true;
        }
        self.services
            .allow
            .iter()
            .find(|e| e.label == label)
            .is_some_and(|e| e.names.iter().any(|n| n == name))
    }

    /// Command mode of a client: the first matching `[[commands.client]]`, else the default.
    pub fn command_mode(&self, key: &str, label: &str) -> CommandMode {
        self.commands
            .client
            .iter()
            .find(|c| c.matches(key, label))
            .map_or(self.commands.default, |c| c.mode)
    }

    /// The `[grace]` settings the broker sends to its runners.
    pub fn settings_update(&self) -> SettingsUpdate {
        SettingsUpdate {
            broker_lost_kill_s: Some(self.grace.lost_min.get().saturating_mul(60)),
        }
    }
}
