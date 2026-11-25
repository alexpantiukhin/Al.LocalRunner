use super::text::{chars, shorten};
use super::{Cell, Out, Tone};

const GAP: usize = 2;
const MIN_WIDTH: usize = 6;

struct Column {
    header: String,
    right: bool,
    fixed: bool,
}

/// A table: a header row and one row per item, columns aligned, the whole fitted to the width
/// of the terminal by shortening the widest columns.
pub struct Table {
    columns: Vec<Column>,
    rows: Vec<Vec<Cell>>,
}

impl Table {
    /// An empty table with these headers.
    pub fn new(headers: &[&str]) -> Table {
        Table {
            columns: headers
                .iter()
                .map(|h| Column {
                    header: (*h).to_string(),
                    right: false,
                    fixed: false,
                })
                .collect(),
            rows: Vec::new(),
        }
    }

    /// Aligns column `index` to the right (numbers).
    pub fn right(mut self, index: usize) -> Table {
        if let Some(c) = self.columns.get_mut(index) {
            c.right = true;
        }
        self
    }

    /// Never shortens column `index` (ids, states, versions).
    pub fn fixed(mut self, index: usize) -> Table {
        if let Some(c) = self.columns.get_mut(index) {
            c.fixed = true;
        }
        self
    }

    /// Adds a row; missing cells are empty, extra ones dropped.
    pub fn row(&mut self, mut cells: Vec<Cell>) {
        cells.resize(self.columns.len(), Cell::default());
        self.rows.push(cells);
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    fn widths(&self, out: &Out) -> Vec<usize> {
        let mut widths: Vec<usize> = self
            .columns
            .iter()
            .enumerate()
            .map(|(i, c)| {
                self.rows
                    .iter()
                    .map(|r| chars(&r[i].text))
                    .chain([chars(&c.header)])
                    .max()
                    .unwrap_or(0)
            })
            .collect();
        let Some(limit) = out.width() else {
            return widths;
        };
        let gaps = GAP * widths.len().saturating_sub(1);
        let mut total = widths.iter().sum::<usize>() + gaps;
        while total > limit {
            let widest = (0..widths.len())
                .filter(|&i| !self.columns[i].fixed)
                .filter(|&i| widths[i] > MIN_WIDTH.max(chars(&self.columns[i].header)))
                .max_by_key(|&i| widths[i]);
            let Some(i) = widest else {
                break;
            };
            widths[i] -= 1;
            total -= 1;
        }
        widths
    }

    /// The lines of the table: the header, then the rows. Nothing at all when there are no
    /// rows. The last column has no trailing spaces.
    pub fn lines(&self, out: &Out) -> Vec<String> {
        if self.rows.is_empty() {
            return Vec::new();
        }
        let widths = self.widths(out);
        let last = widths.len().saturating_sub(1);
        let line = |cells: Vec<(String, Tone)>| -> String {
            let mut text = String::new();
            for (i, (cell, tone)) in cells.into_iter().enumerate() {
                let shown = match out.width() {
                    Some(_) => shorten(&cell, widths[i]),
                    None => cell,
                };
                let pad = " ".repeat(widths[i].saturating_sub(chars(&shown)));
                let column = &self.columns[i];
                match (column.right, i == last) {
                    (true, _) => {
                        text.push_str(&pad);
                        text.push_str(&out.paint(&shown, tone));
                    }
                    (false, true) => text.push_str(&out.paint(&shown, tone)),
                    (false, false) => {
                        text.push_str(&out.paint(&shown, tone));
                        text.push_str(&pad);
                    }
                }
                if i != last {
                    text.push_str(&" ".repeat(GAP));
                }
            }
            text.trim_end().to_string()
        };
        let mut lines = vec![line(
            self.columns
                .iter()
                .map(|c| (c.header.clone(), Tone::Head))
                .collect(),
        )];
        for row in &self.rows {
            lines.push(line(row.iter().map(|c| (c.text.clone(), c.tone)).collect()));
        }
        lines
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sample() -> Table {
        let mut t = Table::new(&["NAME", "STATE", "MEM"]).right(2).fixed(1);
        t.row(vec!["alpha".into(), Cell::ok("online"), "4096".into()]);
        t.row(vec![
            "a-very-long-runner-name".into(),
            Cell::bad("offline"),
            "8".into(),
        ]);
        t
    }

    #[test]
    fn plain_output_aligns_columns_and_shortens_nothing() {
        let lines = sample().lines(&Out::plain());
        assert_eq!(
            lines,
            vec![
                "NAME                     STATE     MEM".to_string(),
                "alpha                    online   4096".to_string(),
                "a-very-long-runner-name  offline     8".to_string(),
            ]
        );
    }

    #[test]
    fn a_narrow_terminal_shortens_the_widest_flexible_column_only() {
        let mut out = sample().lines(&Out::styled(30));
        for l in &mut out {
            *l = strip(l);
        }
        assert_eq!(out[2], "a-very-long-ru…  offline     8");
        assert!(out.iter().all(|l| l.chars().count() <= 30), "{out:?}");
        assert!(out[1].starts_with("alpha"), "{out:?}");
    }

    fn strip(text: &str) -> String {
        let mut out = String::new();
        let mut skip = false;
        for c in text.chars() {
            match (skip, c) {
                (_, '\x1b') => skip = true,
                (true, 'm') => skip = false,
                (true, _) => {}
                (false, c) => out.push(c),
            }
        }
        out
    }

    #[test]
    fn colors_wrap_the_text_and_never_change_the_alignment() {
        let styled: Vec<String> = sample()
            .lines(&Out::styled(200))
            .iter()
            .map(|l| strip(l))
            .collect();
        assert_eq!(styled, sample().lines(&Out::plain()));
        let raw = sample().lines(&Out::styled(200));
        assert!(raw[1].contains("\x1b[32monline\x1b[0m"), "{raw:?}");
        assert!(raw[2].contains("\x1b[31moffline\x1b[0m"), "{raw:?}");
        assert!(raw[0].contains("\x1b[1mNAME\x1b[0m"), "{raw:?}");
    }

    #[test]
    fn a_table_without_rows_prints_nothing_and_short_rows_are_padded() {
        assert!(Table::new(&["A", "B"]).lines(&Out::plain()).is_empty());
        let mut t = Table::new(&["A", "B"]);
        t.row(vec!["x".into()]);
        assert_eq!(
            t.lines(&Out::plain()),
            vec!["A  B".to_string(), "x".to_string()]
        );
    }

    #[test]
    fn a_line_break_inside_a_cell_is_a_space() {
        let mut t = Table::new(&["A"]);
        t.row(vec![Cell::plain("one\ntwo")]);
        assert_eq!(t.lines(&Out::plain())[1], "one two");
    }
}
