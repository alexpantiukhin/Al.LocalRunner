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
//! [`NOT_SENSITIVE`] are exceptions to the second rule only; neither the built-in names nor
//! the configured `[redact] fields` can be weakened by them. The configured list only
//! extends the built-in one.
//!
//! Registered values are found with one Aho-Corasick automaton. [`Redactor::protect`] only
//! marks it stale; the next redaction rebuilds it outside the lock (a burst of registrations
//! costs one rebuild), and a redaction that starts after `protect` returned always uses an
//! automaton that holds the value. Overlapping matches are merged into their union while
//! they are found, so no tail of a longer or overlapping value is left; past 65 536 matches
//! in one text (a degenerate set of values) the rest of the text is masked.
//!
//! A message or value is cut before redaction at its limit plus the longest registered
//! value, and the cut is moved back, until stable, to the start of a registered value it
//! falls inside and to the start of the token it falls inside (tokens end at whitespace,
//! `,` and `;`). Every pattern is therefore judged on complete tokens: a URL credential,
//! `Basic` value or key-value pair split by the limit is dropped whole, never emitted as an
//! unmasked prefix. A text whose first token is longer than the limit becomes
//! [`TRUNCATED`] alone. The result is then cut to the limit and marked with [`TRUNCATED`].
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
/// (counters, people, outcomes); compared without separators, case-insensitive. They are
/// exceptions to the [`SENSITIVE_PARTS`] rule only: a built-in or configured name that
/// matches by segments is always sensitive (`token_count` is, by the segment `token`).
pub const NOT_SENSITIVE: &[&str] = &[
    "tokens_used",
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
    generation: u64,
    matcher: Option<Arc<Matcher>>,
}

struct Matcher {
    generation: u64,
    automaton: Option<AhoCorasick>,
    values: Vec<String>,
    longest: usize,
}

const MATCH_BUDGET: usize = 65_536;
const TOKEN_ENDS: [char; 2] = [',', ';'];

fn merge(merged: &mut Vec<(usize, usize)>, mut start: usize, mut end: usize) {
    while let Some(&(s, e)) = merged.last() {
        if start > e {
            break;
        }
        start = start.min(s);
        end = end.max(e);
        merged.pop();
    }
    merged.push((start, end));
}

fn is_token_end(c: char) -> bool {
    c.is_whitespace() || TOKEN_ENDS.contains(&c)
}

fn token_start(text: &str, at: usize) -> usize {
    let before = text[..at].chars().next_back();
    let after = text[at..].chars().next();
    if before.is_none_or(is_token_end) || after.is_none_or(is_token_end) {
        return at;
    }
    text[..at].rfind(is_token_end).unwrap_or(0)
}

impl Matcher {
    fn build(order: &VecDeque<String>, generation: u64) -> Matcher {
        let values: Vec<String> = order.iter().cloned().collect();
        let longest = values.iter().map(String::len).max().unwrap_or(0);
        let automaton = if values.is_empty() {
            None
        } else {
            AhoCorasick::new(&values).ok()
        };
        Matcher {
            generation,
            automaton,
            values,
            longest,
        }
    }

    fn spans(&self, text: &str) -> Vec<(usize, usize)> {
        let mut merged = Vec::new();
        let Some(ac) = &self.automaton else {
            let mut all: Vec<(usize, usize)> = self
                .values
                .iter()
                .flat_map(|v| {
                    text.match_indices(v.as_str())
                        .map(|(i, _)| (i, i + v.len()))
                })
                .collect();
            all.sort_unstable_by_key(|&(start, end)| (end, start));
            for (start, end) in all {
                merge(&mut merged, start, end);
            }
            return merged;
        };
        for (n, m) in ac.find_overlapping_iter(text).enumerate() {
            if n >= MATCH_BUDGET {
                let mut from = m.end().saturating_sub(self.longest);
                while !text.is_char_boundary(from) {
                    from -= 1;
                }
                merge(&mut merged, from, text.len());
                break;
            }
            merge(&mut merged, m.start(), m.end());
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
        let mut window_end = (at + self.longest).min(text.len());
        while !text.is_char_boundary(window_end) {
            window_end += 1;
        }
        let spans = if self.longest > 0 {
            self.spans(&text[..window_end])
        } else {
            Vec::new()
        };
        loop {
            let before = at;
            if let Some(&(start, _)) = spans.iter().find(|(s, e)| *s < at && at < *e) {
                at = start;
            }
            at = token_start(text, at);
            if at == before {
                return Some(at);
            }
        }
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
                generation: 0,
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
        s.generation += 1;
    }

    fn matcher(&self) -> Arc<Matcher> {
        let (order, generation) = {
            let s = self.secrets.read().unwrap_or_else(|e| e.into_inner());
            if let Some(m) = s.matcher.as_ref().filter(|m| m.generation == s.generation) {
                return m.clone();
            }
            (s.order.clone(), s.generation)
        };
        let built = Arc::new(Matcher::build(&order, generation));
        let mut s = self.secrets.write().unwrap_or_else(|e| e.into_inner());
        if s.matcher.as_ref().is_none_or(|m| m.generation < generation) {
            s.matcher = Some(built.clone());
        }
        built
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
        let segs = scan::segments(name);
        let by_segments = {
            let patterns = self.patterns.read().unwrap_or_else(|e| e.into_inner());
            patterns
                .iter()
                .any(|p| p.len() <= segs.len() && segs.windows(p.len()).any(|w| w == p.as_slice()))
        };
        if by_segments {
            return true;
        }
        let flat = squashed(name);
        SENSITIVE_PARTS.iter().any(|p| flat.contains(p))
            && !NOT_SENSITIVE.iter().any(|n| squashed(n) == flat)
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
