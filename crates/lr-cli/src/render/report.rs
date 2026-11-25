use super::{Cell, Out, Tone};

/// How a line of a report ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Level {
    /// The check passed.
    Ok,
    /// Needs a look; the command still works.
    Warn,
    /// Wrong.
    Fail,
    /// A fact, no verdict.
    Info,
}

impl Level {
    fn marker(self) -> (&'static str, Tone) {
        match self {
            Level::Ok => ("ok", Tone::Ok),
            Level::Warn => ("warn", Tone::Warn),
            Level::Fail => ("fail", Tone::Bad),
            Level::Info => ("info", Tone::Dim),
        }
    }
}

/// A report: one line per check or fact with a colored marker, then one summary line.
#[derive(Default)]
pub struct Report {
    items: Vec<(Level, String)>,
}

impl Report {
    /// An empty report.
    pub fn new() -> Report {
        Report::default()
    }

    /// Adds a line; text after a line break goes on continuation lines under the text.
    pub fn line(&mut self, level: Level, text: impl Into<String>) {
        self.items.push((level, text.into()));
    }

    /// How many lines have `level`.
    pub fn count(&self, level: Level) -> usize {
        self.items.iter().filter(|(l, _)| *l == level).count()
    }

    /// The marked lines, then `N ok, N warn, N fail` (a count of zero is left out; nothing but
    /// facts gives no summary).
    pub fn lines(&self, out: &Out) -> Vec<String> {
        let mut lines = Vec::new();
        for (level, text) in &self.items {
            let (word, tone) = level.marker();
            let mut parts = text.lines();
            let first = parts.next().unwrap_or("");
            lines.push(format!(
                "{}{}  {first}",
                out.paint(word, tone),
                " ".repeat(4 - word.len())
            ));
            for rest in parts {
                lines.push(format!("      {rest}"));
            }
        }
        let counted: Vec<Cell> = [Level::Ok, Level::Warn, Level::Fail]
            .into_iter()
            .filter(|l| self.count(*l) > 0)
            .map(|l| {
                let (word, tone) = l.marker();
                Cell::new(format!("{} {word}", self.count(l)), tone)
            })
            .collect();
        if !counted.is_empty() {
            let text: Vec<String> = counted.iter().map(|c| out.paint(&c.text, c.tone)).collect();
            lines.push(text.join(", "));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_report_marks_every_line_and_ends_with_the_counts() {
        let mut r = Report::new();
        r.line(Level::Ok, "state directory");
        r.line(Level::Warn, "broker is older\nrun: broker update-service");
        r.line(Level::Info, "os: windows");
        assert_eq!(
            r.lines(&Out::plain()),
            vec![
                "ok    state directory".to_string(),
                "warn  broker is older".to_string(),
                "      run: broker update-service".to_string(),
                "info  os: windows".to_string(),
                "1 ok, 1 warn".to_string(),
            ]
        );
        let styled = r.lines(&Out::styled(80));
        assert!(styled[0].starts_with("\x1b[32mok\x1b[0m"), "{styled:?}");
        assert!(styled[1].starts_with("\x1b[33mwarn\x1b[0m"), "{styled:?}");
    }

    #[test]
    fn only_facts_give_no_summary() {
        let mut r = Report::new();
        r.line(Level::Info, "a");
        assert_eq!(r.lines(&Out::plain()), vec!["info  a".to_string()]);
    }
}
