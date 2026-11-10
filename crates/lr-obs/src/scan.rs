use lr_abstractions::MASK;

const JOIN_PREFIX: &str = "lrj1.";

fn is_word(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '_' | '-' | '.')
}

fn token_char(c: char) -> bool {
    c.is_ascii_alphanumeric() || matches!(c, '-' | '.' | '_' | '~' | '+' | '/' | '=')
}

fn prev_char(s: &str, at: usize) -> Option<char> {
    s[..at].chars().next_back()
}

fn take_while(s: &str, from: usize, f: impl Fn(char) -> bool) -> usize {
    s[from..]
        .char_indices()
        .find(|(_, c)| !f(*c))
        .map_or(s.len(), |(i, _)| from + i)
}

pub(crate) fn private_keys(s: &str) -> String {
    let marker = "-----BEGIN ";
    let mut out = String::with_capacity(s.len());
    let mut rest = s;
    while let Some(i) = rest.find(marker) {
        let after = &rest[i + marker.len()..];
        let line_end = after.find('\n').unwrap_or(after.len());
        let header_len = after[..line_end].find("-----").unwrap_or(line_end);
        if !after[..header_len].contains("PRIVATE KEY") {
            out.push_str(&rest[..i + marker.len()]);
            rest = after;
            continue;
        }
        out.push_str(&rest[..i]);
        out.push_str(MASK);
        let body = &after[header_len..];
        rest = match body.find("-----END ") {
            Some(e) => {
                let tail = &body[e + "-----END ".len()..];
                let close = tail.find('\n').unwrap_or(tail.len());
                match tail[..close].find("-----") {
                    Some(t) => &tail[t + 5..],
                    None => &tail[close..],
                }
            }
            None => "",
        };
    }
    out.push_str(rest);
    out
}

pub(crate) fn join_codes(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut from = 0;
    let mut search = 0;
    while let Some(rel) = s[search..].find(JOIN_PREFIX) {
        let at = search + rel;
        let end = take_while(s, at + JOIN_PREFIX.len(), is_word);
        if prev_char(s, at).is_some_and(is_word) || end == at + JOIN_PREFIX.len() {
            search = at + JOIN_PREFIX.len();
            continue;
        }
        out.push_str(&s[from..at]);
        out.push_str(MASK);
        from = end;
        search = end;
    }
    out.push_str(&s[from..]);
    out
}

pub(crate) fn url_credentials(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    let mut from = 0;
    let mut search = 0;
    while let Some(rel) = s[search..].find("://") {
        let start = search + rel + 3;
        let end = take_while(s, start, |c| {
            !(c.is_whitespace() || matches!(c, '/' | '?' | '#' | '"' | '\''))
        });
        search = end.max(start);
        if let Some(at) = s[start..end].rfind('@') {
            out.push_str(&s[from..start]);
            out.push_str(MASK);
            from = start + at;
        }
    }
    out.push_str(&s[from..]);
    out
}

fn is_basic_credential(token: &str) -> bool {
    token.len() >= 8
        && token.len().is_multiple_of(4)
        && token
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '+' | '/' | '='))
}

pub(crate) fn auth_schemes(s: &str) -> String {
    let lower = s.to_ascii_lowercase();
    let mut out = String::with_capacity(s.len());
    let mut from = 0;
    let mut i = 0;
    while i < s.len() {
        let scheme = if lower[i..].starts_with("bearer") {
            Some(("bearer", false))
        } else if s[i..].starts_with("Basic") {
            Some(("basic", true))
        } else {
            None
        };
        let Some((word, basic)) = scheme else {
            i += s[i..].chars().next().map_or(1, char::len_utf8);
            continue;
        };
        let after_word = i + word.len();
        let spaces = take_while(s, after_word, |c| c == ' ');
        let end = take_while(s, spaces, token_char);
        let bounded = !prev_char(s, i).is_some_and(|c| c.is_ascii_alphanumeric());
        let token = &s[spaces..end];
        if bounded
            && spaces > after_word
            && !token.is_empty()
            && (!basic || is_basic_credential(token))
        {
            out.push_str(&s[from..spaces]);
            out.push_str(MASK);
            from = end;
            i = end;
        } else {
            i = after_word;
        }
    }
    out.push_str(&s[from..]);
    out
}

fn closing_quote(s: &str, from: usize, quote: char) -> Option<usize> {
    let mut escaped = false;
    for (i, c) in s[from..].char_indices() {
        if escaped {
            escaped = false;
        } else if c == '\\' {
            escaped = true;
        } else if c == quote {
            return Some(from + i);
        }
    }
    None
}

