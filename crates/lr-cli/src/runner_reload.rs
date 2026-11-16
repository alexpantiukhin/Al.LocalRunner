//! `reload` of a running runner: `runner.toml` is read again and everything that can change
//! while the runner runs is applied; what cannot is named and keeps its old value.

use crate::build::{finish_settings, runner_config_path};
use crate::paths::Layout;
use crate::runner_config::RunnerConfig;
use lr_abstractions::metric::{POLICY_RELOAD_TOTAL, RESULT};
use lr_abstractions::Meter;
use lr_runner::{AgentControl, BrokerAddress};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

/// Names of the settings of `new` that differ from the running `old` and take effect only after
/// a restart: what identifies the runner and its processes, who may run it, its services and the
/// sources of its secrets.
pub fn diff(old: &RunnerConfig, new: &RunnerConfig) -> Vec<&'static str> {
    let mut out = Vec::new();
    if old.settings.name != new.settings.name {
        out.push("name");
    }
    if old.settings.state_dir != new.settings.state_dir {
        out.push("state directory");
    }
    if old.allow_elevated != new.allow_elevated {
        out.push("allow_elevated");
    }
    if old.allow_same_user_as_broker != new.allow_same_user_as_broker {
        out.push("allow_same_user_as_broker");
    }
    if old.settings.services != new.settings.services {
        out.push("services");
    }
    if old.secrets != new.secrets {
        out.push("secrets");
    }
    out
}

/// Lowercase SHA-256 of a settings file's text: what a role reports as the file it applied.
pub fn hash_text(text: &str) -> String {
    use sha2::{Digest, Sha256};
    Sha256::digest(text.as_bytes())
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// Whether the file on disk is no longer the one a role applied.
pub fn disk_differs(text_on_disk: &str, applied_hash: &str) -> bool {
    hash_text(text_on_disk) != applied_hash
}

/// What a runner started with: its parsed `runner.toml`, the text it was parsed from and the
/// broker it connected to.
pub struct Started {
    /// The parsed file.
    pub config: RunnerConfig,
    /// The text of the file (empty without one).
    pub text: String,
    /// Where the broker is.
    pub broker: BrokerAddress,
}

struct Applied {
    hash: String,
    config: RunnerConfig,
    broker: BrokerAddress,
}

/// Rereads the `runner.toml` of one running runner and hands it to the agent.
pub struct RunnerReloader {
    layout: Layout,
    name: String,
    config: Option<PathBuf>,
    applied: Mutex<Applied>,
    control: AgentControl,
    meter: Arc<dyn Meter>,
}

impl RunnerReloader {
    /// A reloader for runner `name` that started with `applied` and `broker`; `config` is the
    /// explicit `--config` of its start.
    pub fn new(
        layout: Layout,
        name: &str,
        config: Option<&Path>,
        started: Started,
        control: AgentControl,
        meter: Arc<dyn Meter>,
    ) -> RunnerReloader {
        RunnerReloader {
            layout,
            name: name.to_string(),
            config: config.map(Path::to_path_buf),
            applied: Mutex::new(Applied {
                hash: hash_text(&started.text),
                config: started.config,
                broker: started.broker,
            }),
            control,
            meter,
        }
    }

    /// [`hash_text`] of the `runner.toml` text the runner applied last.
    pub fn config_hash(&self) -> String {
        let applied = self.applied.lock().unwrap_or_else(|p| p.into_inner());
        applied.hash.clone()
    }

    /// Reads, checks and applies `runner.toml`. The file is parsed and its `[programs]` are
    /// checked again; on any error nothing changes and the error is returned. Otherwise the new
    /// settings apply to the next job and to the capabilities the broker sees, and the names of
    /// the settings that need a restart are returned (their running values stay).
    pub fn reload(&self) -> Result<Vec<String>, String> {
        let result = self.apply();
        let label = if result.is_ok() { "ok" } else { "error" };
        self.meter.add(POLICY_RELOAD_TOTAL, &[(RESULT, label)], 1);
        result
    }

    fn apply(&self) -> Result<Vec<String>, String> {
        let dir = self.layout.runner_dir(&self.name);
        let path = runner_config_path(&self.layout, &self.name, self.config.as_deref());
        let text = match &path {
            Some(p) => std::fs::read_to_string(p).map_err(|e| format!("{}: {e}", p.display()))?,
            None => String::new(),
        };
        let mut new = RunnerConfig::parse(
            &text,
            Some(&self.name),
            &self.name,
            dir.clone(),
            crate::host::resources(),
        )
        .map_err(|e| e.to_string())?;
        let problems = finish_settings(&self.layout, &dir, &mut new);
        if !problems.is_empty() {
            return Err(problems.join("; "));
        }
        let broker = crate::build::broker_address(&dir, &self.layout)?;
        let mut applied = self.applied.lock().unwrap_or_else(|p| p.into_inner());
        let mut restart: Vec<String> = diff(&applied.config, &new)
            .into_iter()
            .map(String::from)
            .collect();
        if applied.broker != broker {
            restart.push("broker address".into());
        }
        let old = &applied.config;
        new.allow_elevated = old.allow_elevated;
        new.allow_same_user_as_broker = old.allow_same_user_as_broker;
        new.secrets = old.secrets.clone();
        new.settings.name = old.settings.name.clone();
        new.settings.state_dir = old.settings.state_dir.clone();
        new.settings.services = old.settings.services.clone();
        self.control.apply_settings(new.settings.clone());
        applied.config = new;
        applied.hash = hash_text(&text);
        Ok(restart)
    }
}
