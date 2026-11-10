//! Field redaction (spec 12): every record passes here before any sink sees it.
//!
//! What is masked, in this order, with [`MASK`]:
//! 1. secret values registered at run time ([`Redactor::protect`], [`RedactingSecrets`]),
//!    wherever they occur in the message, field values, component or MachineId;
//! 2. PEM blocks whose header names a `PRIVATE KEY`, up to their `END` line;
//! 3. join codes (`lrj1.` followed by the code);
//! 4. credentials in URLs (`scheme://user:pass@host`);
//! 5. `Bearer <token>` and `Basic <base64>` credentials;
//! 6. the value after a sensitive name followed by `=` or `:` inside text (`token=..`,
//!    `"password": ".."`, `{\"token\":\"..\"}`, `?api_key=..&`): a quoted value up to
//!    its closing quote, a nested `{..}`/`[..]` value as a whole, an unquoted value after
//!    `=` up to the next space, after `:` up to the end of the line or `,` `;` `&`, and the
//!    value of an `Authorization`/`auth` name up to the end of the line (`token ..`,
//!    `Digest ..`, `Basic ..`);
//! 7. the whole value of a field whose name is sensitive.
//!
//! A name is sensitive when the segments of one of [`BUILTIN_FIELDS`] or of the configured
//! `[redact] fields` occur, in order, among its segments (split at `_`, `-`, `.`, camelCase,
//! acronyms and letter/digit changes, case-insensitive: `APIKey` is `api`, `key`; `token2`
//! is `token`, `2`), or when the name with separators removed contains one of
//! [`SENSITIVE_PARTS`] (`PGPASSWORD`, `accesstoken`, `dbPwd`, `OAuthHeader`). Names in
//! [`NOT_SENSITIVE`] are the documented exceptions to the second rule. The configured list
//! only extends the built-in one.
//!
//! Registered values are found with one Aho-Corasick automaton (rebuilt after
//! [`Redactor::protect`]); overlapping matches are masked as their union, so no tail of a
//! longer or overlapping value is left. A message or value is cut to its limit plus the
//! longest registered value before redaction, moved back to the start of a registered
//! value that straddles the cut, so redaction costs a bounded amount and a cut never leaves
//! an unmasked prefix of a registered value.
//!
//! Limits (no false confidence): a secret that was never registered and appears without a
//! sensitive name, scheme or known format next to it is not recognised; registered values
//! shorter than [`MIN_SECRET_LEN`] are ignored; encoded or split forms of a registered value
//! (base64, URL-escaped, cut by truncation of the caller) are not matched; at most
//! [`MAX_SECRETS`] values are kept, the oldest forgotten first.

use crate::scan;
use aho_corasick::AhoCorasick;
use async_trait::async_trait;
use lr_abstractions::{Record, SecretError, SecretSource, SecretValue, MASK};
use std::collections::{HashSet, VecDeque};
use std::sync::{Arc, RwLock};

/// Field names always treated as sensitive (spec 12: commands and environment of jobs, keys,
/// join codes and tokens are never written).
pub const BUILTIN_FIELDS: &[&str] = &[
    "token",
    "key",
    "password",
    "passwd",
    "secret",
    "authorization",
    "auth",
    "cookie",
    "credential",
    "credentials",
    "command",
    "env",
    "environment",
    "join_code",
];

/// Parts that make a name sensitive wherever they occur in it (separators removed,
/// case-insensitive).
pub const SENSITIVE_PARTS: &[&str] = &[
    "token",
    "secret",
    "passw",
    "apikey",
    "passphrase",
    "pwd",
    "credential",
    "bearer",
    "auth",
];

/// Names that contain one of [`SENSITIVE_PARTS`] but are known not to hold secrets
/// (counters, people, outcomes); compared without separators, case-insensitive.
pub const NOT_SENSITIVE: &[&str] = &[
    "tokens_used",
    "token_count",
    "author",
    "authority",
    "unauthorized",
    "authenticated",
];

/// Registered secret values shorter than this are ignored: masking them would garble logs.
pub const MIN_SECRET_LEN: usize = 4;

/// Most registered secret values kept; the oldest is forgotten first.
pub const MAX_SECRETS: usize = 4096;

/// Longest message kept, in bytes; longer ones are cut and end with [`TRUNCATED`].
pub const MAX_MESSAGE_BYTES: usize = 16 * 1024;

/// Longest field value kept, in bytes.
pub const MAX_FIELD_BYTES: usize = 4 * 1024;

/// Most fields kept per record; later ones are dropped.
pub const MAX_FIELDS: usize = 64;

