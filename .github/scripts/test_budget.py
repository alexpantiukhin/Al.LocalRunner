import os, re, subprocess, sys, tomllib

KINDS = ("unit", "integration", "e2e")
RUNNING = re.compile(r"^\s*Running (unittests|\S+)")
FINISHED = re.compile(r"^test result: .* finished in ([0-9.]+)s")


def kind_of(running):
    if running == "unittests":
        return "unit"
    path = running.replace(chr(92), "/")
    if "/e2e/" in path:
        return "e2e"
    return "integration"


def totals(lines):
    sums = dict.fromkeys(KINDS, 0.0)
    kind = None
    for line in lines:
        m = RUNNING.match(line)
        if m:
            kind = kind_of(m.group(1))
            continue
        m = FINISHED.match(line)
        if m and kind:
            sums[kind] += float(m.group(1))
    return sums


def verdict(sums, budgets):
    out, over = [], False
    for k in KINDS:
        limit = budgets[k]
        bad = sums[k] > limit
        over |= bad
        out.append(f"{k:<12} {sums[k]:7.1f}s of {limit}s {'OVER BUDGET' if bad else 'ok'}")
    return out, over


def with_overrides(budgets):
    out = dict(budgets)
    for k in KINDS:
        value = os.environ.get(f"TEST_BUDGET_{k.upper()}_S", "").strip()
        if value:
            out[k] = float(value)
    return out


def main(argv):
    with open(".github/test-budgets.toml", "rb") as f:
        budgets = with_overrides(tomllib.load(f)["budget_s"])
    if argv[0] == "--from-log":
        lines = open(argv[1], encoding="utf-8", errors="replace").read().splitlines()
        code = 0
    else:
        proc = subprocess.Popen(argv, stdout=subprocess.PIPE, stderr=subprocess.STDOUT, text=True, errors="replace")
        lines = []
        for line in proc.stdout:
            sys.stdout.write(line)
            lines.append(line.rstrip(chr(10)))
        code = proc.wait()
    report, over = verdict(totals(lines), budgets)
    print(chr(10).join(["", "test time budgets:"] + report))
    return code or (1 if over else 0)


sys.exit(main(sys.argv[1:]))
