use super::Engine;
use crate::policy::CommandMode;
use lr_core::{QueueEntry, StepSpec};
use lr_proto::{OrderSpec, RejectReason};
use std::collections::BTreeSet;

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
                format!("секрет {name} не разрешён ярлыку {label}"),
            ));
        }
        if let Some(name) = req
            .services
            .iter()
            .find(|n| !file.service_allowed(label, n))
        {
            return Err((
                RejectReason::ServiceNotAllowed,
                format!("сервис {name} не разрешён ярлыку {label}"),
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
        steps: &[StepSpec],
    ) -> BTreeSet<String> {
        if self.policy.file().command_mode(key, label) == CommandMode::Named {
            steps.iter().map(|s| s.command.clone()).collect()
        } else {
            BTreeSet::new()
        }
    }

    fn named_only(&self, label: &str, spec: &OrderSpec) -> Result<(), (RejectReason, String)> {
        let rule = format!("ярлыку {label} разрешены только названия из списка раннера");
        if spec.steps.is_empty() {
            return Err((
                RejectReason::CommandNotAllowed,
                format!("заказ без шагов не называет команд: {rule}"),
            ));
        }
        let known: BTreeSet<&str> = self
            .runners
            .values()
            .flat_map(|slot| slot.wire.commands.iter().map(String::as_str))
            .collect();
        let Some(step) = spec
            .steps
            .iter()
            .find(|s| !known.contains(s.command.as_str()))
        else {
            return Ok(());
        };
        let names = if known.is_empty() {
            "раннеры этого брокера названий не сообщают".to_string()
        } else {
            let list: Vec<&str> = known.into_iter().collect();
            format!("названия раннеров: {}", list.join(", "))
        };
        Err((
            RejectReason::CommandNotAllowed,
            format!(
                "команда {} не входит в названия раннеров: {rule} ({names})",
                step.command
            ),
        ))
    }
}
