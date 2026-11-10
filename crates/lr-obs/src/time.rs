use lr_core::Timestamp;

pub(crate) struct Civil {
    pub year: i64,
    pub month: u32,
    pub day: u32,
    pub hour: u32,
    pub minute: u32,
    pub second: u32,
    pub millis: u32,
}

pub(crate) fn civil(t: Timestamp) -> Civil {
    let secs = t.0 / 1000;
    let days = (secs / 86_400) as i64;
    let rem = secs % 86_400;
    let z = days + 719_468;
    let era = z.div_euclid(146_097);
    let doe = z.rem_euclid(146_097);
    let yoe = (doe - doe / 1460 + doe / 36_524 - doe / 146_096) / 365;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let day = doy - (153 * mp + 2) / 5 + 1;
    let month = if mp < 10 { mp + 3 } else { mp - 9 };
    Civil {
        year: yoe + era * 400 + i64::from(month <= 2),
        month: month as u32,
        day: day as u32,
        hour: (rem / 3600) as u32,
        minute: (rem % 3600 / 60) as u32,
        second: (rem % 60) as u32,
        millis: (t.0 % 1000) as u32,
    }
}

pub(crate) fn from_civil(c: &Civil) -> Option<Timestamp> {
    if !(1..=12).contains(&c.month)
        || !(1..=31).contains(&c.day)
        || c.hour > 23
        || c.minute > 59
        || c.second > 59
        || c.millis > 999
        || c.year < 1970
    {
        return None;
    }
    let y = if c.month <= 2 { c.year - 1 } else { c.year };
    let era = y.div_euclid(400);
    let yoe = y.rem_euclid(400);
    let m = i64::from(c.month);
    let doy = (153 * (if m > 2 { m - 3 } else { m + 9 }) + 2) / 5 + i64::from(c.day) - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let days = era * 146_097 + doe - 719_468;
    let secs =
        days * 86_400 + i64::from(c.hour) * 3600 + i64::from(c.minute) * 60 + i64::from(c.second);
    let ms = u64::try_from(secs).ok()?.checked_mul(1000)? + u64::from(c.millis);
    let back = civil(Timestamp(ms));
    (back.day == c.day && back.month == c.month).then_some(Timestamp(ms))
}

pub(crate) fn rfc3339(t: Timestamp) -> String {
    let c = civil(t);
    format!(
        "{:04}-{:02}-{:02}T{:02}:{:02}:{:02}.{:03}Z",
        c.year, c.month, c.day, c.hour, c.minute, c.second, c.millis
    )
}
