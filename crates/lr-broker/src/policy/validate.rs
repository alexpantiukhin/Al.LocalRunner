use super::error::PolicyWarning;
use super::file::{HostResources, PolicyFile};
use super::rules::{display_name, has_action, RuleEntry};
use super::sections::{AllowEntry, CacheShare, CommandMode, OthersOrders, TimeoutAboveMax};
use ipnet::IpNet;
use lr_core::{Condition, Rule};

/// IPv4 ranges shorter than this prefix are reported as too wide.
pub const WIDE_V4_PREFIX: u8 = 16;

/// IPv6 ranges shorter than this prefix are reported as too wide.
pub const WIDE_V6_PREFIX: u8 = 48;

/// Warnings of `policy check` and `policy reload` (spec 3.5, 11.1): they do not stop the file
/// from applying. `host` fills the parts of `[pool]` the owner leaves out.
pub fn check(file: &PolicyFile, host: &HostResources) -> Vec<PolicyWarning> {
    let mut out = Vec::new();
    rule_warnings(file, host, &mut out);
    network_warnings(file, &mut out);
    allow_warnings("secrets.allow", &file.secrets.allow, &mut out);
    allow_warnings("services.allow", &file.services.allow, &mut out);
    secret_warnings(file, &mut out);
    if file.cache.share == CacheShare::Project {
        out.push(PolicyWarning::at(
            "cache",
            Some("share"),
            None,
            "share = \"project\": one client can plant an artifact into the build of another"
                .into(),
        ));
        let named = file.commands.default == CommandMode::Named
            || file
                .commands
                .client
                .iter()
                .any(|c| c.mode == CommandMode::Named);
        if named {
            out.push(PolicyWarning::at(
                "cache",
                Some("share"),
                None,
                "share = \"project\" with a client limited to [commands] mode = \"named\": the \
                 project is declared by the client, so a limited client can plant an artifact \
                 into the shared cache of trusted builds"
                    .into(),
            ));
        }
    }
    if file.limits.timeout_above_max == TimeoutAboveMax::Trim
        && file.limits.max_job_timeout_s.is_none()
    {
        out.push(PolicyWarning::at(
            "limits",
            Some("timeout_above_max"),
            None,
            "timeout_above_max = \"trim\" without max_job_timeout_s: there is no maximum to trim to"
                .into(),
        ));
    }
    if file.visibility.others_orders == OthersOrders::Full {
        out.push(PolicyWarning::at(
            "visibility",
            Some("others_orders"),
            None,
            "others_orders = \"full\": every client sees projects, branches, directories and \
             commands of the others"
                .into(),
        ));
    }
    out
}

fn implied_by(earlier: &Condition, later: &Condition) -> bool {
    let same = |a: &Option<String>, b: &Option<String>| a.is_none() || a == b;
    same(&earlier.client_label, &later.client_label)
        && same(&earlier.client_key, &later.client_key)
        && same(&earlier.machine_id, &later.machine_id)
        && same(&earlier.source, &later.source)
        && same(&earlier.project, &later.project)
        && same(&earlier.submitter_label, &later.submitter_label)
        && same(&earlier.tag, &later.tag)
        && earlier
            .min_mem_mb
            .is_none_or(|e| later.min_mem_mb.is_some_and(|l| l >= e))
}

