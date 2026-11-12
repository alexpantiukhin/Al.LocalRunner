use serde_json::{Map, Value};

struct Line {
    indent: usize,
    text: String,
    number: usize,
}

pub fn parse(text: &str) -> Result<Value, String> {
    let mut lines = Vec::new();
    for (i, raw) in text.lines().enumerate() {
        let number = i + 1;
        if raw.contains('\t') {
            return Err(format!("line {number}: tab character"));
        }
        let trimmed = raw.trim_end();
        let content = trimmed.trim_start();
        if content.is_empty() || content.starts_with('#') {
            continue;
        }
        lines.push(Line {
            indent: trimmed.len() - content.len(),
            text: content.to_string(),
            number,
        });
    }
    if lines.is_empty() {
        return Ok(Value::Null);
    }
    let mut at = 0;
    let indent = lines[0].indent;
    let value = block(&mut lines, &mut at, indent)?;
    if at < lines.len() {
        return Err(format!("line {}: unexpected indentation", lines[at].number));
    }
    Ok(value)
}

fn is_item(text: &str) -> bool {
    text == "-" || text.starts_with("- ")
}

fn block(lines: &mut [Line], at: &mut usize, indent: usize) -> Result<Value, String> {
    if is_item(&lines[*at].text) {
        sequence(lines, at, indent)
    } else {
        mapping(lines, at, indent)
    }
}

fn split_key(text: &str) -> Option<(String, String)> {
    if text.starts_with(['"', '\'', '[', '{']) {
        return None;
    }
    if let Some(key) = text.strip_suffix(':') {
        return Some((key.to_string(), String::new()));
    }
    let pos = text.find(": ")?;
    Some((text[..pos].to_string(), text[pos + 2..].trim().to_string()))
}

fn mapping(lines: &mut [Line], at: &mut usize, indent: usize) -> Result<Value, String> {
    let mut map = Map::new();
    while *at < lines.len() && lines[*at].indent == indent && !is_item(&lines[*at].text) {
        let number = lines[*at].number;
        let (key, rest) =
            split_key(&lines[*at].text).ok_or(format!("line {number}: expected `key: value`"))?;
        *at += 1;
        let value = if !rest.is_empty() {
            scalar_or_flow(&rest).map_err(|e| format!("line {number}: {e}"))?
        } else if *at < lines.len() && lines[*at].indent > indent {
            let inner = lines[*at].indent;
            block(lines, at, inner)?
        } else if *at < lines.len() && lines[*at].indent == indent && is_item(&lines[*at].text) {
            sequence(lines, at, indent)?
        } else {
            Value::Null
        };
        if map.insert(key.clone(), value).is_some() {
            return Err(format!("line {number}: duplicate key `{key}`"));
        }
    }
    if *at < lines.len() && lines[*at].indent > indent {
        return Err(format!(
            "line {}: unexpected indentation",
            lines[*at].number
        ));
    }
    Ok(Value::Object(map))
}

fn sequence(lines: &mut [Line], at: &mut usize, indent: usize) -> Result<Value, String> {
    let mut items = Vec::new();
    while *at < lines.len() && lines[*at].indent == indent && is_item(&lines[*at].text) {
        let number = lines[*at].number;
        let rest = lines[*at].text[1..].trim_start().to_string();
        if rest.is_empty() {
            *at += 1;
            if *at >= lines.len() || lines[*at].indent <= indent {
                items.push(Value::Null);
                continue;
            }
            let inner = lines[*at].indent;
            items.push(block(lines, at, inner)?);
        } else if split_key(&rest).is_some() {
            let inner = indent + (lines[*at].text.len() - rest.len());
            lines[*at].indent = inner;
            lines[*at].text = rest;
            items.push(mapping(lines, at, inner)?);
        } else {
            *at += 1;
            items.push(scalar_or_flow(&rest).map_err(|e| format!("line {number}: {e}"))?);
        }
    }
    Ok(Value::Array(items))
}

fn scalar_or_flow(text: &str) -> Result<Value, String> {
    if text.starts_with(['|', '>', '&', '*', '!']) {
        return Err(format!("unsupported construct `{text}`"));
    }
    if text == "{}" {
        return Ok(Value::Object(Map::new()));
    }
    if let Some(inner) = text.strip_prefix('[') {
        let inner = inner.strip_suffix(']').ok_or("unclosed `[`")?;
        let mut items = Vec::new();
        for part in split_flow(inner)? {
            let part = part.trim();
            if part.is_empty() {
                return Err("empty flow item".into());
            }
            items.push(scalar(part)?);
        }
        return Ok(Value::Array(items));
    }
    if text.starts_with('{') {
        return Err(format!("unsupported flow mapping `{text}`"));
    }
    scalar(text)
}

fn split_flow(text: &str) -> Result<Vec<String>, String> {
    let mut parts = Vec::new();
    let mut current = String::new();
    let mut quote = None;
    for c in text.chars() {
        match (quote, c) {
            (None, '"' | '\'') => {
                quote = Some(c);
                current.push(c);
            }
            (Some(q), _) if c == q => {
                quote = None;
                current.push(c);
            }
            (None, ',') => parts.push(std::mem::take(&mut current)),
            (None, '[' | ']' | '{' | '}') => return Err("nested flow collection".into()),
            _ => current.push(c),
        }
    }
    if quote.is_some() {
        return Err("unclosed quote".into());
    }
    if !text.trim().is_empty() {
        parts.push(current);
    }
    Ok(parts)
}

fn scalar(text: &str) -> Result<Value, String> {
    for q in ['"', '\''] {
        if let Some(inner) = text.strip_prefix(q) {
            let inner = inner.strip_suffix(q).ok_or("unclosed quote")?;
            if inner.contains(q) || (q == '"' && inner.contains('\\')) {
                return Err(format!("unsupported escape in `{text}`"));
            }
            return Ok(Value::String(inner.to_string()));
        }
    }
    if text.contains(" #") || text.contains(": ") {
        return Err(format!("ambiguous plain scalar `{text}`"));
    }
    Ok(Value::String(text.to_string()))
}

#[test]
fn the_subset_parser_reads_mappings_sequences_and_flow_values() {
    let doc = "a:\n  b: [x, \"y z\"]\n  c:\n    - p: 1\n      q: two\n    - plain\n  d: {}\nlist:\n- one\n";
    let v = parse(doc).unwrap();
    assert_eq!(v["a"]["b"], serde_json::json!(["x", "y z"]));
    assert_eq!(v["a"]["c"][0]["q"], "two");
    assert_eq!(v["a"]["c"][1], "plain");
    assert_eq!(v["a"]["d"], serde_json::json!({}));
    assert_eq!(v["list"], serde_json::json!(["one"]));
}

#[test]
fn the_subset_parser_refuses_broken_documents() {
    for bad in [
        "a: 1\na: 2\n",
        "a:\n\tb: 1\n",
        "a: [x, y\n",
        "a: \"open\n",
        "a: 1\n   b: 2\n",
        "just text\n",
        "a: |\n  x\n",
    ] {
        assert!(parse(bad).is_err(), "accepted: {bad:?}");
    }
}
