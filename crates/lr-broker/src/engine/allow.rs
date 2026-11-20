use super::Engine;
use crate::policy::CommandMode;
use lr_core::QueueEntry;
use lr_proto::{OrderSpec, RejectReason};
use std::collections::BTreeSet;

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
                format!("secret {name} is not allowed for label {label}"),
            ));
        }
        if let Some(name) = req
            .services
            .iter()
            .find(|n| !file.service_allowed(label, n))
        {
            return Err((
                RejectReason::ServiceNotAllowed,
                format!("service {name} is not allowed for label {label}"),
            ));
        }
        if file.command_mode(&entry.client.key, label) == CommandMode::Named {
            self.named_only(label, spec)?;
        }
        Ok(())
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
