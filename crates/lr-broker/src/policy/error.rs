use std::fmt;
use std::ops::Range;

/// A refused `policy.toml` (spec 3.5): what is wrong and where.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub struct PolicyError {
    /// Table the error is in (`grace`, `rule`, `secrets.allow`); `None` at the top level.
    pub table: Option<String>,
    /// Key the error is about, when one is known.
    pub key: Option<String>,
    /// 1-based line of the file, when known.
    pub line: Option<usize>,
    /// What is wrong.
    pub message: String,
}

/// A `policy check` finding (spec 3.5, 11.1): the file applies, but the owner should look.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PolicyWarning {
    /// Table the warning is about.
    pub table: Option<String>,
    /// Key the warning is about, when one is known.
    pub key: Option<String>,
    /// 1-based line of the file, when known.
    pub line: Option<usize>,
    /// What looks wrong.
    pub message: String,
}

fn render(
    f: &mut fmt::Formatter<'_>,
    table: &Option<String>,
    key: &Option<String>,
    line: Option<usize>,
    message: &str,
) -> fmt::Result {
    f.write_str("policy.toml")?;
    if let Some(line) = line {
        write!(f, " line {line}")?;
    }
    match (table, key) {
        (Some(t), Some(k)) => write!(f, ", [{t}] {k}")?,
        (Some(t), None) => write!(f, ", [{t}]")?,
        (None, Some(k)) => write!(f, ", {k}")?,
        (None, None) => {}
    }
    write!(f, ": {message}")
}

impl fmt::Display for PolicyError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render(f, &self.table, &self.key, self.line, &self.message)
    }
}

impl fmt::Display for PolicyWarning {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        render(f, &self.table, &self.key, self.line, &self.message)
    }
}

impl PolicyError {
    pub(crate) fn at(table: &str, key: Option<&str>, line: Option<usize>, message: String) -> Self {
        PolicyError {
            table: Some(table.to_string()),
            key: key.map(str::to_string),
            line,
            message,
        }
    }

    pub(crate) fn from_toml(e: &toml::de::Error, text: &str) -> Self {
        let message = e.message().trim().to_string();
        let Some(span) = e.span() else {
            return PolicyError {
                table: None,
                key: None,
                line: None,
                message,
            };
        };
        let start = span.start.min(text.len());
        PolicyError {
            table: table_at(text, start),
            key: key_at(text, &span),
            line: Some(line_of(text, start)),
            message,
        }
    }
}

impl PolicyWarning {
    pub(crate) fn at(table: &str, key: Option<&str>, line: Option<usize>, message: String) -> Self {
        PolicyWarning {
            table: Some(table.to_string()),
            key: key.map(str::to_string),
            line,
            message,
        }
    }
}

pub(crate) fn line_of(text: &str, offset: usize) -> usize {
    text[..offset.min(text.len())].matches('\n').count() + 1
}

fn header_name(line: &str) -> Option<String> {
    let t = line.trim_start();
    if !t.starts_with('[') {
        return None;
    }
    let inner = t.trim_start_matches('[');
    let end = inner.find(']')?;
    let name: String = inner[..end]
        .split('.')
        .map(|part| part.trim().trim_matches('"').trim_matches('\''))
        .collect::<Vec<_>>()
        .join(".");
    Some(name)
}

fn lines_until(text: &str, offset: usize) -> impl Iterator<Item = (usize, &str)> {
    let mut start = 0;
    text.split_inclusive('\n')
        .map(move |l| {
            let s = start;
            start += l.len();
            (s, l)
        })
        .take_while(move |(s, _)| *s <= offset)
}

pub(crate) fn table_at(text: &str, offset: usize) -> Option<String> {
    lines_until(text, offset)
        .filter_map(|(_, l)| header_name(l))
        .last()
}

pub(crate) fn header_line_at(text: &str, offset: usize) -> Option<(usize, String)> {
    lines_until(text, offset)
        .filter_map(|(s, l)| header_name(l).map(|n| (line_of(text, s), n)))
        .last()
}

fn is_bare_key(s: &str) -> bool {
    !s.is_empty()
        && s.chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-')
}

fn key_of_line(line: &str) -> Option<String> {
    let t = line.trim_start();
    if t.starts_with('[') || t.starts_with('#') {
        return None;
    }
    let (key, _) = t.split_once('=')?;
    let key = key.trim().trim_matches('"');
    is_bare_key(key).then(|| key.to_string())
}

fn key_at(text: &str, span: &Range<usize>) -> Option<String> {
    let end = span.end.min(text.len());
    let start = span.start.min(end);
    let snippet = text[start..end].trim().trim_matches('"');
    if is_bare_key(snippet) && text[end..].trim_start().starts_with('=') {
        return Some(snippet.to_string());
    }
    let lines: Vec<(usize, &str)> = lines_until(text, start).collect();
    for (_, line) in lines.iter().rev() {
        if header_name(line).is_some() {
            return None;
        }
        if let Some(key) = key_of_line(line) {
            return Some(key);
        }
    }
    None
}
