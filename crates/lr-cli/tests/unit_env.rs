mod common;

use common::TempDir;
use lr_cli::unit_env::systemd_environment;

fn unit(dir: &TempDir) -> std::path::PathBuf {
    let home = dir.path().to_path_buf();
    let d = home.join(".config/systemd/user");
    std::fs::create_dir_all(d.join("u.service.d")).unwrap();
    std::fs::create_dir_all(home.join(".config/al-localrunner")).unwrap();
    std::fs::write(
        d.join("u.service"),
        "[Service]\nEnvironment=A=1 \"B=two words\"\nExecStart=/bin/true\n",
    )
    .unwrap();
    std::fs::write(
        d.join("u.service.d/secrets.conf"),
        "[Service]\nEnvironmentFile=-%h/.config/al-localrunner/secrets.env\nEnvironmentFile=-%h/missing.env\n",
    )
    .unwrap();
    std::fs::write(
        home.join(".config/al-localrunner/secrets.env"),
        "# a comment\nTOKEN=abc\nQUOTED=\"in quotes\"\nEMPTY=\n\nexport EXPORTED=yes\n",
    )
    .unwrap();
    home
}

#[test]
fn the_variables_of_a_unit_come_from_its_environment_lines_its_drop_ins_and_environment_files() {
    let dir = TempDir::new("unit-env");
    let home = unit(&dir);
    let env = systemd_environment(&home, "u.service").unwrap();
    assert_eq!(env.get("A").map(String::as_str), Some("1"));
    assert_eq!(env.get("B").map(String::as_str), Some("two words"));
    assert_eq!(env.get("TOKEN").map(String::as_str), Some("abc"));
    assert_eq!(env.get("QUOTED").map(String::as_str), Some("in quotes"));
    assert_eq!(env.get("EMPTY").map(String::as_str), Some(""));
    assert_eq!(env.get("EXPORTED").map(String::as_str), Some("yes"));
    assert!(!env.contains_key("# a comment"));
}

#[test]
fn a_unit_that_does_not_exist_has_no_environment_and_a_missing_optional_file_is_skipped() {
    let dir = TempDir::new("unit-env-none");
    assert!(systemd_environment(dir.path(), "none.service").is_none());
    let home = unit(&dir);
    assert!(systemd_environment(&home, "u.service").is_some());
}
