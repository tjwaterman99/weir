#!/usr/bin/env python3
"""Checks a built book: every include was resolved and every internal link leads somewhere.

    mdbook build docs && python3 docs/check-links.py docs/book
"""
import os
import re
import sys

root = sys.argv[1]
problems = []
pages = 0
for folder, _, files in os.walk(root):
    for name in files:
        if not name.endswith(".html"):
            continue
        pages += 1
        path = os.path.join(folder, name)
        html = open(path, encoding="utf-8").read()
        if "{{#include" in html:
            problems.append(f"{path}: an include was not resolved")
        main = re.search(r"<main>(.*?)</main>", html, re.S)
        for href in re.findall(r'href="([^"#]+)(?:#[^"]*)?"', main.group(1) if main else ""):
            if re.match(r"^(https?:|mailto:)", href):
                continue
            if not os.path.exists(os.path.normpath(os.path.join(folder, href))):
                problems.append(f"{path}: broken link to {href}")

for problem in problems:
    print(problem)
print(f"{pages} pages checked, {len(problems)} problems")
sys.exit(1 if problems else 0)