type Field = (&'static str, fn(&Rule) -> bool);

const FIELDS: [Field; 4] = [
    ("priority", |r| r.priority.is_some()),
    ("reserve", |r| r.reserve.is_some()),
    ("quota", |r| r.quota.is_some()),
    ("weight", |r| r.weight.is_some()),
];

fn shadowed_by(rules: &[RuleEntry], index: usize) -> Option<Vec<String>> {
    let rule = &rules[index].rule;
    let mut by = Vec::new();
    for (_, sets) in FIELDS.iter().filter(|(_, sets)| sets(rule)) {
        let earlier = rules[..index]
            .iter()
            .enumerate()
            .find(|(_, e)| sets(&e.rule) && implied_by(&e.rule.when, &rule.when))?;
        let name = display_name(&earlier.1.rule, earlier.0);
        if !by.contains(&name) {
            by.push(name);
        }
    }
    Some(by)
}

fn rule_warnings(file: &PolicyFile, host: &HostResources, out: &mut Vec<PolicyWarning>) {
    let pool = file.pool.limits(host);
    let mut reserved_cpu = 0u64;
    let mut reserved_mem = 0u64;
    for (i, entry) in file.rules.iter().enumerate() {
        let rule = &entry.rule;
        let name = display_name(rule, i);
        let warn =
            |key: &str, message: String| PolicyWarning::at("rule", Some(key), entry.line, message);
        if !has_action(rule) {
            out.push(warn(
                "when",
                format!("rule {name} sets no priority, reserve, quota or weight"),
            ));
        } else if let Some(by) = shadowed_by(&file.rules, i) {
            out.push(warn(
                "when",
                format!(
                    "rule {name} is never applied: earlier rule {} decides every field it sets",
                    by.join(", ")
                ),
            ));
        }
        if let Some(m) = rule.when.min_mem_mb.filter(|m| *m > pool.mem_mb) {
            out.push(warn(
                "when",
                format!(
                    "rule {name}: min_mem_mb = {m} is more than the pool's {} MB; no order \
                     that large is ever accepted",
                    pool.mem_mb
                ),
            ));
        }
        if rule.quota.is_some_and(|q| q.max_running == Some(0)) {
            out.push(warn(
                "quota",
                format!("rule {name}: max_running = 0 refuses every matching order"),
            ));
        }
        if let Some(r) = rule.reserve {
            reserved_cpu += u64::from(r.cpu);
            reserved_mem += r.mem_mb;
        }
    }
    let cpu_full = reserved_cpu > 0 && reserved_cpu >= u64::from(pool.cpu);
    let mem_full = reserved_mem > 0 && reserved_mem >= pool.mem_mb;
    if cpu_full || mem_full {
        out.push(PolicyWarning::at(
            "rule",
            Some("reserve"),
            None,
            format!(
                "reserves take {reserved_cpu} of {} CPU and {reserved_mem} of {} MB: orders \
                 outside them never start",
                pool.cpu, pool.mem_mb
            ),
        ));
    }
}

fn too_wide(net: &IpNet) -> bool {
    match net {
        IpNet::V4(n) => n.prefix_len() < WIDE_V4_PREFIX,
        IpNet::V6(n) => n.prefix_len() < WIDE_V6_PREFIX,
    }
}

fn network_warnings(file: &PolicyFile, out: &mut Vec<PolicyWarning>) {
    for (name, ranges) in &file.networks {
        for net in ranges.iter().filter(|n| too_wide(n)) {
            out.push(PolicyWarning::at(
                "networks",
                Some(name),
                None,
                format!(
                    "{net} is a very wide range: every address in it counts as lan for zone \
                     limits and routing"
                ),
            ));
        }
    }
}

fn allow_warnings(table: &str, entries: &[AllowEntry], out: &mut Vec<PolicyWarning>) {
    for (i, entry) in entries.iter().enumerate() {
        if entries[..i].iter().any(|e| e.label == entry.label) {
            out.push(PolicyWarning::at(
                table,
                Some("label"),
                None,
                format!(
                    "entry {} for label `{}` is never used: an earlier entry of that label decides",
                    i + 1,
                    entry.label
                ),
            ));
        }
    }
}

fn secret_warnings(file: &PolicyFile, out: &mut Vec<PolicyWarning>) {
    for entry in &file.secrets.allow {
        if entry.names.is_empty() {
            continue;
        }
        let named_entry = file
            .commands
            .client
            .iter()
            .find(|c| c.key.is_none() && c.label.as_deref() == Some(entry.label.as_str()))
            .map(|c| c.mode);
        if named_entry.unwrap_or(file.commands.default) == CommandMode::Named {
            continue;
        }
        out.push(PolicyWarning::at(
            "secrets.allow",
            Some("label"),
            None,
            format!(
                "label `{}` gets secrets but may run any command: the admission holds only with \
                 [commands] mode = \"named\" or a runner under a separate OS user",
                entry.label
            ),
        ));
    }
}
