#[cfg(target_os = "linux")]
pub(crate) fn rss_bytes(pids: &[u32]) -> u64 {
    let page = unsafe { libc::sysconf(libc::_SC_PAGESIZE) }.max(1) as u64;
    pids.iter()
        .filter_map(|pid| std::fs::read_to_string(format!("/proc/{pid}/statm")).ok())
        .filter_map(|text| text.split_whitespace().nth(1)?.parse::<u64>().ok())
        .map(|pages| pages.saturating_mul(page))
        .sum()
}

#[cfg(not(target_os = "linux"))]
pub(crate) fn rss_bytes(pids: &[u32]) -> u64 {
    if pids.is_empty() {
        return 0;
    }
    let list: Vec<String> = pids.iter().map(u32::to_string).collect();
    let out = std::process::Command::new("ps")
        .args(["-o", "rss=", "-p", &list.join(",")])
        .stdin(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .output();
    let Ok(out) = out else {
        return 0;
    };
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .filter_map(|l| l.trim().parse::<u64>().ok())
        .map(|kib| kib.saturating_mul(1024))
        .sum()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn this_process_has_resident_memory() {
        assert!(rss_bytes(&[std::process::id()]) > 0);
        assert_eq!(rss_bytes(&[]), 0);
    }
}
