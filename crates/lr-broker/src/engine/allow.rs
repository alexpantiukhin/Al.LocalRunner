use super::Engine;
use crate::policy::CommandMode;
use lr_core::QueueEntry;
use lr_proto::{OrderSpec, RejectReason};

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
            if let Some(step) = spec.steps.first() {
                return Err((
                    RejectReason::CommandNotAllowed,
                    format!(
                        "команда {} не входит в названия раннеров: ярлыку {label} разрешены \
                         только названия из списка раннера, а раннеры этого брокера списка \
                         названий не сообщают",
                        step.command
                    ),
                ));
            }
        }
        Ok(())
    }
}
