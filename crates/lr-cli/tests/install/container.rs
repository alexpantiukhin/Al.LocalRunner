use crate::{install_file, repo_root, yaml};
use lr_broker::policy::{HostResources, ListenValue};
use serde_json::Value;

const VERSION: &str = env!("CARGO_PKG_VERSION");
const STATE_DIR: &str = "/var/lib/al-localrunner";
const CONFIG_DIR: &str = "/etc/al-localrunner";

fn compose() -> Value {
    yaml::parse(&install_file("compose.yaml")).expect("compose.yaml is valid YAML")
}

fn strings(v: &Value) -> Vec<String> {
    v.as_array()
        .unwrap_or_else(|| panic!("expected a list, got {v}"))
        .iter()
        .map(|s| s.as_str().expect("a string").to_string())
        .collect()
}

struct Stage {
    from: String,
    lines: Vec<String>,
}

fn dockerfile_stages() -> Vec<Stage> {
    stages_of(&install_file("Dockerfile"))
}

fn stages_of(text: &str) -> Vec<Stage> {
    let mut stages: Vec<Stage> = Vec::new();
    let mut logical = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with('#') {
            continue;
        }
        if let Some(head) = line.strip_suffix('\\') {
            logical.push_str(head);
            logical.push(' ');
            continue;
        }
        logical.push_str(line);
        let full = std::mem::take(&mut logical);
        if let Some(rest) = full.strip_prefix("FROM ") {
            stages.push(Stage {
                from: rest.trim().to_string(),
                lines: Vec::new(),
            });
        } else {
            stages
                .last_mut()
                .expect("an instruction before FROM")
                .lines
                .push(full);
        }
    }
    stages
}

fn instruction<'a>(stage: &'a Stage, name: &str) -> Vec<&'a str> {
    stage
        .lines
        .iter()
        .filter_map(|l| l.strip_prefix(name).and_then(|r| r.strip_prefix(' ')))
        .map(str::trim)
        .collect()
}

fn assert_explicit_tag(image: &str) {
    let name = image.split_whitespace().next().unwrap();
    let last = name.rsplit('/').next().unwrap();
    let tag = last
        .split_once(':')
        .map(|(_, t)| t)
        .unwrap_or_else(|| panic!("image {name} has no explicit tag"));
    assert!(
        !tag.is_empty() && tag != "latest",
        "image {name}: tag {tag}"
    );
}

#[test]
fn compose_runs_broker_and_runner_from_the_versioned_image_as_non_root() {
    let doc = compose();
    let services = &doc["services"];
    let image = format!("al-localrunner:{VERSION}");
    for (name, role, volume, config) in [
        ("broker", "broker", "broker-state", "policy.toml"),
        ("runner", "runner", "runner-state", "runner.toml"),
    ] {
        let s = &services[name];
        if name == "broker" {
            assert_eq!(s["image"], image.as_str(), "{name}: image");
        }
        assert_eq!(
            strings(&s["command"]),
            ["localrunner", role, "run"],
            "{name}: command"
        );
        let user = s["user"].as_str().expect("user is set");
        assert!(
            !user.starts_with("0") && !user.starts_with("root"),
            "{user}"
        );
        let volumes = strings(&s["volumes"]);
        assert!(
            volumes.contains(&format!("{volume}:{STATE_DIR}")),
            "{volumes:?}"
        );
        assert!(
            volumes.contains(&format!("./{config}:{CONFIG_DIR}/{config}:ro")),
            "{volumes:?}"
        );
        assert_eq!(s["restart"], "unless-stopped");
        assert!(doc["volumes"][volume].is_object(), "named volume {volume}");
    }
    assert_eq!(strings(&services["runner"]["depends_on"]), ["broker"]);
    assert!(
        services["runner"]["ports"].is_null(),
        "runner publishes no port"
    );
    let ports = strings(&services["broker"]["ports"]);
    assert_eq!(
        ports,
        ["127.0.0.1:7443:7443"],
        "the broker port is published on the host's loopback only"
    );
    assert_eq!(
        services["broker"]["build"]["dockerfile"],
        "install/Dockerfile"
    );
    assert_eq!(
        services["broker"]["build"]["context"], "..",
        "the build context is the repository root"
    );
    assert_eq!(
        services["runner"]["hostname"], "runner",
        "the runner's name in the pool"
    );
}

fn runner_dockerfile() -> String {
    compose()["services"]["runner"]["build"]["dockerfile_inline"]
        .as_str()
        .expect("the runner is built from dockerfile_inline")
        .to_string()
}

