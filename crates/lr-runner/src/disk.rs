use crate::agent::Shared;
use lr_abstractions::Level;
use lr_proto::{AssignSpec, WireCode};
use std::path::PathBuf;

const ENTRY: &str = ".mb";

impl Shared {
    fn own_entry(&self) -> String {
        format!("{}{ENTRY}", self.deps.settings.name)
    }

    async fn reserved_by_others(&self) -> u64 {
        let Some(dir) = &self.deps.settings.disk_ledger else {
            return 0;
        };
        let names = match self.deps.files.list_files(dir).await {
            Ok(n) => n,
            Err(e) => {
                self.diag.event(
                    Level::Warn,
                    "disk reservations of other runners unreadable",
                    &[("error", &e.to_string())],
                );
                return 0;
            }
        };
        let own = self.own_entry();
        let mut total = 0u64;
        for name in names.iter().filter(|n| n.ends_with(ENTRY) && **n != own) {
            if let Ok(Some(bytes)) = self.deps.files.read(&dir.join(name)).await {
                let mb = String::from_utf8_lossy(&bytes).trim().parse::<u64>();
                total = total.saturating_add(mb.unwrap_or(0));
            }
        }
        total
    }

    /// Free space for jobs on the workspace volume: measured now (or found at start when it
    /// cannot be measured), less what other runners have reserved.
    pub(crate) async fn free_disk_mb(&self) -> u64 {
        let s = &self.deps.settings;
        let free = self.deps.disk.free_mb(&s.state_dir).unwrap_or(s.disk_mb);
        free.saturating_sub(self.reserved_by_others().await)
    }

    /// Checks `disk_mb` of the job against the free space of its volume (the work tree's for a
    /// work tree, the workspace's otherwise) less the reservations of other runners, and
    /// reserves it until [`Shared::release_disk`].
    pub(crate) async fn reserve_disk(&self, spec: &AssignSpec) -> Result<(), String> {
        let need = spec.requirements.disk_mb;
        if need == 0 {
            return Ok(());
        }
        let volume = match &spec.code {
            WireCode::WorkTree { cwd } => PathBuf::from(cwd),
            _ => self.deps.settings.state_dir.clone(),
        };
        let Some(free) = self.deps.disk.free_mb(&volume) else {
            self.diag.event(
                Level::Warn,
                "free disk not measured, disk_mb of the job not checked",
                &[("path", &volume.display().to_string())],
            );
            return Ok(());
        };
        let others = self.reserved_by_others().await;
        let available = free.saturating_sub(others);
        if available < need {
            return Err(format!(
                "not enough disk: the job needs {need} MB, {available} MB are free on {} after \
                 the cache cleanup ({others} MB reserved by other runners)",
                volume.display()
            ));
        }
        if let Some(dir) = &self.deps.settings.disk_ledger {
            let entry = dir.join(self.own_entry());
            self.deps
                .files
                .write(&entry, need.to_string().as_bytes())
                .await
                .map_err(|e| format!("disk reservation not written: {e}"))?;
        }
        Ok(())
    }

    /// Drops the disk reservation of this runner, if any.
    pub(crate) async fn release_disk(&self) {
        let Some(dir) = &self.deps.settings.disk_ledger else {
            return;
        };
        if let Err(e) = self
            .deps
            .files
            .remove_file(&dir.join(self.own_entry()))
            .await
        {
            self.diag.event(
                Level::Error,
                "disk reservation not removed",
                &[("error", &e.to_string())],
            );
        }
    }
}
