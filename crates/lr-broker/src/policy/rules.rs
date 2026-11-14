use super::error::PolicyError;
use super::when::RawWhen;
use lr_core::{Condition, Quota, Reserve, Rule};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::num::NonZeroU32;

/// Name of the default rule used when the file has no `[[rule]]`.
pub const DEFAULT_CI_RULE: &str = "ci-first";

/// Label the default rule raises.
pub const DEFAULT_CI_LABEL: &str = "ci";

/// Priority of the default rule (spec 6: orders of the `ci` client go first).
pub const DEFAULT_CI_PRIORITY: i64 = 100;

/// One loaded `[[rule]]` with the line of its header.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuleEntry {
    /// The rule.
    pub rule: Rule,
    /// 1-based line of the rule in the file; `None` for a default rule.
    pub line: Option<usize>,
}

/// The rules used when the file has no `[[rule]]`: `ci-first`, label `ci`, priority 100.
pub fn default_rules() -> Vec<RuleEntry> {
    vec![RuleEntry {
        rule: Rule {
            name: Some(DEFAULT_CI_RULE.into()),
            when: Condition {
                client_label: Some(DEFAULT_CI_LABEL.into()),
                ..Condition::default()
            },
            priority: Some(DEFAULT_CI_PRIORITY),
            ..Rule::default()
        },
        line: None,
    }]
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawReserve {
    #[serde(default)]
    cpu: u32,
    #[serde(default)]
    mem_mb: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RawQuota {
    max_running: Option<u32>,
    max_mem_mb: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawRule {
    name: Option<String>,
    #[serde(default)]
    when: RawWhen,
    priority: Option<i64>,
    reserve: Option<RawReserve>,
    quota: Option<RawQuota>,
    weight: Option<NonZeroU32>,
}

impl RawRule {
    fn into_rule(self) -> Result<Rule, String> {
        Ok(Rule {
            name: self.name,
            when: self.when.into_condition()?,
            priority: self.priority,
            reserve: self.reserve.map(|r| Reserve {
                cpu: r.cpu,
                mem_mb: r.mem_mb,
            }),
            quota: self.quota.map(|q| Quota {
                max_running: q.max_running,
                max_mem_mb: q.max_mem_mb,
            }),
            weight: self.weight.map(NonZeroU32::get),
        })
    }
}

/// Whether `rule` sets any decision field.
pub fn has_action(rule: &Rule) -> bool {
    rule.priority.is_some()
        || rule.reserve.is_some()
        || rule.quota.is_some()
        || rule.weight.is_some()
}

/// Display name of rule `index` (0-based): its name, else its 1-based number.
pub fn display_name(rule: &Rule, index: usize) -> String {
    rule.name.clone().unwrap_or_else(|| (index + 1).to_string())
}

/// Converts the raw rules in file order and checks each one (spec 3.5): a rule that decides by
/// declared attributes must also restrict the trusted `client_label` positively; names are unique and non-empty.
pub(crate) fn build(raw: Vec<(RawRule, Option<usize>)>) -> Result<Vec<RuleEntry>, PolicyError> {
    let mut seen: BTreeMap<String, usize> = BTreeMap::new();
    let mut out = Vec::with_capacity(raw.len());
    for (index, (raw, line)) in raw.into_iter().enumerate() {
        let shown = raw.name.clone().unwrap_or_else(|| (index + 1).to_string());
        let rule = raw.into_rule().map_err(|m| {
            PolicyError::at("rule", Some("when"), line, format!("rule {shown}: {m}"))
        })?;
        if let Some(name) = &rule.name {
            if name.trim().is_empty() {
                return Err(PolicyError::at(
                    "rule",
                    Some("name"),
                    line,
                    format!("rule {shown}: the name must not be empty; omit it to use the number"),
                ));
            }
            if let Some(first) = seen.insert(name.clone(), index) {
                return Err(PolicyError::at(
                    "rule",
                    Some("name"),
                    line,
                    format!(
                        "rule name `{name}` is used by rules {} and {}",
                        first + 1,
                        index + 1
                    ),
                ));
            }
        }
        if rule.when.uses_declared() && !rule.when.guarded_by_label() && has_action(&rule) {
            return Err(PolicyError::at(
                "rule",
                Some("when"),
                line,
                format!(
                    "rule {shown}: the declared attribute {} may be decided on only together \
                     with a trusted client_label condition",
                    rule.when.declared_names().join(", ")
                ),
            ));
        }
        out.push(RuleEntry { rule, line });
    }
    Ok(out)
}