#[test]
fn runner_is_built_from_an_inline_dockerfile_with_visible_packages_and_the_published_binary() {
    let doc = compose();
    let runner = &doc["services"]["runner"];
    assert!(
        runner["image"].is_null(),
        "the runner names no image to pull"
    );
    let stages = stages_of(&runner_dockerfile());
    assert_eq!(
        stages.len(),
        1,
        "one stage: the binary comes from the published image"
    );
    let stage = &stages[0];
    assert_explicit_tag(&stage.from);
    let runs = instruction(stage, "RUN");
    let packages = runs
        .iter()
        .find(|r| r.contains("install"))
        .expect("a RUN installs the packages");
    for package in ["ca-certificates", "git", "python3"] {
        assert!(
            packages.split_whitespace().any(|w| w == package),
            "{package} is in the visible package list: {packages}"
        );
    }
    let copies = instruction(stage, "COPY");
    assert_eq!(copies.len(), 1, "{copies:?}");
    let parts: Vec<&str> = copies[0].split_whitespace().collect();
    assert_eq!(
        parts[0],
        format!("--from=ghcr.io/alexpantiukhin/al.localrunner:{VERSION}"),
        "the binary comes from the image of the same version"
    );
    assert_eq!(
        parts[1..],
        ["/usr/local/bin/localrunner", "/usr/local/bin/localrunner"]
    );
    let user = instruction(stage, "USER");
    assert_eq!(user.len(), 1, "{user:?}");
    assert_eq!(
        runner["user"].as_str().unwrap(),
        user[0],
        "the Dockerfile and the service run as the same user"
    );
    let uid = user[0].split(':').next().unwrap();
    assert!(uid != "0" && uid != "root", "runtime user {uid}");
    let env = instruction(stage, "ENV").join(" ");
    assert!(
        env.contains(&format!("LOCALRUNNER_STATE_DIR={STATE_DIR}")),
        "{env}"
    );
    assert!(
        env.contains(&format!("LOCALRUNNER_CONFIG_DIR={CONFIG_DIR}")),
        "{env}"
    );
    let mkdir = runs
        .iter()
        .find(|r| r.contains("mkdir"))
        .expect("directories are made");
    for dir in [STATE_DIR, CONFIG_DIR, "/run/al-localrunner"] {
        assert!(mkdir.contains(dir), "{dir} is created and owned: {mkdir}");
    }
    assert_eq!(strings(&runner["cap_drop"]), ["ALL"]);
    assert_eq!(strings(&runner["security_opt"]), ["no-new-privileges:true"]);
}

#[test]
fn the_release_compose_keeps_the_runner_build_and_drops_only_the_broker_build() {
    let release = std::fs::read_to_string(repo_root().join(".github/workflows/release.yml"))
        .expect("release.yml exists")
        .replace("\r\n", "\n");
    let sed = release
        .lines()
        .find(|l| l.contains("src/install/compose.yaml"))
        .expect("the release rewrites compose.yaml");
    assert!(
        !sed.contains("/dockerfile:/d"),
        "a range up to `dockerfile:` would eat the inline Dockerfile of the runner: {sed}"
    );
    assert!(
        sed.contains("COPY --from="),
        "the binary source follows the release tag: {sed}"
    );
}

#[test]
fn image_is_built_on_alpine_musl_and_runs_on_debian_slim_with_a_shell_and_glibc() {
    let stages = dockerfile_stages();
    let build = stages[0].from.split_whitespace().next().unwrap();
    let (name, tag) = build.split_once(':').unwrap();
    assert_eq!(name, "rust");
    assert!(tag.ends_with("-alpine"), "musl build image: {build}");
    assert_eq!(stages.last().unwrap().from, "debian:bookworm-slim");
    let dirs: Vec<&str> = instruction(stages.last().unwrap(), "COPY")
        .into_iter()
        .filter(|c| c.contains("/out/empty"))
        .map(|c| c.rsplit(' ').next().unwrap())
        .collect();
    assert_eq!(
        dirs,
        [
            "/var/lib/al-localrunner",
            "/etc/al-localrunner",
            "/run/al-localrunner"
        ],
        "state, configuration and IPC directories owned by 65532"
    );
}

#[test]
fn no_image_anywhere_uses_latest_or_an_implicit_tag() {
    let doc = compose();
    for (_, s) in doc["services"].as_object().unwrap() {
        if let Some(image) = s["image"].as_str() {
            assert_explicit_tag(image);
        }
    }
    for stage in stages_of(&runner_dockerfile()) {
        assert_explicit_tag(&stage.from);
    }
    for stage in dockerfile_stages() {
        assert_explicit_tag(&stage.from);
    }
    for name in ["compose.yaml", "Dockerfile"] {
        assert!(
            !install_file(name).contains(":latest"),
            "{name} names latest"
        );
    }
}

