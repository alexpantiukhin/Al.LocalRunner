import glob, re, sys

bad = []
for f in sorted(glob.glob(".github/workflows/*.yml") + glob.glob(".github/workflows/*.yaml")):
    text = open(f, encoding="utf-8").read()
    for n, line in enumerate(text.split(chr(10)), 1):
        if re.search("[" + chr(0) + "-" + chr(8) + chr(11) + chr(12) + chr(14) + "-" + chr(31) + chr(127) + "]", line):
            bad.append(f"{f}:{n}: control character")
print(chr(10).join(bad) or "ok")
sys.exit(1 if bad else 0)
