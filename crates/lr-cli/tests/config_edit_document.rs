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
    assert!(doc.unset("runner.cpu").unwrap());
    assert!(!doc.render().contains("[runner]"));
    assert!(!doc.unset("runner.cpu").unwrap());
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
