//! Line formats of the local sinks. Both put one record on exactly one line and escape
//! control characters (C0, DEL, C1), bidirectional controls (U+200E/F, U+202A-202E,
//! U+2066-2069) and the line separators U+2028/2029, so a field value cannot forge a second
//! record, reorder what a reader sees or recolour a terminal.

use crate::redact::Redacted;
use crate::time::rfc3339;
use lr_abstractions::{Level, Role};
use serde::Deserialize;

/// Line format of a file sink (`format`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Format {
    /// One JSON object per line (default; what Alloy or Promtail read).
    Json,
    /// One human-readable line, as on the console.
    Text,
}

/// Lower-case level name.
pub fn level_name(level: Level) -> &'static str {
    match level {
        Level::Trace => "trace",
        Level::Debug => "debug",
        Level::Info => "info",
        Level::Warn => "warn",
        Level::Error => "error",
    }
}

/// Role name as in spec 12 (`hub`, `broker`, `runner`).
pub fn role_name(role: Role) -> &'static str {
    match role {
        Role::Hub => "hub",
        Role::Broker => "broker",
        Role::Runner => "runner",
    }
}

fn unique_fields(r: &Redacted) -> Vec<&(String, String)> {
    let fields = &r.record().fields;
    fields
        .iter()
        .enumerate()
        .filter(|(i, (name, _))| !fields[i + 1..].iter().any(|(n, _)| n == name))
        .map(|(_, f)| f)
        .collect()
}

fn js(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if needs_escape(c) => out.push_str(&format!("\\u{:04x}", c as u32)),
            c => out.push(c),
        }
    }
    out.push('"');
    out
}

/// The record as one JSON line ending in `\n`: `time` (RFC 3339 UTC, milliseconds),
/// `level`, `role`, `component`, `machine_id`, `message` and the contextual `fields` as an
/// object (a repeated name keeps its last value).
pub fn json_line(r: &Redacted) -> String {
    let rec = r.record();
    let fields: Vec<String> = unique_fields(r)
        .into_iter()
        .map(|(n, v)| format!("{}:{}", js(n), js(v)))
        .collect();
    format!(
        "{{\"time\":{},\"level\":{},\"role\":{},\"component\":{},\"machine_id\":{},\"message\":{},\"fields\":{{{}}}}}\n",
        js(&rfc3339(rec.time)),
        js(level_name(rec.level)),
        js(role_name(rec.role)),
        js(&rec.component),
        js(&rec.machine_id),
        js(&rec.message),
        fields.join(",")
    )
}

fn needs_escape(c: char) -> bool {
    c.is_control()
        || matches!(
            c,
            '\u{200e}' | '\u{200f}' | '\u{2028}' | '\u{2029}' | '\u{202a}'..='\u{202e}' | '\u{2066}'..='\u{2069}'
        )
}

/// Escapes characters that could break the line or control a terminal.
pub fn escape(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for c in s.chars() {
        match c {
            '\n' => out.push_str("\\n"),
            '\r' => out.push_str("\\r"),
            '\t' => out.push_str("\\t"),
            c if needs_escape(c) => out.push_str(&format!("\\u{{{:x}}}", c as u32)),
            c => out.push(c),
        }
    }
    out
}

fn field_name(name: &str) -> String {
    let n: String = name
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if n.is_empty() {
        "_".to_string()
    } else {
        n
    }
}

fn quoted(value: &str) -> String {
    let escaped = escape(&value.replace('\\', "\\\\")).replace('"', "\\\"");
    format!("\"{escaped}\"")
}

fn field_value(value: &str) -> String {
    let plain = !value.is_empty()
        && !value.contains([' ', '"', '=', '\\'])
        && !value.chars().any(needs_escape);
    if plain {
        value.to_string()
    } else {
        quoted(value)
    }
}

/// The record as one text line ending in `\n`:
/// `[time LEVEL ]role component "message" name=value ...`. The message is always quoted and
/// values are quoted when they hold spaces, quotes, `=`, `\` or escaped characters, with `\`
/// and `"` escaped inside quotes, so no text can forge another field. The time and level are
/// left out for the OS journal, which records both itself.
pub fn text_line(r: &Redacted, with_time: bool) -> String {
    let rec = r.record();
    let mut line = String::new();
    if with_time {
        line.push_str(&rfc3339(rec.time));
        line.push(' ');
        line.push_str(&format!("{:<5} ", level_name(rec.level).to_uppercase()));
    }
    line.push_str(role_name(rec.role));
    line.push(' ');
    line.push_str(&field_name(&rec.component));
    line.push(' ');
    line.push_str(&quoted(&rec.message));
    for (name, value) in unique_fields(r) {
        line.push(' ');
        line.push_str(&field_name(name));
        line.push('=');
        line.push_str(&field_value(value));
    }
    line.push('\n');
    line
}
