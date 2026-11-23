use super::document::ConfigDoc;
use std::collections::BTreeSet;
use toml_edit::{Array, ArrayOfTables, DocumentMut, InlineTable, Item, Table, TableLike, Value};

const OPEN_WHEN_EMPTY: &[&str] = &["services.allow"];

#[derive(Default)]
struct Row {
    label: Option<String>,
    name: Option<String>,
    names: Vec<String>,
    members: Vec<String>,
}

fn not_tables(path: &str) -> String {
    format!("`{path}` is not a list of tables")
}

fn strings(table: &dyn TableLike, key: &str) -> Vec<String> {
    table
        .get(key)
        .and_then(Item::as_array)
        .map(|a| {
            a.iter()
                .filter_map(|v| v.as_str().map(String::from))
                .collect()
        })
        .unwrap_or_default()
}

fn row(table: &dyn TableLike) -> Row {
    let text = |key: &str| table.get(key).and_then(Item::as_str).map(String::from);
    Row {
        label: text("label"),
        name: text("name"),
        names: strings(table, "names"),
        members: strings(table, "members"),
    }
}

fn find<'a>(doc: &'a DocumentMut, path: &str) -> Option<&'a Item> {
    let mut parts = path.split('.');
    let mut item = doc.as_table().get(parts.next()?)?;
    for part in parts {
        item = item.as_table_like()?.get(part)?;
    }
    Some(item)
}

fn rows(doc: &DocumentMut, path: &str) -> Result<Vec<Row>, String> {
    match find(doc, path) {
        None => Ok(Vec::new()),
        Some(Item::ArrayOfTables(tables)) => Ok(tables.iter().map(|t| row(t)).collect()),
        Some(Item::Value(Value::Array(array))) => array
            .iter()
            .map(|v| {
                v.as_inline_table()
                    .map(|t| row(t))
                    .ok_or_else(|| not_tables(path))
            })
            .collect(),
        Some(_) => Err(not_tables(path)),
    }
}

enum List<'a> {
    Tables(&'a mut ArrayOfTables),
    Inline(&'a mut Array),
}

impl<'a> List<'a> {
    fn push(self, fields: &[(&str, Value)]) {
        match self {
            List::Tables(tables) => {
                let mut entry = Table::new();
                for (key, v) in fields {
                    entry.insert(key, Item::Value(v.clone()));
                }
                tables.push(entry);
            }
            List::Inline(array) => {
                let mut entry = InlineTable::new();
                for (key, v) in fields {
                    entry.insert(*key, v.clone());
                }
                entry.fmt();
                array.push(entry);
            }
        }
    }

    fn remove(self, index: usize) {
        match self {
            List::Tables(tables) => tables.remove(index),
            List::Inline(array) => remove_at(array, index),
        }
    }

    fn entry(self, index: usize) -> Option<&'a mut dyn TableLike> {
        match self {
            List::Tables(tables) => tables.get_mut(index).map(|t| t as &mut dyn TableLike),
            List::Inline(array) => array
                .get_mut(index)
                .and_then(Value::as_inline_table_mut)
                .map(|t| t as &mut dyn TableLike),
        }
    }
}

fn list_mut<'a>(doc: &'a mut DocumentMut, path: &str) -> Result<List<'a>, String> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts
        .split_last()
        .ok_or_else(|| format!("empty key `{path}`"))?;
    let mut table: &mut dyn TableLike = doc.as_table_mut();
    let mut inline = false;
    for part in parents {
        if !table.contains_key(part) {
            let created = match inline {
                true => Item::Value(Value::InlineTable(InlineTable::new())),
                false => {
                    let mut created = Table::new();
                    created.set_implicit(true);
                    Item::Table(created)
                }
            };
            table.insert(part, created);
        }
        let item = table
            .get_mut(part)
            .ok_or_else(|| format!("`{part}` in `{path}` cannot be created"))?;
        inline = item.is_inline_table();
        table = item
            .as_table_like_mut()
            .ok_or_else(|| format!("`{part}` in `{path}` is not a table"))?;
    }
    if !table.contains_key(last) {
        let created = match inline {
            true => Item::Value(Value::Array(Array::new())),
            false => Item::ArrayOfTables(ArrayOfTables::new()),
        };
        table.insert(last, created);
    }
    match table.get_mut(last) {
        Some(Item::ArrayOfTables(tables)) => Ok(List::Tables(tables)),
        Some(Item::Value(Value::Array(array))) if array.iter().all(Value::is_inline_table) => {
            Ok(List::Inline(array))
        }
        _ => Err(not_tables(path)),
    }
}

fn entry_mut<'a>(
    doc: &'a mut DocumentMut,
    path: &str,
    index: usize,
) -> Result<&'a mut dyn TableLike, String> {
    list_mut(doc, path)?
        .entry(index)
        .ok_or_else(|| not_tables(path))
}

fn names_mut<'a>(entry: &'a mut dyn TableLike, key: &str) -> Result<&'a mut Array, String> {
    entry
        .get_mut(key)
        .and_then(Item::as_array_mut)
        .ok_or_else(|| format!("`{key}` is not a list"))
}

fn keeps_own_prefix(v: &Value) -> bool {
    v.decor()
        .prefix()
        .and_then(|p| p.as_str())
        .is_some_and(|p| p.contains('\n') || p.contains('#'))
}

