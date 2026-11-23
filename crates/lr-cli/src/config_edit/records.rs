use super::document::ConfigDoc;
use std::collections::BTreeSet;
use toml_edit::{value, Array, ArrayOfTables, DocumentMut, Item, Table, TableLike, Value};

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

fn tables_mut<'a>(doc: &'a mut DocumentMut, path: &str) -> Result<&'a mut ArrayOfTables, String> {
    let parts: Vec<&str> = path.split('.').collect();
    let (last, parents) = parts
        .split_last()
        .ok_or_else(|| format!("empty key `{path}`"))?;
    let mut table: &mut Table = doc.as_table_mut();
    for part in parents {
        let mut created = Table::new();
        created.set_implicit(true);
        table = table
            .entry(part)
            .or_insert(Item::Table(created))
            .as_table_mut()
            .ok_or_else(|| format!("`{part}` in `{path}` is not a table"))?;
    }
    let item = table
        .entry(last)
        .or_insert(Item::ArrayOfTables(ArrayOfTables::new()));
    let inline = match &*item {
        Item::Value(Value::Array(array)) => Some(
            array
                .iter()
                .map(|v| v.as_inline_table().map(|t| t.clone().into_table()))
                .collect::<Option<Vec<Table>>>()
                .ok_or_else(|| not_tables(path))?,
        ),
        _ => None,
    };
    if let Some(converted) = inline {
        let mut tables = ArrayOfTables::new();
        for t in converted {
            tables.push(t);
        }
        *item = Item::ArrayOfTables(tables);
    }
    item.as_array_of_tables_mut()
        .ok_or_else(|| not_tables(path))
}

fn list_mut<'a>(table: &'a mut Table, key: &str) -> Result<&'a mut Array, String> {
    table
        .entry(key)
        .or_insert(Item::Value(Value::Array(Array::new())))
        .as_array_mut()
        .ok_or_else(|| format!("`{key}` is not a list"))
}

fn remove_from(array: &mut Array, text: &str) {
    let Some(index) = array.iter().position(|v| v.as_str() == Some(text)) else {
        return;
    };
    let removed = array.remove(index);
    if index == 0 {
        if let Some(first) = array.get_mut(0) {
            let prefix = removed.decor().prefix().cloned().unwrap_or_default();
            first.decor_mut().set_prefix(prefix);
        }
    }
}

/// Adds `name` to the entry of `label` in the list of tables at the dotted `path`
/// (`secrets.allow`, `services.allow`): the first entry of the label decides, so the name goes
/// there, and an entry is created when the label has none. False when the name is already
/// allowed for the label.
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
        let mut entry = Table::new();
        entry.insert("label", value(label));
        entry.insert("names", Item::Value(Value::Array(Array::from_iter([name]))));
        tables_mut(doc, path)?.push(entry);
        return Ok(true);
    };
    let entry = tables_mut(doc, path)?
        .get_mut(index)
        .ok_or_else(|| not_tables(path))?;
    list_mut(entry, "names")?.push(name);
    Ok(true)
}

/// Removes `name` from the entry of `label` that decides; the entry goes when no name is left,
/// unless removing it would change what is allowed (a later entry of the label would decide, or
/// an empty `services.allow` would allow every service). False when the label's entry does not
/// list the name.
pub fn remove_name(
    doc: &mut ConfigDoc,
    path: &str,
    label: &str,
    name: &str,
) -> Result<bool, String> {
    let doc = doc.inner_mut();
    let all = rows(doc, path)?;
    let Some(index) = all.iter().position(|r| r.label.as_deref() == Some(label)) else {
        return Ok(false);
    };
    let names = &all[index].names;
    if !names.iter().any(|n| n == name) {
        return Ok(false);
    }
    let later = all[index + 1..]
        .iter()
        .any(|r| r.label.as_deref() == Some(label));
    let opens = all.len() == 1 && OPEN_WHEN_EMPTY.contains(&path);
    let drop = names.iter().all(|n| n == name) && !later && !opens;
    let tables = tables_mut(doc, path)?;
    if drop {
        tables.remove(index);
        return Ok(true);
    }
    let entry = tables.get_mut(index).ok_or_else(|| not_tables(path))?;
    remove_from(list_mut(entry, "names")?, name);
    Ok(true)
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
    let mut entry = Table::new();
    entry.insert("name", value(name));
    if let Some(label) = label {
        entry.insert("label", value(label));
    }
    tables_mut(doc, "local_user")?.push(entry);
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
    tables_mut(doc, "local_user")?.remove(index);
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
    let mut block = Table::new();
    block.insert("members", Item::Value(Value::Array(unique)));
    tables_mut(doc, "shared")?.push(block);
    Ok(true)
}

/// Removes the `[[shared]]` block whose set of members is `members`. False when there is none.
pub fn remove_shared(doc: &mut ConfigDoc, members: &[String]) -> Result<bool, String> {
    let doc = doc.inner_mut();
    let Some(index) = shared_index(doc, members)? else {
        return Ok(false);
    };
    tables_mut(doc, "shared")?.remove(index);
    Ok(true)
}
