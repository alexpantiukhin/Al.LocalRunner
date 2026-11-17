import re, sys


def read(p):
    return open(p, encoding="utf-8").read().replace("\r\n", "\n")


bad = []
en, ru = read("README.md"), read("README.ru.md")
h = lambda s: len(re.findall(r"(?m)^## ", s))
if h(en) != h(ru):
    bad.append(f"README sections differ: {h(en)} vs {h(ru)}")
ru_html, en_html = read("docs/architecture.html"), read("docs/architecture.en.html")
if ru_html.count("\n") != en_html.count("\n"):
    bad.append("architecture html pair: line counts differ")
ids = lambda s: re.findall(r'\bid="([^"]+)"', s)
if ids(ru_html) != ids(en_html):
    bad.append("architecture html pair: ids differ")
for t in ("h2", "h3", "table", "tr", "svg", "li", "pre"):
    a = len(re.findall(rf"<{t}[\s>]", ru_html))
    b = len(re.findall(rf"<{t}[\s>]", en_html))
    if a != b:
        bad.append(f"architecture html pair: <{t}> {a} vs {b}")
if re.search(r"[А-Яа-яЁё]", en_html):
    bad.append("architecture.en.html contains Cyrillic")
print("\n".join(bad) or "ok")
sys.exit(1 if bad else 0)
