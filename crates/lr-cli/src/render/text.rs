/// The number of characters of `text` (the width of a column; wide characters count as one).
pub fn chars(text: &str) -> usize {
    text.chars().count()
}

/// `text` of at most `width` characters: longer text is cut and ends with `…`, at a space
/// when the cut would fall inside a word and there is a space to cut at; a single word longer
/// than `width` is cut anyway.
pub fn shorten(text: &str, width: usize) -> String {
    if chars(text) <= width {
        return text.to_string();
    }
    if width == 0 {
        return String::new();
    }
    let kept: Vec<char> = text.chars().take(width - 1).collect();
    let next = text.chars().nth(width - 1);
    let inside_word =
        next.is_some_and(|c| !c.is_whitespace()) && kept.last().is_some_and(|c| !c.is_whitespace());
    let cut = match (inside_word, kept.iter().rposition(|c| c.is_whitespace())) {
        (true, Some(space)) if space > 0 => space,
        _ => kept.len(),
    };
    let head: String = kept[..cut].iter().collect();
    format!("{}…", head.trim_end())
}

fn parts(version: &str) -> Vec<u64> {
    version
        .trim_start_matches('v')
        .split(['.', '-', '+'])
        .map_while(|p| p.parse::<u64>().ok())
        .collect()
}

/// The newest of `versions` (numeric parts compared in order); `None` for none.
pub fn newest_version<'a>(versions: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    versions.into_iter().max_by_key(|v| parts(v))
}

/// The tone of a version shown next to others: a version that is not the newest one known is
/// `Warn`, an unknown one is `Dim`, the newest is plain.
pub fn version_tone(version: Option<&str>, newest: Option<&str>) -> super::Tone {
    match (version, newest) {
        (None, _) => super::Tone::Dim,
        (Some(v), Some(n)) if parts(v) < parts(n) => super::Tone::Warn,
        _ => super::Tone::Plain,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn short_text_is_kept_and_long_text_ends_with_an_ellipsis() {
        assert_eq!(shorten("abc", 3), "abc");
        assert_eq!(shorten("abcdef", 4), "abc…");
        assert_eq!(shorten("abcdef", 1), "…");
        assert_eq!(shorten("abcdef", 0), "");
        assert_eq!(shorten("привет мир", 7), "привет…");
    }

    #[test]
    fn a_cut_never_lands_inside_a_word_while_a_space_can_be_used() {
        assert_eq!(shorten("runner is busy now", 12), "runner is…");
        assert_eq!(shorten("runner is busy now", 9), "runner…");
        assert_eq!(shorten("extraordinarily", 6), "extra…");
    }

    #[test]
    fn versions_are_compared_by_their_numbers() {
        assert_eq!(newest_version(["0.9.0", "0.10.0", "0.9.9"]), Some("0.10.0"));
        assert_eq!(newest_version([]), None);
        let newest = Some("0.10.0");
        assert_eq!(
            version_tone(Some("0.9.0"), newest),
            super::super::Tone::Warn
        );
        assert_eq!(
            version_tone(Some("0.10.0"), newest),
            super::super::Tone::Plain
        );
        assert_eq!(version_tone(None, newest), super::super::Tone::Dim);
        assert_eq!(
            version_tone(Some("v0.10.0"), newest),
            super::super::Tone::Plain
        );
    }
}
