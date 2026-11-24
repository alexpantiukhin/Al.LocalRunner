use super::value::parse_value;
use toml_edit::{Decor, DocumentMut, Item, Key, Table, TableLike};

/// A TOML document edited in place: comments, order and formatting survive a change.
pub struct ConfigDoc {
    doc: DocumentMut,
}

fn split_key(key: &str) -> Result<Vec<String>, String> {
    let keys = Key::parse(key).map_err(|e| format!("invalid key `{key}`: {e}"))?;
    Ok(keys.iter().map(|k| k.get().to_string()).collect())
}

fn find<'a>(table: &'a dyn TableLike, path: &[String]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    let item = table.get(first)?;
    if rest.is_empty() {
        return Some(item);
    }
    find(item.as_table_like()?, rest)
}

fn decor_has_comment(decor: &Decor) -> bool {
    [decor.prefix(), decor.suffix()]
        .into_iter()
        .flatten()
        .filter_map(|raw| raw.as_str())
        .any(|text| text.contains('#'))
}

fn has_comment(table: &dyn TableLike, key: &str) -> bool {
    let own = match table.get(key) {
        Some(Item::Table(t)) => decor_has_comment(t.decor()),
        Some(Item::Value(v)) => decor_has_comment(v.decor()),
        _ => false,
    };
    own || table
        .key(key)
        .is_some_and(|k| decor_has_comment(k.leaf_decor()))
}

fn remove(table: &mut dyn TableLike, path: &[String]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return table.remove(first).is_some();
    }
    let Some(child) = table.get_mut(first).and_then(Item::as_table_like_mut) else {
        return false;
    };
    let removed = remove(child, rest);
    let prune = removed && child.is_empty() && !has_comment(table, first);
    if prune {
        table.remove(first);
    }
    removed
}

impl ConfigDoc {
    /// Parses the text of a TOML file.
    pub fn parse(text: &str) -> Result<ConfigDoc, String> {
        let doc = text
            .parse::<DocumentMut>()
            .map_err(|e| format!("invalid TOML: {e}"))?;
        Ok(ConfigDoc { doc })
    }

    /// Renders the document back to text.
    pub fn render(&self) -> String {
        self.doc.to_string()
    }

    /// The underlying document, for edits the typed methods do not cover.
    pub fn inner_mut(&mut self) -> &mut DocumentMut {
        &mut self.doc
    }

    /// The TOML text of the value at the dotted `key`, or `None` when absent or not a value.
    pub fn get(&self, key: &str) -> Option<String> {
        let path = split_key(key).ok()?;
        let value = find(self.doc.as_table(), &path)?.as_value()?;
        let mut value = value.clone();
        value.decor_mut().clear();
        Some(value.to_string())
    }

    /// Sets the value at the dotted `key`, creating missing tables.
    pub fn set(&mut self, key: &str, value: &str, force_string: bool) -> Result<(), String> {
        let path = split_key(key)?;
        let (last, parents) = path
            .split_last()
            .ok_or_else(|| format!("empty key `{key}`"))?;
        let mut table: &mut dyn TableLike = self.doc.as_table_mut();
        for segment in parents {
            if !table.contains_key(segment) {
                let mut created = Table::new();
                created.set_implicit(true);
                table.insert(segment, Item::Table(created));
            }
            table = table
                .get_mut(segment)
                .and_then(Item::as_table_like_mut)
                .ok_or_else(|| format!("`{segment}` in `{key}` is not a table"))?;
        }
        if table.get(last).is_some_and(|i| !i.is_value()) {
            return Err(format!("`{key}` is a table, not a value"));
        }
        let existing = table.get(last).and_then(Item::as_value).cloned();
        let mut new = parse_value(value, existing.as_ref(), force_string)
            .map_err(|expected| format!("`{key}` expects {expected}, got `{value}`"))?;
        if let Some(old) = &existing {
            *new.decor_mut() = old.decor().clone();
        }
        table.insert(last, Item::Value(new));
        Ok(())
    }

    /// Removes the key and the tables it leaves empty, except a table with a comment of its own. True when something was removed.
    pub fn unset(&mut self, key: &str) -> Result<bool, String> {
        let path = split_key(key)?;
        Ok(remove(self.doc.as_table_mut(), &path))
    }
}
