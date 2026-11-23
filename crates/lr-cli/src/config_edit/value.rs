use toml_edit::Value;

/// Turns the text of a setting value into a TOML value. `force_string` stores the text as a
/// string. Without an existing value the text is read as TOML, and as a plain string when that
/// fails. With an existing value its type decides: a string stays a string, any other type must
/// be given in that type, otherwise the name of the expected type is returned as the error.
pub fn parse_value(
    text: &str,
    existing: Option<&Value>,
    force_string: bool,
) -> Result<Value, &'static str> {
    if force_string {
        return Ok(Value::from(text));
    }
    let Some(existing) = existing else {
        return Ok(text
            .trim()
            .parse::<Value>()
            .unwrap_or_else(|_| Value::from(text)));
    };
    if existing.is_str() {
        return Ok(Value::from(text));
    }
    let parsed = text.trim().parse::<Value>().ok();
    match (existing, parsed) {
        (Value::Integer(_), Some(v @ Value::Integer(_))) => Ok(v),
        (Value::Integer(_), _) => Err("an integer"),
        (Value::Float(_), Some(v @ Value::Float(_))) => Ok(v),
        (Value::Float(_), Some(Value::Integer(i))) => Ok(Value::from(*i.value() as f64)),
        (Value::Float(_), _) => Err("a float"),
        (Value::Boolean(_), Some(v @ Value::Boolean(_))) => Ok(v),
        (Value::Boolean(_), _) => Err("a boolean (true or false)"),
        (Value::Array(_), Some(v @ Value::Array(_))) => Ok(v),
        (Value::Array(_), _) => Err("an array"),
        (Value::InlineTable(_), Some(v @ Value::InlineTable(_))) => Ok(v),
        (Value::InlineTable(_), _) => Err("an inline table"),
        (Value::Datetime(_), Some(v @ Value::Datetime(_))) => Ok(v),
        (Value::Datetime(_), _) => Err("a datetime"),
        _ => Err("a value of the existing type"),
    }
}
