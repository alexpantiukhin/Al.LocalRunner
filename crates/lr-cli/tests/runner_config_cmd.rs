mod common;

use common::TempDir;
use lr_cli::commands::runner_config_cmd::{diff, show};
use lr_cli::paths::Layout;

fn layout(dir: &TempDir) -> Layout {
    let layout = Layout::new(dir.path().join("state"), dir.path().join("config"));
    std::fs::create_dir_all(&layout.config).unwrap();
    std::fs::write(
        layout.config_file("runner.toml"),
        "mem_mb = 8192\ncpu = 4\n",
    )
    .unwrap();
    layout
}

#[test]
fn a_runner_without_an_own_copy_reads_the_template_and_says_so() {
    let dir = TempDir::new("rc-template");
    let layout = layout(&dir);
    let text = show(&layout, "r1").unwrap();
    assert!(text.contains("the template"), "{text}");
    assert!(text.contains("mem_mb = 8192"), "{text}");
    assert!(diff(&layout, "r1").unwrap().contains("no own copy"));
}

#[test]
fn a_joined_runner_reads_its_own_copy_and_the_diff_shows_the_lines_that_differ() {
    let dir = TempDir::new("rc-own");
    let layout = layout(&dir);
    let own = layout.runner_dir("r1");
    std::fs::create_dir_all(&own).unwrap();
    std::fs::write(own.join("runner.toml"), "mem_mb = 4096\ncpu = 4\n").unwrap();
    let text = show(&layout, "r1").unwrap();
    assert!(text.contains("its own copy"), "{text}");
    assert!(text.contains("mem_mb = 4096"), "{text}");
    let d = diff(&layout, "r1").unwrap();
    assert!(d.contains("-mem_mb = 8192"), "{d}");
    assert!(d.contains("+mem_mb = 4096"), "{d}");
    assert!(!d.contains("cpu = 4"), "{d}");
}

#[test]
fn a_name_that_leaves_the_runner_directory_is_refused() {
    let dir = TempDir::new("rc-name");
    assert!(show(&layout(&dir), "../x").is_err());
}
