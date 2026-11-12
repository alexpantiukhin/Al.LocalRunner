//! Search for registered secret values: one Aho-Corasick automaton, overlapping matches
//! merged into disjoint spans while they are found, and the cut point of a bounded text.

use aho_corasick::AhoCorasick;
use lr_abstractions::MASK;
use std::collections::VecDeque;

pub(crate) struct Matcher {
    pub(crate) generation: u64,
    automaton: Option<AhoCorasick>,
    values: Vec<String>,
    pub(crate) longest: usize,
}

const MATCH_BUDGET: usize = 65_536;

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
    c.is_whitespace()
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
    pub(crate) fn build(order: &VecDeque<String>, generation: u64) -> Matcher {
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

    pub(crate) fn spans(&self, text: &str) -> Vec<(usize, usize)> {
        self.spans_counted(text).0
    }

    fn spans_counted(&self, text: &str) -> (Vec<(usize, usize)>, usize) {
        let mut examined = 0usize;
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
            examined = all.len();
            for (start, end) in all {
                merge(&mut merged, start, end);
            }
            return (merged, examined);
        };
        for (n, m) in ac.find_overlapping_iter(text).enumerate() {
            examined = n + 1;
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
        (merged, examined)
    }

    pub(crate) fn cut(&self, text: &str, max: usize) -> Option<usize> {
        self.cut_counted(text, max).0
    }

    fn cut_counted(&self, text: &str, max: usize) -> (Option<usize>, usize) {
        let mut steps = 0usize;
        let Some(mut at) = max.checked_add(self.longest) else {
            return (None, steps);
        };
        if text.len() <= at {
            return (None, steps);
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
        let mut i = spans.partition_point(|(s, _)| *s < at);
        loop {
            let before = at;
            while i > 0 && spans[i - 1].0 >= at {
                i -= 1;
                steps += 1;
            }
            if i > 0 {
                steps += 1;
                let (start, end) = spans[i - 1];
                if at < end {
                    at = start;
                }
            }
            at = token_start(text, at);
            if at == before {
                return (Some(at), steps);
            }
        }
    }

    pub(crate) fn mask(&self, text: &str) -> String {
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cut_steps_stay_linear_in_the_number_of_spans() {
        let order: VecDeque<String> = ["x y".to_string()].into_iter().collect();
        let m = Matcher::build(&order, 1);
        let text = "x yz".repeat(4096);
        let (at, steps) = m.cut_counted(&text, 8 * 1024);
        assert_eq!(at, Some(0));
        let spans = text.len() / 4;
        assert!(steps <= 4 * spans, "steps {steps} for {spans} spans");
    }

    #[test]
    fn overlapping_matches_examined_stay_within_the_budget() {
        let order: VecDeque<String> = (4..516).map(|n| "a".repeat(n)).collect();
        let m = Matcher::build(&order, 1);
        let text = "a".repeat(16 * 1024);
        let (spans, examined) = m.spans_counted(&text);
        assert_eq!(spans, vec![(0, text.len())]);
        assert!(examined <= MATCH_BUDGET + 1, "{examined}");
    }
}
