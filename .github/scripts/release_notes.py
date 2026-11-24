import re
import subprocess
import sys
from pathlib import Path

VERSION_FILE = "crates/lr-proto/src/version.rs"
EXTRA_FILE = ".github/release-notes.md"
PATTERN = re.compile(r"PROTOCOL_VERSION:\s*Version\s*=\s*Version\s*\{\s*major:\s*(\d+)")


def protocol_major(text):
    found = PATTERN.search(text)
    return int(found.group(1)) if found else None


def breaking_line(previous, current, previous_tag):
    if previous is None or current is None or previous == current:
        return None
    return (
        f"**Breaking:** the major protocol version changes from {previous} to {current} "
        f"(since {previous_tag}). Roles on different major versions refuse each other: "
        "upgrade the broker, the hubs and the runners of every machine in the same window. "
        "A broker that runs as an OS service is not updated by `localrunner upgrade`: "
        "run `localrunner broker update-service` as administrator."
    )


def notes(previous, current, previous_tag, extra):
    parts = [breaking_line(previous, current, previous_tag), extra.strip() or None]
    return "\n\n".join(p for p in parts if p)


def git(repo, *args):
    done = subprocess.run(
        ["git", "-C", repo, *args], capture_output=True, text=True, encoding="utf-8"
    )
    return done.stdout if done.returncode == 0 else None


def main(repo, tag):
    previous_tag = (git(repo, "describe", "--tags", "--abbrev=0", "--match", "v*", f"{tag}^") or "").strip()
    previous = protocol_major(git(repo, "show", f"{previous_tag}:{VERSION_FILE}") or "") if previous_tag else None
    current = protocol_major(git(repo, "show", f"{tag}:{VERSION_FILE}") or "")
    extra_path = Path(repo) / EXTRA_FILE
    extra = extra_path.read_text(encoding="utf-8") if extra_path.is_file() else ""
    sys.stdout.write(notes(previous, current, previous_tag, extra))


if __name__ == "__main__":
    main(sys.argv[1], sys.argv[2])
