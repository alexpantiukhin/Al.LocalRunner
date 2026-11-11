use super::conn::Command;
use super::{Engine, Role};
use crate::policy::{Applied, PolicyError};
use lr_abstractions::Level;
use lr_proto::{BrokerToRunner, ReasonCode};
use tokio::sync::{mpsc, oneshot};

pub(crate) enum AdminCommand {
    Reload {
        text: String,
        reply: oneshot::Sender<Result<Applied, PolicyError>>,
    },
    Revoked {
        name: String,
        reply: oneshot::Sender<()>,
    },
}

/// Administrative access to a running broker for the assembly's local admin channel
/// (spec 3.9: `policy reload`, `runner remove`, `client remove` exist on local IPC only).
#[derive(Clone)]
pub struct BrokerHandle(pub(crate) mpsc::Sender<Command>);

impl BrokerHandle {
    /// `policy reload`: checks and applies `text`; an invalid file keeps the running policy.
    /// When `[grace]` changed, every connected runner gets `settings_update`.
    pub async fn reload_policy(&self, text: &str) -> Result<Applied, PolicyError> {
        let (reply, rx) = oneshot::channel();
        let cmd = Command::Admin(AdminCommand::Reload {
            text: text.to_string(),
            reply,
        });
        let stopped = || PolicyError::at("broker", None, None, "the broker is not running".into());
        self.0.send(cmd).await.map_err(|_| stopped())?;
        rx.await.map_err(|_| stopped())?
    }

    /// The key of machine `name` was revoked: its live connections are closed at once, and a
    /// job running on one of its runners is `lost` with `runner_removed` right away, its
    /// resources back in the pool (spec 3.9). Waiting orders stay queued for other runners.
    pub async fn revoked(&self, name: &str) {
        let (reply, rx) = oneshot::channel();
        let cmd = Command::Admin(AdminCommand::Revoked {
            name: name.to_string(),
            reply,
        });
        if self.0.send(cmd).await.is_ok() {
            let _ = rx.await;
        }
    }
}

impl Engine {
    pub(crate) async fn admin(&mut self, cmd: AdminCommand) {
        match cmd {
            AdminCommand::Reload { text, reply } => {
                let result = self.policy.reload(&text);
                self.count_reload(result.is_ok());
                if let Ok(applied) = &result {
                    if let Some(update) = &applied.settings_update {
                        let conns: Vec<_> = self.runners.values().filter_map(|r| r.conn).collect();
                        for conn in conns {
                            self.send(conn, &BrokerToRunner::SettingsUpdate(update.clone()));
                        }
                    }
                    self.diag(Level::Info, "policy reloaded", &[]);
                }
                let _ = reply.send(result);
                self.plan().await;
            }
            AdminCommand::Revoked { name, reply } => {
                let conns: Vec<_> = self
                    .conns
                    .iter()
                    .filter(|(_, c)| match &c.role {
                        Role::Hub(client) => client.machine.as_deref() == Some(name.as_str()),
                        Role::Runner(runner) => self
                            .runners
                            .get(runner)
                            .is_some_and(|r| r.persisted && r.owner == name),
                        Role::Pending => false,
                    })
                    .map(|(id, _)| *id)
                    .collect();
                for conn in conns {
                    self.close(conn);
                }
                let removed: Vec<String> = self
                    .runners
                    .iter()
                    .filter(|(_, r)| r.persisted && r.owner == name)
                    .map(|(n, _)| n.clone())
                    .collect();
                let now = self.now();
                let mut i = 0;
                while i < self.active.len() {
                    let on_removed = self.active[i]
                        .runner
                        .as_ref()
                        .is_some_and(|r| removed.contains(r));
                    if on_removed {
                        let job = self.active.remove(i);
                        self.lose(job, now, ReasonCode::RunnerRemoved).await;
                    } else {
                        i += 1;
                    }
                }
                for r in &removed {
                    self.runners.remove(r);
                    self.forget_runner(r);
                }
                let _ = reply.send(());
                self.plan().await;
            }
        }
    }
}
