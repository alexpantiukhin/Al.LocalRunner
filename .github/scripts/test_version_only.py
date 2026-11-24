import os
import subprocess
import tempfile
import unittest

import version_only as v

CRATE = ["[package]", 'name = "lr-core"', 'version = "0.8.0"', "", "[dependencies]", 'ureq = { version = "2" }']
CRATE_NEW = [l.replace("0.8.0", "0.9.0") for l in CRATE]
LOCK = ["[[package]]", 'name = "lr-core"', 'version = "0.8.0"', "", "[[package]]", 'name = "serde"', 'version = "1.0.1"']
LOCK_NEW = [l.replace("0.8.0", "0.9.0") for l in LOCK]
COMPOSE = [
    "services:",
    "  broker:",
    "    image: al-localrunner:0.8.0",
    "        COPY --from=ghcr.io/alexpantiukhin/al.localrunner:0.8.0 /usr/local/bin/localrunner /usr/local/bin/localrunner",
    "    ports: [\"127.0.0.1:7443:7443\"]",
]
COMPOSE_NEW = [l.replace("0.8.0", "0.9.0") for l in COMPOSE]
PLUGIN = ["{", '  "name": "localrunner",', '  "version": "0.8.0",', '  "description": "x"', "}"]
PLUGIN_NEW = [l.replace("0.8.0", "0.9.0") for l in PLUGIN]
MANIFESTS = ["crates/lr-core/Cargo.toml"]


def check(files):
    statuses = {p: "M" for p in files}
    sides = {"old": {}, "new": {}}
    for p, (old, new) in files.items():
        sides["old"][p] = old
        sides["new"][p] = new
    sides["new"].setdefault("crates/lr-core/Cargo.toml", CRATE_NEW)
    return v.is_version_only(statuses, lambda s, p: sides[s].get(p), MANIFESTS)


class VersionOnly(unittest.TestCase):
    def test_every_version_file_with_only_the_version_changed(self):
        self.assertTrue(
            check(
                {
                    "crates/lr-core/Cargo.toml": (CRATE, CRATE_NEW),
                    "Cargo.lock": (LOCK, LOCK_NEW),
                    "install/compose.yaml": (COMPOSE, COMPOSE_NEW),
                    "plugin/.claude-plugin/plugin.json": (PLUGIN, PLUGIN_NEW),
                    "plugin/.codex-plugin/plugin.json": (PLUGIN, PLUGIN_NEW),
                }
            )
        )

    def test_the_image_tag_of_the_broker_and_of_copy_from_in_compose(self):
        self.assertTrue(check({"install/compose.yaml": (COMPOSE, COMPOSE_NEW)}))
        only_image = [COMPOSE_NEW[0], COMPOSE_NEW[1], COMPOSE_NEW[2], COMPOSE[3], COMPOSE[4]]
        old = COMPOSE
        self.assertTrue(check({"install/compose.yaml": (old, only_image)}))

    def test_another_line_next_to_the_version_gives_the_full_matrix(self):
        for path, old, new, extra in [
            ("crates/lr-core/Cargo.toml", CRATE, CRATE_NEW, 'edition = "2021"'),
            ("Cargo.lock", LOCK, LOCK_NEW, 'checksum = "x"'),
            ("install/compose.yaml", COMPOSE, COMPOSE_NEW, "    restart: always"),
            ("plugin/.claude-plugin/plugin.json", PLUGIN, PLUGIN_NEW, '  "author": "x",'),
        ]:
            with self.subTest(path=path):
                self.assertFalse(check({path: (old, new + [extra])}))
                self.assertFalse(check({path: (old, [extra] + new)}))

    def test_a_changed_line_that_is_not_the_version(self):
        changed = [l.replace("lr-core", "lr-other") for l in CRATE_NEW]
        self.assertFalse(check({"crates/lr-core/Cargo.toml": (CRATE, changed)}))
        dep = CRATE_NEW[:-1] + ['ureq = { version = "3" }']
        self.assertFalse(check({"crates/lr-core/Cargo.toml": (CRATE, dep)}))
        lock = LOCK_NEW[:-1] + ['version = "1.0.2"']
        self.assertFalse(check({"Cargo.lock": (LOCK, lock)}))
        tag = [l.replace("al-localrunner:0.8.0", "other:0.9.0") for l in COMPOSE]
        self.assertFalse(check({"install/compose.yaml": (COMPOSE, tag)}))

    def test_a_file_outside_the_list(self):
        self.assertFalse(
            check(
                {
                    "crates/lr-core/Cargo.toml": (CRATE, CRATE_NEW),
                    "crates/lr-core/src/lib.rs": (["a"], ["b"]),
                }
            )
        )
        self.assertFalse(check({"Cargo.toml": (CRATE, CRATE_NEW)}))
        self.assertFalse(check({"README.md": (["a"], ["a"])}))

    def test_an_added_deleted_or_renamed_file(self):
        for status in ("A", "D", "R", "T"):
            with self.subTest(status=status):
                sides = {"crates/lr-core/Cargo.toml": CRATE}
                self.assertFalse(
                    v.is_version_only(
                        {"crates/lr-core/Cargo.toml": status},
                        lambda s, p: sides.get(p) if s == "old" else CRATE_NEW,
                        MANIFESTS,
                    )
                )
        self.assertFalse(v.is_version_only({"Cargo.lock": "M"}, lambda s, p: None, MANIFESTS))

    def test_no_change_at_all(self):
        self.assertFalse(v.is_version_only({}, lambda s, p: None, MANIFESTS))
        self.assertFalse(check({"Cargo.lock": (LOCK, LOCK)}))

    def test_a_lock_version_of_a_crate_outside_the_workspace(self):
        lock = LOCK[:-1] + ['version = "1.0.1"']
        lock_new = LOCK[:-1] + ['version = "1.0.2"']
        self.assertFalse(check({"Cargo.lock": (lock, lock_new)}))


