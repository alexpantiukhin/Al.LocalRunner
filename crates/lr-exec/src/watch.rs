use crate::limits::{warn_line, Meter, Verdict};
use crate::spawn::Tree;
use lr_abstractions::{Exceeded, LimitedResource, ResourceLimits, Usage};
use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

const DISK_EVERY: Duration = Duration::from_secs(2);
const MAX_ENTRIES: u64 = 500_000;
const MAX_DEPTH: usize = 64;
const BYTES_PER_MB: u64 = 1024 * 1024;

pub(crate) fn dir_bytes(dirs: &[PathBuf], max_entries: u64) -> u64 {
    let mut total = 0u64;
    let mut seen = 0u64;
    let mut stack: Vec<(PathBuf, usize)> = dirs.iter().map(|d| (d.clone(), 0)).collect();
    while let Some((dir, depth)) = stack.pop() {
        let Ok(entries) = fs::read_dir(&dir) else {
            continue;
        };
        for entry in entries.flatten() {
            seen += 1;
            if seen > max_entries {
                return total;
            }
            let Ok(meta) = fs::symlink_metadata(entry.path()) else {
                continue;
            };
            if meta.is_dir() {
                if depth < MAX_DEPTH {
                    stack.push((entry.path(), depth + 1));
                }
            } else if meta.is_file() {
                total = total.saturating_add(meta.len());
            }
        }
    }
    total
}

struct DiskProbe {
    dirs: Vec<PathBuf>,
    latest: Arc<AtomicU64>,
    running: Arc<AtomicBool>,
    last: Option<Instant>,
}

impl DiskProbe {
    fn new(dirs: &[PathBuf]) -> DiskProbe {
        DiskProbe {
            dirs: dirs
                .iter()
                .filter(|d| Path::new(d).is_dir())
                .cloned()
                .collect(),
            latest: Arc::new(AtomicU64::new(0)),
            running: Arc::new(AtomicBool::new(false)),
            last: None,
        }
    }

    fn poll(&mut self) -> Option<u64> {
        if self.dirs.is_empty() {
            return None;
        }
        let due = self.last.is_none_or(|t| t.elapsed() >= DISK_EVERY);
        if due && !self.running.swap(true, Ordering::SeqCst) {
            self.last = Some(Instant::now());
            let dirs = self.dirs.clone();
            let latest = self.latest.clone();
            let running = self.running.clone();
            tokio::task::spawn_blocking(move || {
                let mb = dir_bytes(&dirs, MAX_ENTRIES).div_ceil(BYTES_PER_MB);
                latest.store(mb + 1, Ordering::SeqCst);
                running.store(false, Ordering::SeqCst);
            });
        }
        self.latest.load(Ordering::SeqCst).checked_sub(1)
    }
}

pub(crate) struct Watch {
    mem: Meter,
    disk: Meter,
    probe: DiskProbe,
}

pub(crate) struct Sampled {
    pub(crate) warnings: Vec<u8>,
    pub(crate) kill: Option<Exceeded>,
}

impl Watch {
    pub(crate) fn new(limits: &ResourceLimits) -> Watch {
        Watch {
            mem: Meter::new(
                LimitedResource::Memory,
                limits.mem_mb,
                limits.declared_mem_mb,
                limits.mode,
            ),
            disk: Meter::new(
                LimitedResource::Disk,
                limits.disk_mb,
                limits.declared_disk_mb,
                limits.mode,
            ),
            probe: DiskProbe::new(&limits.disk_dirs),
        }
    }

    pub(crate) fn usage(&self) -> Usage {
        Usage {
            peak_mem_mb: self.mem.peak(),
            peak_disk_mb: self.disk.peak(),
        }
    }

    pub(crate) fn peak_mem_mb(&self) -> Option<u64> {
        self.mem.peak()
    }

    pub(crate) fn raise_mem_peak(&mut self, mb: u64) {
        self.mem.raise_peak(mb);
    }

    pub(crate) fn feed(&mut self, mem_mb: Option<u64>, disk_mb: Option<u64>) -> Sampled {
        let mut out = Sampled {
            warnings: Vec::new(),
            kill: None,
        };
        let verdicts = [
            mem_mb.map(|mb| self.mem.observe(mb)),
            disk_mb.map(|mb| self.disk.observe(mb)),
        ];
        for verdict in verdicts.into_iter().flatten() {
            match verdict {
                Verdict::Within => {}
                Verdict::Warn(ex) => out.warnings.extend_from_slice(warn_line(&ex).as_bytes()),
                Verdict::Kill(ex) => {
                    out.kill.get_or_insert(ex);
                }
            }
        }
        out
    }

    pub(crate) fn sample(&mut self, tree: &Tree) -> Sampled {
        let mem = tree.memory_mb();
        let disk = self.probe.poll();
        self.feed(mem, disk)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use lr_abstractions::Enforce;

    fn limits(mode: Enforce) -> ResourceLimits {
        ResourceLimits {
            mem_mb: Some(100),
            declared_mem_mb: Some(80),
            disk_mb: Some(10),
            mode,
            ..ResourceLimits::default()
        }
    }

    #[test]
    fn off_records_peaks_and_stays_silent() {
        let mut w = Watch::new(&limits(Enforce::Off));
        let s = w.feed(Some(500), Some(50));
        assert!(s.warnings.is_empty() && s.kill.is_none());
        assert_eq!(w.usage().peak_mem_mb, Some(500));
        assert_eq!(w.usage().peak_disk_mb, Some(50));
    }

    #[test]
    fn warn_writes_one_line_per_resource_once() {
        let mut w = Watch::new(&limits(Enforce::Warn));
        let first = w.feed(Some(120), Some(20));
        let text = String::from_utf8(first.warnings).unwrap();
        assert!(text.contains("memory peak 120 MB exceeds the declared mem_mb 80 MB"));
        assert!(text.contains("disk peak 20 MB"));
        assert!(first.kill.is_none());
        let second = w.feed(Some(130), Some(30));
        assert!(second.warnings.is_empty());
    }

    #[test]
    fn kill_reports_memory_before_disk() {
        let mut w = Watch::new(&limits(Enforce::Kill));
        assert!(w.feed(Some(100), Some(10)).kill.is_none());
        let ex = w.feed(Some(101), Some(99)).kill.unwrap();
        assert_eq!(ex.resource, LimitedResource::Memory);
        assert_eq!(ex.peak_mb, 101);
    }

    #[test]
    fn a_missing_sample_changes_nothing() {
        let mut w = Watch::new(&limits(Enforce::Kill));
        assert!(w.feed(None, None).kill.is_none());
        assert_eq!(w.usage(), Usage::default());
    }

    #[test]
    fn dir_size_sums_files_and_respects_the_entry_bound() {
        let dir = std::env::temp_dir().join(format!("lr-watch-{}", std::process::id()));
        let sub = dir.join("a").join("b");
        fs::create_dir_all(&sub).unwrap();
        fs::write(dir.join("one"), vec![0u8; 1000]).unwrap();
        fs::write(sub.join("two"), vec![0u8; 500]).unwrap();
        let dirs = [dir.clone(), dir.join("missing")];
        assert_eq!(dir_bytes(&dirs, 100), 1500);
        assert!(dir_bytes(&dirs, 1) <= 1000);
        let _ = fs::remove_dir_all(&dir);
    }
}
