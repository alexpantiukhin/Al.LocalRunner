use crate::error::ProfileError;
use crate::profile::StepSpec;
use crate::profile_args::{full_match, ArgSpec};
use std::collections::BTreeMap;

type Values = BTreeMap<String, String>;

fn args_err(text: impl Into<String>) -> ProfileError {
    ProfileError::Args(text.into())
}

/// Fills the `{name}` and `{? ... }` placeholders of every step command with the values the
/// submitter gave (or the declared defaults). A profile without declared arguments is left
/// alone, and braces that are not a declared argument stay as written.
pub fn bind_args(
    steps: &mut [StepSpec],
    specs: &[ArgSpec],
    given: &[(String, String)],
) -> Result<(), ProfileError> {
    let values = resolve(specs, given)?;
    if specs.is_empty() {
        return Ok(());
    }
    for step in steps {
        step.command = render_top(&step.command, specs, &values)?;
    }
    Ok(())
}

fn resolve(specs: &[ArgSpec], given: &[(String, String)]) -> Result<Values, ProfileError> {
    let mut by_name: BTreeMap<&str, Vec<&str>> = BTreeMap::new();
    for (name, value) in given {
        let Some(spec) = specs.iter().find(|s| &s.name == name) else {
            let declared: Vec<&str> = specs.iter().map(|s| s.name.as_str()).collect();
            let list = if declared.is_empty() {
                "none".to_string()
            } else {
                declared.join(", ")
            };
            return Err(args_err(format!(
                "unknown argument `{name}`; the profile declares: {list}"
            )));
        };
        check_value(spec, value)?;
        by_name.entry(&spec.name).or_default().push(value);
    }
    let mut out = Values::new();
    for spec in specs {
        match by_name.get(spec.name.as_str()) {
            Some(list) if list.len() > 1 && !spec.many => {
                return Err(args_err(format!(
                    "argument `{}` was given {} times but is not declared with many = true",
                    spec.name,
                    list.len()
                )))
            }
            Some(list) => {
                out.insert(spec.name.clone(), list.join(&spec.join));
            }
            None => {
                if let Some(d) = &spec.default {
                    out.insert(spec.name.clone(), d.clone());
                }
            }
        }
    }
    Ok(out)
}

fn check_value(spec: &ArgSpec, value: &str) -> Result<(), ProfileError> {
    if let Some(list) = &spec.one_of {
        if !list.iter().any(|v| v == value) {
            return Err(args_err(format!(
                "argument `{}`: `{value}` is not one of: {}",
                spec.name,
                list.join(", ")
            )));
        }
    }
    if let Some(p) = &spec.pattern {
        let re = full_match(p).map_err(|e| args_err(format!("argument `{}`: {e}", spec.name)))?;
        if !re.is_match(value) {
            return Err(args_err(format!(
                "argument `{}`: `{value}` does not match `{p}`",
                spec.name
            )));
        }
    }
    Ok(())
}

fn render_top(text: &str, specs: &[ArgSpec], values: &Values) -> Result<String, ProfileError> {
    render(text, specs, values).map_err(|name| {
        args_err(format!(
            "argument `{name}` is required: pass --arg {name}=VALUE or declare a default"
        ))
    })
}

fn render(text: &str, specs: &[ArgSpec], values: &Values) -> Result<String, String> {
    let mut out = String::new();
    let mut floor = 0;
    let mut i = 0;
    while i < text.len() {
        let rest = &text[i..];
        if rest.starts_with("{?") {
            if let Some(end) = matching_close(rest) {
                let inner = &rest[2..end];
                let inner = inner.strip_prefix(' ').unwrap_or(inner);
                let after = &rest[end + 1..];
                match render(inner, specs, values) {
                    Ok(s) => {
                        out.push_str(&s);
                        floor = out.len();
                    }
                    Err(_) => {
                        if after.is_empty() || after.starts_with(char::is_whitespace) {
                            out.truncate(out.trim_end().len().max(floor));
                        }
                    }
                }
                i += end + 1;
                continue;
            }
        } else if let Some(body) = rest.strip_prefix('{') {
            if let Some(close) = body.find('}') {
                let name = &body[..close];
                if specs.iter().any(|s| s.name == name) {
                    match values.get(name) {
                        Some(v) => {
                            out.push_str(v);
                            floor = out.len();
                        }
                        None => return Err(name.to_string()),
                    }
                    i += close + 2;
                    continue;
                }
            }
        }
        let ch = rest.chars().next().unwrap_or_default();
        out.push(ch);
        i += ch.len_utf8();
    }
    Ok(out)
}

fn matching_close(rest: &str) -> Option<usize> {
    let mut depth = 0usize;
    for (at, ch) in rest.char_indices() {
        match ch {
            '{' => depth += 1,
            '}' => {
                depth -= 1;
                if depth == 0 {
                    return Some(at);
                }
            }
            _ => {}
        }
    }
    None
}
