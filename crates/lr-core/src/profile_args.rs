use crate::error::ProfileError;
use regex::Regex;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;

/// Names the runner fills in `source` steps; a profile argument may not use them.
pub const RESERVED_ARG_NAMES: [&str; 5] = ["repo_url", "commit", "branch", "workspace", "job_dir"];

const DEFAULT_JOIN: &str = " ";

/// One `[profile.X.args.<name>]` declaration: a value the submitter fills into the commands.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ArgSpec {
    /// Argument name, the `{name}` of the command templates.
    pub name: String,
    /// Allowed values; a typo guard, not a security check.
    pub one_of: Option<Vec<String>>,
    /// Regular expression a whole value must match.
    pub pattern: Option<String>,
    /// Value used when the submitter gives none.
    pub default: Option<String>,
    /// Whether the argument may be given several times.
    pub many: bool,
    /// Separator between the values of a `many` argument.
    pub join: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(crate) struct RawArg {
    one_of: Option<Vec<String>>,
    pattern: Option<String>,
    default: Option<String>,
    #[serde(default)]
    many: bool,
    join: Option<String>,
}

pub(crate) fn full_match(pattern: &str) -> Result<Regex, regex::Error> {
    Regex::new(&format!("^(?:{pattern})$"))
}

pub(crate) fn build_args(
    profile: &str,
    raw: BTreeMap<String, RawArg>,
) -> Result<Vec<ArgSpec>, ProfileError> {
    raw.into_iter()
        .map(|(name, a)| build_arg(profile, name, a))
        .collect()
}

fn build_arg(profile: &str, name: String, a: RawArg) -> Result<ArgSpec, ProfileError> {
    let at = format!("args.{name}");
    let bad = |key: &str, reason: &str| {
        let field = if key.is_empty() {
            at.clone()
        } else {
            format!("{at}.{key}")
        };
        Err(ProfileError::invalid(profile, &field, reason))
    };
    if name.is_empty() || !name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
        return bad("", "the name must be letters, digits and `_`");
    }
    if RESERVED_ARG_NAMES.contains(&name.as_str()) {
        return bad("", "the name is reserved for the runner `source` placeholders");
    }
    if a.one_of.is_some() && a.pattern.is_some() {
        return bad("", "`one_of` and `pattern` are mutually exclusive");
    }
    if a.join.is_some() && !a.many {
        return bad("join", "`join` needs `many = true`");
    }
    let matcher = match &a.pattern {
        Some(p) => match full_match(p) {
            Ok(r) => Some(r),
            Err(e) => return bad("pattern", &e.to_string()),
        },
        None => None,
    };
    if let Some(d) = &a.default {
        if a.one_of.as_ref().is_some_and(|list| !list.contains(d)) {
            return bad("default", "the default is not in `one_of`");
        }
        if matcher.as_ref().is_some_and(|r| !r.is_match(d)) {
            return bad("default", "the default does not match `pattern`");
        }
    }
    Ok(ArgSpec {
        name,
        one_of: a.one_of,
        pattern: a.pattern,
        default: a.default,
        many: a.many,
        join: a.join.unwrap_or_else(|| DEFAULT_JOIN.to_string()),
    })
}
