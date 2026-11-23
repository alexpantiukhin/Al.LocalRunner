use super::value::parse_value;
use toml_edit::{DocumentMut, Item, Key, Table};

/// A TOML document edited in place: comments, order and formatting survive a change.
pub struct ConfigDoc {
    doc: DocumentMut,
}

fn split_key(key: &str) -> Result<Vec<String>, String> {
    let keys = Key::parse(key).map_err(|e| format!("invalid key `{key}`: {e}"))?;
    Ok(keys.iter().map(|k| k.get().to_string()).collect())
}

fn find<'a>(table: &'a Table, path: &[String]) -> Option<&'a Item> {
    let (first, rest) = path.split_first()?;
    let item = table.get(first)?;
    if rest.is_empty() {
        return Some(item);
    }
    find(item.as_table()?, rest)
}

fn remove(table: &mut Table, path: &[String]) -> bool {
    let Some((first, rest)) = path.split_first() else {
        return false;
    };
    if rest.is_empty() {
        return table.remove(first).is_some();
    }
    let Some(child) = table.get_mut(first).and_then(Item::as_table_mut) else {
        return false;
    };
    let removed = remove(child, rest);
    if removed && child.is_empty() {
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
        let mut table = self.doc.as_table_mut();
        for segment in parents {
            if !table.contains_key(segment) {
                let mut created = Table::new();
                created.set_implicit(true);
                table.insert(segment, Item::Table(created));
            }
            table = table
                .get_mut(segment)
                .and_then(Item::as_table_mut)
                .ok_or_else(|| format!("`{segment}` in `{key}` is not a table"))?;
        }
        let existing = table.get(last).and_then(Item::as_value);
        if table.get(last).is_some_and(|i| !i.is_value()) {
            return Err(format!("`{key}` is a table, not a value"));
        }
        let mut new = parse_value(value, existing, force_string);
        if let Some(old) = existing {
            *new.decor_mut() = old.decor().clone();
        }
        table.insert(last, Item::Value(new));
        Ok(())
    }

    /// Removes the key and the tables it leaves empty. True when something was removed.
    pub fn unset(&mut self, key: &str) -> Result<bool, String> {
        let path = split_key(key)?;
        Ok(remove(self.doc.as_table_mut(), &path))
    }
}
