use super::text::chars;
use super::{Cell, Out, Tone};

/// The details of one item: a label and a value per line, the labels dimmed and aligned.
#[derive(Default)]
pub struct Card {
    rows: Vec<(String, Cell)>,
}

impl Card {
    /// An empty card.
    pub fn new() -> Card {
        Card::default()
    }

    /// Adds a line.
    pub fn row(&mut self, label: &str, value: impl Into<Cell>) {
        self.rows.push((label.to_string(), value.into()));
    }

    /// Adds a line unless `value` is `None`.
    pub fn row_opt(&mut self, label: &str, value: Option<impl Into<Cell>>) {
        if let Some(v) = value {
            self.row(label, v);
        }
    }

    /// The lines, the values of the longest label starting in the same column.
    pub fn lines(&self, out: &Out) -> Vec<String> {
        let width = self.rows.iter().map(|(l, _)| chars(l)).max().unwrap_or(0);
        self.rows
            .iter()
            .map(|(label, value)| {
                let pad = " ".repeat(width - chars(label));
                format!(
                    "{}{pad}  {}",
                    out.paint(label, Tone::Dim),
                    out.paint(&value.text, value.tone)
                )
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn labels_are_aligned_and_a_missing_value_is_skipped() {
        let mut c = Card::new();
        c.row("name", "box");
        c.row_opt("zone", None::<&str>);
        c.row("state", Cell::ok("online"));
        assert_eq!(
            c.lines(&Out::plain()),
            vec!["name   box".to_string(), "state  online".to_string()]
        );
        assert!(c.lines(&Out::styled(80))[1].contains("\x1b[32monline"));
    }
}