fn remove_at(array: &mut Array, index: usize) {
    let removed = array.remove(index);
    if index != 0 {
        return;
    }
    let Some(first) = array.get_mut(0) else {
        return;
    };
    if !keeps_own_prefix(first) {
        let prefix = removed.decor().prefix().cloned().unwrap_or_default();
        first.decor_mut().set_prefix(prefix);
    }
}

/// Adds `name` to the entry of `label` in the list of tables at the dotted `path`
/// (`secrets.allow`, `services.allow`): the first entry of the label decides, so the name goes
/// there, and an entry is created when the label has none. The list keeps its form: an inline
/// list stays inline. False when the name is already allowed for the label.
pub fn add_name(doc: &mut ConfigDoc, path: &str, label: &str, name: &str) -> Result<bool, String> {
    let doc = doc.inner_mut();
    let found = rows(doc, path)?
        .into_iter()
        .enumerate()
        .find(|(_, r)| r.label.as_deref() == Some(label));
    if found
        .as_ref()
        .is_some_and(|(_, r)| r.names.iter().any(|n| n == name))
    {
        return Ok(false);
    }
    let Some((index, _)) = found else {
        list_mut(doc, path)?.push(&[
            ("label", Value::from(label)),
            ("names", Value::Array(Array::from_iter([name]))),
        ]);
        return Ok(true);
    };
    names_mut(entry_mut(doc, path, index)?, "names")?.push(name);
    Ok(true)
}

/// Removes every `name` from the entry of `label` that decides and returns how many were
/// removed (0: the entry does not list the name). The entry goes when no name is left, unless
/// removing it would change what is allowed (a later entry of the label would decide, or an
/// empty `services.allow` would allow every service). Comments of the other names stay.
pub fn remove_name(
    doc: &mut ConfigDoc,
    path: &str,
    label: &str,
    name: &str,
) -> Result<usize, String> {
    let doc = doc.inner_mut();
    let all = rows(doc, path)?;
    let Some(index) = all.iter().position(|r| r.label.as_deref() == Some(label)) else {
        return Ok(0);
    };
    let names = &all[index].names;
    let count = names.iter().filter(|n| *n == name).count();
    if count == 0 {
        return Ok(0);
    }
    let later = all[index + 1..]
        .iter()
        .any(|r| r.label.as_deref() == Some(label));
    let opens = all.len() == 1 && OPEN_WHEN_EMPTY.contains(&path);
    if names.len() == count && !later && !opens {
        list_mut(doc, path)?.remove(index);
        return Ok(count);
    }
    let array = names_mut(entry_mut(doc, path, index)?, "names")?;
    loop {
        let found = array.iter().position(|v| v.as_str() == Some(name));
        let Some(i) = found else {
            break;
        };
        remove_at(array, i);
    }
    Ok(count)
}

/// Adds a `[[local_user]]` entry for the OS user `name` with `label` (none: the default empty
/// label). False when the user is already listed with that label; a user listed with another
/// label is an error.
pub fn add_local_user(
    doc: &mut ConfigDoc,
    name: &str,
    label: Option<&str>,
) -> Result<bool, String> {
    let doc = doc.inner_mut();
    let wanted = label.unwrap_or("");
    let existing = rows(doc, "local_user")?
        .into_iter()
        .find(|r| r.name.as_deref() == Some(name));
    if let Some(user) = existing {
        let has = user.label.unwrap_or_default();
        return match has == wanted {
            true => Ok(false),
            false => Err(format!(
                "local user `{name}` is already listed with label `{has}`: remove it first"
            )),
        };
    }
    let mut fields = vec![("name", Value::from(name))];
    if let Some(label) = label {
        fields.push(("label", Value::from(label)));
    }
    list_mut(doc, "local_user")?.push(&fields);
    Ok(true)
}

/// Removes the `[[local_user]]` entry of the OS user `name`. False when there is none.
pub fn remove_local_user(doc: &mut ConfigDoc, name: &str) -> Result<bool, String> {
    let doc = doc.inner_mut();
    let found = rows(doc, "local_user")?
        .into_iter()
        .position(|r| r.name.as_deref() == Some(name));
    let Some(index) = found else {
        return Ok(false);
    };
    list_mut(doc, "local_user")?.remove(index);
    Ok(true)
}

fn set_of(members: &[String]) -> BTreeSet<&str> {
    members.iter().map(String::as_str).collect()
}

fn shared_index(doc: &DocumentMut, members: &[String]) -> Result<Option<usize>, String> {
    let wanted = set_of(members);
    Ok(rows(doc, "shared")?
        .iter()
        .position(|r| set_of(&r.members) == wanted))
}

/// Adds a `[[shared]]` block of `members` (each named once, in the given order). False when a
/// block with the same set of members exists, whatever its order.
pub fn add_shared(doc: &mut ConfigDoc, members: &[String]) -> Result<bool, String> {
    let doc = doc.inner_mut();
    if shared_index(doc, members)?.is_some() {
        return Ok(false);
    }
    let mut seen = BTreeSet::new();
    let unique: Array = members
        .iter()
        .filter(|m| seen.insert(m.as_str()))
        .map(String::as_str)
        .collect();
    list_mut(doc, "shared")?.push(&[("members", Value::Array(unique))]);
    Ok(true)
}

/// Removes the `[[shared]]` block whose set of members is `members`. False when there is none.
pub fn remove_shared(doc: &mut ConfigDoc, members: &[String]) -> Result<bool, String> {
    let doc = doc.inner_mut();
    let Some(index) = shared_index(doc, members)? else {
        return Ok(false);
    };
    list_mut(doc, "shared")?.remove(index);
    Ok(true)
}
