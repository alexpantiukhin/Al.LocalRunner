use crate::agent::Shared;
use lr_abstractions::{Level, Liveness};
use lr_proto::{AssignSpec, WireCode};
use std::path::PathBuf;

const ENTRY: &str = ".mb";

struct Others {
    mb: u64,
    unreadable: Vec<String>,
}

impl Shared {
    fn own_entry(&self) -> String {
        format!("{}{ENTRY}", self.deps.settings.name)
    }

    fn entry_text(&self, mb: u64) -> String {
        match &self.deps.settings.process {
            Some(p) => match p.start {
                Some(start) => format!("{mb} {} {start}", p.pid),
                None => format!("{mb} {}", p.pid),
            },
            None => mb.to_string(),
        }
    }

    fn holder_gone(&self, pid: Option<&str>, start: Option<&str>) -> bool {
        let (Some(pid), Some(start)) = (pid, start) else {
            return false;
        };
        match (pid.parse::<u32>(), start.parse::<u64>()) {
            (Ok(pid), Ok(start)) => self.deps.inspector.liveness(pid, start) == Liveness::Dead,
            _ => false,
        }
    }

    async fn reserved_by_others(&self) -> Others {
        let mut others = Others {
            mb: 0,
            unreadable: Vec::new(),
        };
        let Some(dir) = &self.deps.settings.disk_ledger else {
            return others;
        };
        let names = match self.deps.files.list_files(dir).await {
            Ok(n) => n,
            Err(e) => {
                others.unreadable.push(dir.display().to_string());
                self.diag.event(
                    Level::Warn,
                    "disk reservations of other runners unreadable",
                    &[("error", &e.to_string())],
                );
                return others;
            }
        };
        let own = self.own_entry();
        for name in names.iter().filter(|n| n.ends_with(ENTRY) && **n != own) {
            let path = dir.join(name);
            let text = match self.deps.files.read(&path).await {
                Ok(Some(bytes)) => String::from_utf8_lossy(&bytes).into_owned(),
                Ok(None) => continue,
                Err(_) => String::new(),
            };
            let mut parts = text.split_whitespace();
            let Some(Ok(mb)) = parts.next().map(str::parse::<u64>) else {
                others.unreadable.push(name.clone());
                continue;
            };
            if self.holder_gone(parts.next(), parts.next()) {
                self.diag.event(
                    Level::Info,
                    "disk reservation of a stopped runner dropped",
                    &[("entry", name)],
                );
                let _ = self.deps.files.remove_file(&path).await;
                continue;
            }
            others.mb = others.mb.saturating_add(mb);
        }
        if !others.unreadable.is_empty() {
            self.diag.event(
                Level::Warn,
                "unreadable disk reservation counts as taking all free disk",
                &[("entries", &others.unreadable.join(", "))],
            );
        }
        others
    }

    /// Free space for jobs on the workspace volume: measured now (or found at start when it
    /// cannot be measured), less what other runners have reserved; none while a reservation
    /// cannot be read.
    pub(crate) async fn free_disk_mb(&self) -> u64 {
        let s = &self.deps.settings;
        let free = self.deps.disk.free_mb(&s.state_dir).unwrap_or(s.disk_mb);
        let others = self.reserved_by_others().await;
        if others.unreadable.is_empty() {
            free.saturating_sub(others.mb)
        } else {
            0
        }
    }

    /// Reserves `disk_mb` of the job and then checks it against the free space of its volume
    /// (the work tree's for a work tree, the workspace's otherwise) less the reservations of
    /// other runners; when it does not fit the reservation is withdrawn. Writing before
    /// counting means two runners deciding at once may both refuse but never both take the
    /// same space. The reservation lasts until [`Shared::release_disk`].
    pub(crate) async fn reserve_disk(&self, spec: &AssignSpec) -> Result<(), String> {
        let declared = spec.requirements.disk_mb;
        if declared == 0 {
            return Ok(());
        }
        let need = match crate::limits::disk_headroom_pct(spec.limits.as_ref(), &self.deps.settings)
        {
            Some(pct) => declared.saturating_mul(u64::from(pct)).div_ceil(100),
            None => declared,
        };
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
        if let Some(dir) = &self.deps.settings.disk_ledger {
            let entry = dir.join(self.own_entry());
            self.deps
                .files
                .write(&entry, self.entry_text(need).as_bytes())
                .await
                .map_err(|e| format!("disk reservation not written: {e}"))?;
        }
        let others = self.reserved_by_others().await;
        let refusal = if !others.unreadable.is_empty() {
            Some(format!(
                "not enough disk: the job needs {need} MB, but the disk reservation {} is \
                 unreadable, so no free disk is assumed",
                others.unreadable.join(", ")
            ))
        } else {
            let available = free.saturating_sub(others.mb);
            (available < need).then(|| {
                format!(
                    "not enough disk: the job needs {need} MB, {available} MB are free on {} \
                     after the cache cleanup ({} MB reserved by other runners)",
                    volume.display(),
                    others.mb
                )
            })
        };
        match refusal {
            Some(text) => {
                self.release_disk().await;
                Err(text)
            }
            None => Ok(()),
        }
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
