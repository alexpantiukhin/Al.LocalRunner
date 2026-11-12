//! Times and durations as the read commands print and take them: UTC, `YYYY-MM-DD HH:MM:SSZ`;
//! `--since` as an age (`90s`, `30m`, `2h`, `7d`) or a UTC date (`2026-11-01`).

use lr_core::Timestamp;

const DAY_MS: u64 = 86_400_000;

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = (m + 9) % 12;
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = yoe + era * 400;
    (if m <= 2 { y + 1 } else { y }, m, d)
}

/// `t` in UTC as `YYYY-MM-DD HH:MM:SSZ`.
pub fn format_time(t: Timestamp) -> String {
    let days = (t.0 / DAY_MS) as i64;
    let rest = t.0 % DAY_MS / 1000;
    let (y, m, d) = civil_from_days(days);
    format!(
        "{y:04}-{m:02}-{d:02} {:02}:{:02}:{:02}Z",
        rest / 3600,
        rest / 60 % 60,
        rest % 60
    )
}

/// A duration in ms, short: `850ms`, `12.4s`, `3m05s`, `2h07m`.
pub fn format_ms(ms: u64) -> String {
    match ms {
        0..=999 => format!("{ms}ms"),
        1000..=59_999 => format!("{}.{}s", ms / 1000, ms % 1000 / 100),
        60_000..=3_599_999 => format!("{}m{:02}s", ms / 60_000, ms / 1000 % 60),
        _ => format!("{}h{:02}m", ms / 3_600_000, ms / 60_000 % 60),
    }
}

/// `--since`: runs queued at or after an age before now or a UTC date.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Since {
    /// This many ms before now.
    Ago(u64),
    /// From this moment.
    At(Timestamp),
}

impl Since {
    /// The moment it names, given `now`.
    pub fn resolve(self, now: Timestamp) -> Timestamp {
        match self {
            Since::Ago(ms) => Timestamp(now.0.saturating_sub(ms)),
            Since::At(t) => t,
        }
    }
}

fn date(text: &str) -> Option<Timestamp> {
    let mut parts = text.splitn(3, '-');
    let mut num = |len: usize| -> Option<i64> {
        let p = parts.next()?;
        (p.len() == len && p.bytes().all(|b| b.is_ascii_digit()))
            .then(|| p.parse().ok())
            .flatten()
    };
    let (y, m, d) = (num(4)?, num(2)?, num(2)?);
    let month_days = [31, 29, 31, 30, 31, 30, 31, 31, 30, 31, 30, 31];
    if !(1970..=9999).contains(&y) || !(1..=12).contains(&m) {
        return None;
    }
    if d < 1 || d > month_days[(m - 1) as usize] {
        return None;
    }
    let days = days_from_civil(y, m, d);
    if civil_from_days(days) != (y, m, d) {
        return None;
    }
    Some(Timestamp(days as u64 * DAY_MS))
}

/// Parses `--since`: `90s`, `30m`, `2h`, `7d` or `YYYY-MM-DD` (UTC).
pub fn parse_since(text: &str) -> Result<Since, String> {
    let bad =
        || format!("`{text}` is not a period: give an age (30m, 2h, 7d) or a date (2026-11-01)");
    if let Some(t) = date(text) {
        return Ok(Since::At(t));
    }
    let unit = match text.as_bytes().last() {
        Some(b's') => 1_000,
        Some(b'm') => 60_000,
        Some(b'h') => 3_600_000,
        Some(b'd') => DAY_MS,
        _ => return Err(bad()),
    };
    let digits = &text[..text.len() - 1];
    if digits.is_empty() || !digits.bytes().all(|b| b.is_ascii_digit()) {
        return Err(bad());
    }
    digits
        .parse::<u64>()
        .ok()
        .and_then(|n| n.checked_mul(unit))
        .map(Since::Ago)
        .ok_or_else(bad)
}

/// Now on the wall clock.
pub fn now() -> Timestamp {
    let ms = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| u64::try_from(d.as_millis()).unwrap_or(u64::MAX));
    Timestamp(ms)
}