/// Appended to a value that was cut.
pub const TRUNCATED: &str = "...[truncated]";

const MAX_NAME_BYTES: usize = 128;
const MAX_LABEL_BYTES: usize = 256;
const RECORD_OVERHEAD: usize = 64;

/// A record that went through a [`Redactor`]: only [`Redactor::redact`] constructs one and
/// sinks accept only this type, so every record a sink sees was passed through some
/// redactor. It does not prove which redactor, or that its configuration was the current one.
#[derive(Debug, Clone)]
pub struct Redacted {
    record: Record,
    size: usize,
}

impl Redacted {
    /// The redacted record.
    pub fn record(&self) -> &Record {
        &self.record
    }

    /// Approximate memory taken by the record, in bytes, for queue limits.
    pub fn size(&self) -> usize {
        self.size
    }
}

struct Secrets {
    order: VecDeque<String>,
    known: HashSet<String>,
    matcher: Option<Arc<Matcher>>,
}

struct Matcher {
    automaton: Option<AhoCorasick>,
    values: Vec<String>,
    longest: usize,
}

impl Matcher {
    fn build(order: &VecDeque<String>) -> Matcher {
        let values: Vec<String> = order.iter().cloned().collect();
        let longest = values.iter().map(String::len).max().unwrap_or(0);
        let automaton = if values.is_empty() {
            None
        } else {
            AhoCorasick::new(&values).ok()
        };
        Matcher {
            automaton,
            values,
            longest,
        }
    }

    fn spans(&self, text: &str) -> Vec<(usize, usize)> {
        let mut spans: Vec<(usize, usize)> = match &self.automaton {
            Some(ac) => ac
                .find_overlapping_iter(text)
                .map(|m| (m.start(), m.end()))
                .collect(),
            None => self
                .values
                .iter()
                .flat_map(|v| {
                    text.match_indices(v.as_str())
                        .map(|(i, _)| (i, i + v.len()))
                })
                .collect(),
        };
        spans.sort_unstable();
        let mut merged: Vec<(usize, usize)> = Vec::with_capacity(spans.len());
        for (start, end) in spans {
            match merged.last_mut() {
                Some(last) if start <= last.1 => last.1 = last.1.max(end),
                _ => merged.push((start, end)),
            }
        }
        merged
    }

    fn cut(&self, text: &str, max: usize) -> Option<usize> {
        let mut at = max.checked_add(self.longest)?;
        if text.len() <= at {
            return None;
        }
        while !text.is_char_boundary(at) {
            at -= 1;
        }
        let window = (at + self.longest).min(text.len());
        let mut window_end = window;
        while !text.is_char_boundary(window_end) {
            window_end += 1;
        }
        if let Some(&(start, _)) = self
            .spans(&text[..window_end])
            .iter()
            .find(|(start, end)| *start < at && at < *end)
        {
            at = start;
        }
        Some(at)
    }

    fn mask(&self, text: &str) -> String {
        let spans = self.spans(text);
        if spans.is_empty() {
            return text.to_string();
        }
        let mut out = String::with_capacity(text.len());
        let mut from = 0;
        for (start, end) in spans {
            out.push_str(&text[from..start]);
            out.push_str(MASK);
            from = end;
        }
        out.push_str(&text[from..]);
        out
    }
}

fn squashed(name: &str) -> String {
    name.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}

/// Masks secrets in records (see the module documentation for what and how).
pub struct Redactor {
    patterns: RwLock<Vec<Vec<String>>>,
    secrets: RwLock<Secrets>,
}

fn patterns(extra: &[String]) -> Vec<Vec<String>> {
    BUILTIN_FIELDS
        .iter()
        .map(|s| s.to_string())
        .chain(extra.iter().cloned())
        .map(|n| scan::segments(&n))
        .filter(|s| !s.is_empty())
        .collect()
}

fn truncate(mut s: String, max: usize, cut: bool) -> String {
    if s.len() <= max {
        if cut {
            s.push_str(TRUNCATED);
        }
        return s;
    }
    let mut cut = max;
    while !s.is_char_boundary(cut) {
        cut -= 1;
    }
    s.truncate(cut);
    s.push_str(TRUNCATED);
    s
}

impl Redactor {
    /// A redactor for the built-in names plus `fields` (`[redact] fields`).
    pub fn new(fields: &[String]) -> Redactor {
        Redactor {
            patterns: RwLock::new(patterns(fields)),
            secrets: RwLock::new(Secrets {
                order: VecDeque::new(),
                known: HashSet::new(),
                matcher: None,
            }),
        }
    }

