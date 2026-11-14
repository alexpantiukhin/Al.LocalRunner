use super::file::PolicyFile;
use super::rules::display_name;

/// One line per rule of `file` for `policy check`: its number, name, line, condition and the
/// fields it decides.
pub fn explain_rules(file: &PolicyFile) -> Vec<String> {
    file.rules
        .iter()
        .enumerate()
        .map(|(i, entry)| {
            let rule = &entry.rule;
            let mut head = format!("rule {}", i + 1);
            if rule.name.is_some() {
                head = format!("{head} {}", display_name(rule, i));
            }
            if let Some(line) = entry.line {
                head = format!("{head} (line {line})");
            }
            let mut parts = vec![format!("when {}", rule.when.describe())];
            if let Some(p) = rule.priority {
                parts.push(format!("priority {p}"));
            }
            if let Some(r) = rule.reserve {
                parts.push(format!("reserve cpu {}, mem_mb {}", r.cpu, r.mem_mb));
            }
            if let Some(q) = rule.quota {
                let mut set = Vec::new();
                if let Some(n) = q.max_running {
                    set.push(format!("max_running {n}"));
                }
                if let Some(n) = q.max_mem_mb {
                    set.push(format!("max_mem_mb {n}"));
                }
                parts.push(format!("quota {}", set.join(", ")));
            }
            if let Some(w) = rule.weight {
                parts.push(format!("weight {w}"));
            }
            format!("{head}: {}", parts.join("; "))
        })
        .collect()
}