#[test]
fn image_is_multi_stage_with_only_the_binary_and_a_non_root_user() {
    let stages = dockerfile_stages();
    assert!(stages.len() >= 2, "multi-stage build");
    let build_name = stages[0]
        .from
        .split_whitespace()
        .last()
        .unwrap()
        .to_string();
    let last = stages.last().unwrap();
    let runs = instruction(last, "RUN");
    assert_eq!(runs.len(), 1, "one RUN in the runtime image: {runs:?}");
    assert!(
        runs[0].contains("apt-get install -y --no-install-recommends ca-certificates"),
        "the only package is ca-certificates: {}",
        runs[0]
    );
    let user = instruction(last, "USER");
    assert_eq!(user.len(), 1, "one USER in the runtime stage");
    let uid = user[0].split(':').next().unwrap();
    assert!(uid != "0" && uid != "root", "runtime user {uid}");
    assert_eq!(uid, "65532");
    let mut binary = 0;
    for copy in instruction(last, "COPY") {
        let parts: Vec<&str> = copy.split_whitespace().collect();
        assert!(
            parts.contains(&format!("--from={build_name}").as_str()),
            "runtime copies only from the build stage: {copy}"
        );
        let source = parts[parts.len() - 2];
        let dest = parts[parts.len() - 1];
        match source {
            "/out/localrunner" => {
                assert_eq!(dest, "/usr/local/bin/localrunner");
                binary += 1;
            }
            "/out/empty" => assert!(parts.contains(&"--chown=65532:65532"), "{copy}"),
            other => panic!("runtime image copies {other}"),
        }
    }
    assert_eq!(binary, 1, "the binary is copied once");
    assert_eq!(instruction(last, "ENTRYPOINT"), ["[]"]);
    let env = instruction(last, "ENV").join(" ");
    assert!(
        env.contains(&format!("LOCALRUNNER_STATE_DIR={STATE_DIR}")),
        "{env}"
    );
    assert!(
        env.contains(&format!("LOCALRUNNER_CONFIG_DIR={CONFIG_DIR}")),
        "{env}"
    );
    assert!(instruction(&stages[0], "RUN")
        .iter()
        .any(|r| r.contains("cargo build --release --locked")));
}

#[test]
fn build_context_keeps_only_sources() {
    let text = std::fs::read_to_string(repo_root().join(".dockerignore"))
        .expect(".dockerignore exists")
        .replace("\r\n", "\n");
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(lines.first(), Some(&"*"), "everything is excluded first");
    for keep in lines.iter().filter(|l| l.starts_with('!')) {
        assert!(
            ["!Cargo.toml", "!Cargo.lock", "!crates"].contains(keep),
            "unexpected re-include {keep}"
        );
    }
}

#[test]
fn example_policy_listens_in_the_container_and_knows_the_compose_network() {
    let text = install_file("policy.toml");
    let file = lr_broker::policy::load(&text).expect("policy.toml parses");
    assert_eq!(file.listen.addresses, [ListenValue::Container]);
    assert!(file.listen.check_at_start(true).is_ok());
    let doc = compose();
    let subnet = doc["networks"]["pool"]["ipam"]["config"][0]["subnet"]
        .as_str()
        .expect("the compose network has a fixed subnet");
    assert!(
        text.contains(&format!("\"{subnet}\"")),
        "[networks] has {subnet}"
    );
    for name in ["broker", "runner"] {
        assert_eq!(strings(&doc["services"][name]["networks"]), ["pool"]);
    }
}

#[test]
fn example_runner_settings_parse() {
    let text = install_file("runner.toml");
    let dir = std::env::temp_dir().join("lr-install-runner-toml");
    let host = HostResources {
        mem_mb: 4096,
        cpu: 2,
    };
    lr_cli::runner_config::RunnerConfig::parse(&text, None, "runner", dir, host)
        .expect("runner.toml parses");
}

#[test]
fn install_files_keep_lf_line_endings_on_every_checkout() {
    let text = std::fs::read_to_string(repo_root().join(".gitattributes"))
        .expect(".gitattributes exists")
        .replace("\r\n", "\n");
    for pattern in ["*.sh", "*.yaml", "install/Dockerfile", "install/*.toml"] {
        assert!(
            text.lines().any(|l| l == format!("{pattern} text eol=lf")),
            "{pattern} has eol=lf"
        );
    }
}
