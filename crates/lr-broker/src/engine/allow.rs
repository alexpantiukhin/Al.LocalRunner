use super::Engine;
use crate::policy::CommandMode;
use lr_core::QueueEntry;
use lr_proto::{OrderSpec, RejectReason};
use std::collections::BTreeSet;

fn shell_word(value: &str) -> String {
    let plain = |c: char| c.is_ascii_alphanumeric() || "._:/@-".contains(c);
    if !value.is_empty() && value.chars().all(plain) {
        value.to_string()
    } else {
        format!("'{}'", value.replace('\'', "''"))
    }
}

const RESTARTED: &str = "runners have not come back after the broker restart, retry";

impl Engine {
    pub(crate) fn allowed(
        &self,
        entry: &QueueEntry,
        spec: &OrderSpec,
    ) -> Result<(), (RejectReason, String)> {
        let file = self.policy.file();
        let label = &entry.client.label;
        let req = &entry.order.requirements;
        let secrets = file.secrets_allowed(label);
        if let Some(name) = req.secrets.iter().find(|n| !secrets.contains(n)) {
            return Err((
                RejectReason::SecretNotAllowed,
                self.not_allowed("secret", name, label, &self.secret_hint(label)),
            ));
        }
        if let Some(name) = req
            .services
            .iter()
            .find(|n| !file.service_allowed(label, n))
        {
            return Err((
                RejectReason::ServiceNotAllowed,
                self.not_allowed("service", name, label, ""),
            ));
        }
        if file.command_mode(&entry.client.key, label) == CommandMode::Named {
            self.named_only(label, spec)?;
        }
        Ok(())
    }

    fn secret_hint(&self, label: &str) -> String {
        let entries = &self.policy.file().secrets.allow;
        if entries.is_empty() || entries.iter().any(|e| e.label == label) {
            return String::new();
        }
        let mut labels: Vec<&str> = Vec::new();
        for e in entries {
            let shown = if e.label.is_empty() { "(empty)" } else { e.label.as_str() };
            if !labels.contains(&shown) {
                labels.push(shown);
            }
        }
        let mut hint = format!(
            " (no [[secrets.allow]] entry has this label; labels with entries: {}",
            labels.join(", ")
        );
        if !label.is_empty() && entries.iter().any(|e| e.label.is_empty()) {
            hint.push_str("; an entry with an empty label matches only clients without a label");
        }
        hint.push(')');
        hint
    }

    fn not_allowed(&self, what: &str, name: &str, label: &str, hint: &str) -> String {
        let shown = if label.is_empty() { "(empty)" } else { label };
        let file = if self.policy_path.is_empty() {
            String::new()
        } else {
            format!(" in {}", self.policy_path)
        };
        let admin = if self.service_policy {
            ", run it as administrator"
        } else {
            ""
        };
        format!(
            "{what} {name} is not allowed for label {shown}{hint}{file}; to allow it{admin}: localrunner policy allow-{what} {} --label {}",
            shell_word(name),
            shell_word(label)
        )
    }

    pub(crate) fn command_names(
        &self,
        key: &str,
        label: &str,
        spec: &OrderSpec,
    ) -> BTreeSet<String> {
        if self.policy.file().command_mode(key, label) == CommandMode::Named {
            spec.all_steps().map(|s| s.command.clone()).collect()
        } else {
            BTreeSet::new()
        }
    }

    fn named_only(&self, label: &str, spec: &OrderSpec) -> Result<(), (RejectReason, String)> {
        let rule = format!("label {label} may use only the names from the runner list");
        if spec.steps.is_empty() {
            return Err((
                RejectReason::CommandNotAllowed,
                format!("an order without steps names no commands: {rule}"),
            ));
        }
        if !spec.zone_steps.is_empty() {
            return Err((
                RejectReason::CommandNotAllowed,
                format!("steps per zone are not available to a client limited to names: {rule}"),
            ));
        }
        let known: BTreeSet<&str> = self
            .runners
            .values()
            .flat_map(|slot| slot.wire.commands.iter().map(String::as_str))
            .collect();
        let Some(step) = spec
            .all_steps()
            .find(|s| !known.contains(s.command.as_str()))
        else {
            return Ok(());
        };
        let restored = self.runners.values().any(|slot| slot.restored);
        let names = match (known.is_empty(), restored) {
            (true, true) => RESTARTED.to_string(),
            (true, false) => "the runners of this broker report no names".to_string(),
            (false, _) => {
                let list: Vec<&str> = known.into_iter().collect();
                let names = format!("runner names: {}", list.join(", "));
                if restored {
                    format!("{names}; {RESTARTED}")
                } else {
                    names
                }
            }
        };
        Err((
            RejectReason::CommandNotAllowed,
            format!(
                "command {} is not among the runner names: {rule} ({names})",
                step.command
            ),
        ))
    }
}