def run(cwd, *args):
    subprocess.run(
        ["git", "-c", "user.name=t", "-c", "user.email=t@t", "-c", "commit.gpgsign=false", *args],
        cwd=cwd,
        check=True,
        capture_output=True,
    )


def write(root, path, lines):
    full = os.path.join(root, path)
    os.makedirs(os.path.dirname(full), exist_ok=True)
    with open(full, "w", encoding="utf-8", newline="\n") as f:
        f.write("\n".join(lines) + "\n")


class InRepository(unittest.TestCase):
    def setUp(self):
        self.tmp = tempfile.TemporaryDirectory()
        self.root = self.tmp.name
        self.cwd = os.getcwd()
        os.chdir(self.root)
        run(self.root, "init", "-b", "main")
        write(self.root, "crates/lr-core/Cargo.toml", CRATE)
        write(self.root, "Cargo.lock", LOCK)
        write(self.root, "src.txt", ["a"])
        run(self.root, "add", "-A")
        run(self.root, "commit", "-m", "base")

    def tearDown(self):
        os.chdir(self.cwd)
        self.tmp.cleanup()

    def merge(self, edit):
        run(self.root, "checkout", "-b", "pr")
        edit()
        run(self.root, "add", "-A")
        run(self.root, "commit", "-m", "pr")
        run(self.root, "checkout", "main")
        write(self.root, "other.txt", ["x"])
        run(self.root, "add", "-A")
        run(self.root, "commit", "-m", "main moved on")
        run(self.root, "merge", "--no-ff", "-m", "merge", "pr")

    def bump(self):
        write(self.root, "crates/lr-core/Cargo.toml", CRATE_NEW)
        write(self.root, "Cargo.lock", LOCK_NEW)

    def test_a_merge_commit_that_only_bumps_the_version(self):
        self.merge(self.bump)
        self.assertTrue(v.version_only_in_repo())

    def test_a_merge_commit_with_another_change(self):
        def edit():
            self.bump()
            write(self.root, "src.txt", ["b"])

        self.merge(edit)
        self.assertFalse(v.version_only_in_repo())

    def test_head_that_is_not_a_merge_commit(self):
        self.bump()
        run(self.root, "add", "-A")
        run(self.root, "commit", "-m", "bump")
        self.assertFalse(v.version_only_in_repo())

    def test_a_directory_that_is_not_a_repository(self):
        with tempfile.TemporaryDirectory() as empty:
            os.chdir(empty)
            try:
                self.assertFalse(v.version_only_in_repo())
            finally:
                os.chdir(self.root)


if __name__ == "__main__":
    unittest.main()