fn closing_bracket(s: &str, from: usize) -> usize {
    let mut depth = 0usize;
    let mut i = from;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap_or(' ');
        match c {
            '{' | '[' => depth += 1,
            '}' | ']' => {
                depth = depth.saturating_sub(1);
                if depth == 0 {
                    return i + 1;
                }
            }
            '"' | '\'' => match closing_quote(s, i + 1, c) {
                Some(q) => i = q,
                None => return s.len(),
            },
            _ => {}
        }
        i += c.len_utf8();
    }
    s.len()
}

#[derive(Clone, Copy, PartialEq)]
enum Until {
    Space,
    LineEnd,
}

fn escaped_quote(s: &str, at: usize) -> Option<&str> {
    let slashes = s[at..].len() - s[at..].trim_start_matches('\\').len();
    let quote = s[at + slashes..].chars().next()?;
    (slashes > 0 && matches!(quote, '"' | '\'')).then(|| &s[at..at + slashes + 1])
}

fn value_span(s: &str, start: usize, until: Until) -> Option<(usize, usize, usize)> {
    if let Some(open) = escaped_quote(s, start) {
        let inner = start + open.len();
        return match s[inner..].find(open) {
            Some(rel) => Some((inner, inner + rel, inner + rel + open.len())),
            None => Some((inner, s.len(), s.len())),
        };
    }
    let first = s[start..].chars().next()?;
    match first {
        '"' | '\'' => {
            let inner = start + 1;
            match closing_quote(s, inner, first) {
                Some(close) => Some((inner, close, close + 1)),
                None => Some((inner, s.len(), s.len())),
            }
        }
        '{' | '[' => {
            let end = closing_bracket(s, start);
            Some((start, end, end))
        }
        _ => {
            let end = match until {
                Until::Space => take_while(s, start, |c| {
                    !(c.is_whitespace()
                        || matches!(c, ',' | ';' | '&' | ')' | '}' | ']' | '"' | '\''))
                }),
                Until::LineEnd => take_while(s, start, |c| !matches!(c, '\n' | '\r')),
            };
            let end = s[start..end].trim_end().len() + start;
            (end > start).then_some((start, end, end))
        }
    }
}

fn is_authorization(word: &str) -> bool {
    word.eq_ignore_ascii_case("auth") || segments(word).iter().any(|s| s == "authorization")
}

pub(crate) fn key_values(s: &str, sensitive: impl Fn(&str) -> bool) -> String {
    let mut out = String::with_capacity(s.len());
    let mut from = 0;
    let mut i = 0;
    while i < s.len() {
        let c = s[i..].chars().next().unwrap_or(' ');
        if !is_word(c) {
            i += c.len_utf8();
            continue;
        }
        let end = take_while(s, i, is_word);
        let word = &s[i..end];
        i = end;
        if !sensitive(word) {
            continue;
        }
        let mut j = end;
        if let Some(close) = escaped_quote(s, j) {
            j += close.len();
        } else if s[j..].starts_with(['"', '\'']) {
            j += 1;
        }
        j = take_while(s, j, |c| c == ' ' || c == '\t');
        if !s[j..].starts_with(['=', ':']) || s[j..].starts_with("://") {
            continue;
        }
        let until = if is_authorization(word) || s[j..].starts_with(':') {
            Until::LineEnd
        } else {
            Until::Space
        };
        j = take_while(s, j + 1, |c| c == ' ' || c == '\t');
        if let Some((mask_from, mask_to, resume)) = value_span(s, j, until) {
            out.push_str(&s[from..mask_from]);
            out.push_str(MASK);
            from = mask_to;
            i = resume;
        }
    }
    out.push_str(&s[from..]);
    out
}

pub(crate) fn segments(name: &str) -> Vec<String> {
    let chars: Vec<char> = name.chars().collect();
    let mut parts = Vec::new();
    let mut cur = String::new();
    for (i, &c) in chars.iter().enumerate() {
        if !c.is_alphanumeric() {
            if !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
            continue;
        }
        if let Some(&p) = i.checked_sub(1).and_then(|j| chars.get(j)) {
            let next_lower = chars.get(i + 1).is_some_and(|n| n.is_lowercase());
            let split = p.is_alphanumeric()
                && ((p.is_lowercase() && c.is_uppercase())
                    || (p.is_alphabetic() != c.is_alphabetic())
                    || (p.is_uppercase() && c.is_uppercase() && next_lower));
            if split && !cur.is_empty() {
                parts.push(std::mem::take(&mut cur));
            }
        }
        cur.extend(c.to_lowercase());
    }
    if !cur.is_empty() {
        parts.push(cur);
    }
    parts
}
