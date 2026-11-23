use lr_cli::config_edit::ConfigDoc;

const SAMPLE: &str = "# top comment
[queue]
max = 3 # limit
name = \"q\"

# runner block
[runner]
cpu = 4

[hub]
addr = \"127.0.0.1\"
";

#[test]
fn set_keeps_comments_and_order() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    doc.set("queue.max", "5", false).unwrap();
    assert_eq!(doc.render(), SAMPLE.replace("max = 3", "max = 5"));
}

#[test]
fn new_value_is_parsed_as_toml_then_string() {
    let mut doc = ConfigDoc::parse("").unwrap();
    doc.set("a.int", "5", false).unwrap();
    doc.set("a.flag", "true", false).unwrap();
    doc.set("a.list", "[1,2]", false).unwrap();
    doc.set("a.text", "hello", false).unwrap();
    assert_eq!(doc.get("a.int").unwrap(), "5");
    assert_eq!(doc.get("a.flag").unwrap(), "true");
    assert_eq!(doc.get("a.list").unwrap(), "[1,2]");
    assert_eq!(doc.get("a.text").unwrap(), "\"hello\"");
    let parsed: toml::Value = doc.render().parse().unwrap();
    assert!(parsed["a"]["int"].is_integer());
    assert!(parsed["a"]["flag"].is_bool());
    assert!(parsed["a"]["list"].is_array());
    assert!(parsed["a"]["text"].is_str());
}

#[test]
fn existing_type_wins() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    doc.set("hub.addr", "5", false).unwrap();
    assert_eq!(doc.get("hub.addr").unwrap(), "\"5\"");
}

#[test]
fn force_string() {
    let mut doc = ConfigDoc::parse("").unwrap();
    doc.set("a.n", "5", true).unwrap();
    assert_eq!(doc.get("a.n").unwrap(), "\"5\"");
}

#[test]
fn unset_removes_key_and_empty_table() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    assert!(doc.unset("hub.addr").unwrap());
    assert!(!doc.render().contains("[hub]"));
    assert!(!doc.unset("hub.addr").unwrap());
}

#[test]
fn get_missing_is_none() {
    let doc = ConfigDoc::parse(SAMPLE).unwrap();
    assert_eq!(doc.get("queue.nope"), None);
    assert_eq!(doc.get("nope.max"), None);
    assert_eq!(doc.get("queue.max").unwrap(), "3");
}

#[test]
fn set_creates_missing_table() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    doc.set("fresh.key", "1", false).unwrap();
    assert!(doc.render().contains("[fresh]"));
    assert_eq!(doc.get("fresh.key").unwrap(), "1");
}

#[test]
fn quoted_segment_with_dots() {
    let mut doc = ConfigDoc::parse("").unwrap();
    doc.set("tags.\"a.b\"", "1", false).unwrap();
    assert_eq!(doc.get("tags.\"a.b\"").unwrap(), "1");
}

fn err(doc: &mut ConfigDoc, key: &str, value: &str) -> String {
    doc.set(key, value, false).unwrap_err()
}

#[test]
fn existing_integer_rejects_bool_and_names_key() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    let message = err(&mut doc, "queue.max", "true");
    assert!(
        message.contains("queue.max") && message.contains("integer"),
        "{message}"
    );
    assert_eq!(doc.get("queue.max").unwrap(), "3");
}

#[test]
fn existing_array_accepts_array() {
    let mut doc = ConfigDoc::parse("a = { l = [3] }\n[t]\nl = [3]\n").unwrap();
    doc.set("t.l", "[1,2]", false).unwrap();
    assert_eq!(doc.get("t.l").unwrap(), "[1,2]");
    assert!(err(&mut doc, "t.l", "5").contains("array"));
}

#[test]
fn existing_bool_rejects_yes() {
    let mut doc = ConfigDoc::parse("[t]\nb = false\n").unwrap();
    let message = err(&mut doc, "t.b", "yes");
    assert!(
        message.contains("t.b") && message.contains("boolean"),
        "{message}"
    );
    doc.set("t.b", "true", false).unwrap();
    assert_eq!(doc.get("t.b").unwrap(), "true");
}

#[test]
fn existing_string_keeps_array_text_as_string() {
    let mut doc = ConfigDoc::parse("[t]\ns = \"x\"\n").unwrap();
    doc.set("t.s", "[1, 2]", false).unwrap();
    assert_eq!(doc.get("t.s").unwrap(), "\"[1, 2]\"");
}

#[test]
fn force_string_overrides_existing_integer() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    doc.set("queue.max", "true", true).unwrap();
    assert_eq!(doc.get("queue.max").unwrap(), "\"true\"");
}

#[test]
fn existing_float_accepts_integer_text() {
    let mut doc = ConfigDoc::parse("[t]\nf = 1.5\n").unwrap();
    doc.set("t.f", "2", false).unwrap();
    assert_eq!(doc.get("t.f").unwrap(), "2.0");
}

const INLINE: &str = "queue = { max = 3, name = \"q\" }\n";

#[test]
fn get_through_inline_table() {
    let doc = ConfigDoc::parse(INLINE).unwrap();
    assert_eq!(doc.get("queue.max").unwrap(), "3");
    assert_eq!(doc.get("queue.nope"), None);
}

#[test]
fn set_through_inline_table_edits_in_place() {
    let mut doc = ConfigDoc::parse(INLINE).unwrap();
    doc.set("queue.max", "5", false).unwrap();
    doc.set("queue.extra", "1", false).unwrap();
    let parsed: toml::Value = doc.render().parse().unwrap();
    assert_eq!(parsed["queue"]["max"].as_integer(), Some(5));
    assert_eq!(parsed["queue"]["extra"].as_integer(), Some(1));
    assert_eq!(parsed["queue"]["name"].as_str(), Some("q"));
    assert!(doc.render().starts_with("queue = {"));
}

#[test]
fn unset_through_inline_table() {
    let mut doc = ConfigDoc::parse(INLINE).unwrap();
    assert!(doc.unset("queue.max").unwrap());
    assert_eq!(doc.get("queue.max"), None);
    assert_eq!(doc.get("queue.name").unwrap(), "\"q\"");
    assert!(doc.unset("queue.name").unwrap());
    assert!(!doc.render().contains("queue"));
}

#[test]
fn unset_last_key_keeps_commented_table_and_drops_plain_one() {
    let mut doc = ConfigDoc::parse(SAMPLE).unwrap();
    assert!(doc.unset("runner.cpu").unwrap());
    let text = doc.render();
    assert!(
        text.contains("# runner block") && text.contains("[runner]"),
        "{text}"
    );
    assert!(doc.unset("hub.addr").unwrap());
    assert!(!doc.render().contains("[hub]"));
}
