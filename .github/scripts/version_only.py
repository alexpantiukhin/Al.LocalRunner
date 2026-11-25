import os
import re
import subprocess
import sys

CARGO_TOML = re.compile(r"^crates/[^/]+/Cargo\.toml$")
CARGO_LOCK = "Cargo.lock"
COMPOSE = "install/compose.yaml"
PLUGIN_JSONS = ("plugin/.claude-plugin/plugin.json", "plugin/.codex-plugin/plugin.json")

VERSION_ASSIGN = re.compile(r'^version\s*=\s*"[^"]*"\s*$')
NAME_ASSIGN = re.compile(r'^name\s*=\s*"([^"]*)"\s*$')
SECTION = re.compile(r"^\s*\[")
IMAGE_TAG = re.compile(r"(al[.-]localrunner:)[0-9][^\s\"']*")
JSON_VERSION = re.compile(r'^(\s*"version"\s*:\s*)"[^"]*"(,?)\s*$')


def git(*args):
    return subprocess.run(
        ["git", *args], check=True, capture_output=True, text=True, encoding="utf-8"
    ).stdout


def differing(old, new):
    if len(old) != len(new):
        return None
    return [i for i, (a, b) in enumerate(zip(old, new)) if a != b]


def package_version_lines(lines):
    found = set()
    in_package = False
    for i, line in enumerate(lines):
        if SECTION.match(line):
            in_package = line.strip() == "[package]"
        elif in_package and VERSION_ASSIGN.match(line):
            found.add(i)
    return found


def cargo_toml_is_version_only(old, new):
    changed = differing(old, new)
    if not changed:
        return False
    return all(
        i in package_version_lines(old) and i in package_version_lines(new)
        for i in changed
    )


def workspace_names(new_crate_tomls):
    names = set()
    for lines in new_crate_tomls:
        in_package = False
        for line in lines:
            if SECTION.match(line):
                in_package = line.strip() == "[package]"
            elif in_package:
                m = NAME_ASSIGN.match(line)
                if m:
                    names.add(m.group(1))
    return names


def cargo_lock_is_version_only(old, new, names):
    changed = differing(old, new)
    if not changed:
        return False
    for i in changed:
        if i == 0 or not VERSION_ASSIGN.match(old[i]) or not VERSION_ASSIGN.match(new[i]):
            return False
        before_old = NAME_ASSIGN.match(old[i - 1])
        before_new = NAME_ASSIGN.match(new[i - 1])
        if not before_old or not before_new:
            return False
        if before_old.group(1) != before_new.group(1) or before_old.group(1) not in names:
            return False
    return True


def masked_is_version_only(old, new, mask):
    changed = differing(old, new)
    if not changed:
        return False
    for i in changed:
        masked_old, hits_old = mask(old[i])
        masked_new, hits_new = mask(new[i])
        if not hits_old or not hits_new or masked_old != masked_new:
            return False
    return True


def mask_image_tag(line):
    return IMAGE_TAG.subn(r"\1<version>", line)


def mask_json_version(line):
    return JSON_VERSION.subn(r'\1"<version>"\2', line)


def is_version_only(statuses, read, manifests):
    if not statuses:
        return False
    crate_tomls = [p for p in statuses if CARGO_TOML.match(p)]
    allowed = set(crate_tomls) | {CARGO_LOCK, COMPOSE, *PLUGIN_JSONS}
    if any(p not in allowed or s != "M" for p, s in statuses.items()):
        return False
    names = None
    for path in statuses:
        old, new = read("old", path), read("new", path)
        if old is None or new is None:
            return False
        if CARGO_TOML.match(path):
            ok = cargo_toml_is_version_only(old, new)
        elif path == CARGO_LOCK:
            if names is None:
                names = workspace_names(read("new", p) or [] for p in manifests)
            ok = cargo_lock_is_version_only(old, new, names)
        elif path == COMPOSE:
            ok = masked_is_version_only(old, new, mask_image_tag)
        else:
            ok = masked_is_version_only(old, new, mask_json_version)
        if not ok:
            return False
    return True


def version_only_in_repo():
    try:
        git("rev-parse", "--verify", "--quiet", "HEAD^2")
        git("rev-parse", "--verify", "--quiet", "HEAD^1")
        raw = git("diff", "--name-status", "--no-renames", "-z", "HEAD^1", "HEAD")
    except (subprocess.CalledProcessError, OSError):
        return False
    parts = raw.split("\0")
    statuses = {}
    for status, path in zip(parts[0::2], parts[1::2]):
        statuses[path] = status
    revs = {"old": "HEAD^1", "new": "HEAD"}

    def read(side, path):
        try:
            return git("show", f"{revs[side]}:{path}").splitlines()
        except subprocess.CalledProcessError:
            return None

    try:
        manifests = [
            p
            for p in git("ls-tree", "-r", "--name-only", "HEAD").splitlines()
            if CARGO_TOML.match(p)
        ]
        return is_version_only(statuses, read, manifests)
    except (subprocess.CalledProcessError, OSError, UnicodeError):
        return False


def main():
    try:
        result = version_only_in_repo()
    except Exception:
        result = False
    print("true" if result else "false")
    return 0


if __name__ == "__main__":
    sys.exit(main())