    /// Replaces the configured names (on `logging reload`); registered values stay.
    pub fn set_fields(&self, fields: &[String]) {
        *self.patterns.write().unwrap_or_else(|e| e.into_inner()) = patterns(fields);
    }

    /// Registers a secret value so it is masked wherever it appears from now on.
    pub fn protect(&self, value: &str) {
        if value.len() < MIN_SECRET_LEN {
            return;
        }
        let mut s = self.secrets.write().unwrap_or_else(|e| e.into_inner());
        if !s.known.insert(value.to_string()) {
            return;
        }
        s.order.push_back(value.to_string());
        while s.order.len() > MAX_SECRETS {
            if let Some(old) = s.order.pop_front() {
                s.known.remove(&old);
            }
        }
        s.matcher = None;
    }

    fn matcher(&self) -> Arc<Matcher> {
        if let Some(m) = &self
            .secrets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .matcher
        {
            return m.clone();
        }
        let mut s = self.secrets.write().unwrap_or_else(|e| e.into_inner());
        if let Some(m) = &s.matcher {
            return m.clone();
        }
        let m = Arc::new(Matcher::build(&s.order));
        s.matcher = Some(m.clone());
        m
    }

    /// Number of registered secret values.
    pub fn secrets(&self) -> usize {
        self.secrets
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .order
            .len()
    }

    /// Whether a field of this name has its whole value masked.
    pub fn is_sensitive_field(&self, name: &str) -> bool {
        let flat = squashed(name);
        if NOT_SENSITIVE.iter().any(|n| squashed(n) == flat) {
            return false;
        }
        if SENSITIVE_PARTS.iter().any(|p| flat.contains(p)) {
            return true;
        }
        let segs = scan::segments(name);
        let patterns = self.patterns.read().unwrap_or_else(|e| e.into_inner());
        patterns
            .iter()
            .any(|p| p.len() <= segs.len() && segs.windows(p.len()).any(|w| w == p.as_slice()))
    }

    /// Masks registered values, known secret formats and sensitive `name=value` pairs in text.
    pub fn redact_text(&self, text: &str) -> String {
        let out = self.matcher().mask(text);
        self.scan(&out)
    }

    fn bounded(&self, text: &str, max: usize) -> String {
        let matcher = self.matcher();
        let cut = matcher.cut(text, max);
        let input = cut.map_or(text, |at| &text[..at]);
        let out = self.scan(&matcher.mask(input));
        truncate(out, max, cut.is_some())
    }

    fn scan(&self, text: &str) -> String {
        let out = scan::private_keys(text);
        let out = scan::join_codes(&out);
        let out = scan::url_credentials(&out);
        let out = scan::auth_schemes(&out);
        scan::key_values(&out, |w| self.is_sensitive_field(w))
    }

    /// The record with every secret masked and every value bounded.
    pub fn redact(&self, record: &Record) -> Redacted {
        let fields: Vec<(String, String)> = record
            .fields
            .iter()
            .take(MAX_FIELDS)
            .map(|(name, value)| {
                let value = if self.is_sensitive_field(name) {
                    MASK.to_string()
                } else {
                    self.bounded(value, MAX_FIELD_BYTES)
                };
                (self.bounded(name, MAX_NAME_BYTES), value)
            })
            .collect();
        let record = Record {
            time: record.time,
            level: record.level,
            role: record.role,
            component: self.bounded(&record.component, MAX_LABEL_BYTES),
            machine_id: self.bounded(&record.machine_id, MAX_LABEL_BYTES),
            message: self.bounded(&record.message, MAX_MESSAGE_BYTES),
            fields,
        };
        let size = RECORD_OVERHEAD
            + record.component.len()
            + record.machine_id.len()
            + record.message.len()
            + record
                .fields
                .iter()
                .map(|(n, v)| n.len() + v.len() + 16)
                .sum::<usize>();
        Redacted { record, size }
    }
}

/// A [`SecretSource`] that registers every value it hands out with a [`Redactor`], so a
/// secret a runner fetched for a job is masked in diagnostics from then on.
pub struct RedactingSecrets<S> {
    inner: S,
    redactor: Arc<Redactor>,
}

impl<S> RedactingSecrets<S> {
    /// Wraps `inner`.
    pub fn new(inner: S, redactor: Arc<Redactor>) -> RedactingSecrets<S> {
        RedactingSecrets { inner, redactor }
    }
}

#[async_trait]
impl<S: SecretSource> SecretSource for RedactingSecrets<S> {
    async fn get(&self, name: &str) -> Result<SecretValue, SecretError> {
        let value = self.inner.get(name).await?;
        self.redactor.protect(value.expose());
        Ok(value)
    }
}
