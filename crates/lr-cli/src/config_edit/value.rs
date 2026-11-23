use toml_edit::Value;

/// Turns the text of a new setting value into a TOML value. The text is read as TOML first and
/// as a plain string when that fails. An existing string keeps its type, and `force_string`
/// stores the text as a string unconditionally.
pub fn parse_value(text: &str, existing: Option<&Value>, force_string: bool) -> Value {
    if force_string {
        return Value::from(text);
    }
    match text.trim().parse::<Value>() {
        Ok(parsed) => {
            let existing_is_string = existing.is_some_and(Value::is_str);
            let scalar = !parsed.is_array() && !parsed.is_inline_table();
            if existing_is_string && scalar && !parsed.is_str() {
                Value::from(text)
            } else {
                parsed
            }
        }
        Err(_) => Value::from(text),
    }
}
