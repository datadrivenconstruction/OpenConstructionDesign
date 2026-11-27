# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Fail when README.md names a repository path that does not exist.

Checks markdown link targets, html src/srcset/href values and backticked or command-line
paths that start with a top-level repository directory (crates/, docs/, tools/, ...).
Usage: python tools/readme_paths.py [README.md]
"""
import re
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
TOPS = sorted(p.name for p in ROOT.iterdir() if p.is_dir() and not p.name.startswith(".git") and p.name not in ("target", "node_modules"))
TOPS += [".github"]


# output folders that a README command creates
CREATED = {"tools/web/pkg"}


def candidates(text: str):
    for m in re.finditer(r"\]\(([^)\s]+)", text):
        yield m.group(1)
    for m in re.finditer(r"""(?:src|srcset|href)=["']([^"'\s]+)""", text):
        yield m.group(1)
    top = "|".join(re.escape(t) for t in TOPS)
    for m in re.finditer(rf"(?<![\w/.-])((?:{top})/[\w./@+-]*[\w])", text):
        yield m.group(1)


def missing(readme: Path):
    out = set()
    for c in candidates(readme.read_text(encoding="utf-8")):
        if re.match(r"^[a-z][a-z0-9+.-]*:", c) or c.startswith(("#", "//")):
            continue
        c = c.split("#")[0].split("?")[0]
        if not c or c in CREATED or any(ch in c for ch in "<>*{}$…"):
            continue
        if not (ROOT / c.lstrip("/")).exists():
            out.add(c)
    return sorted(out)


def main():
    readme = Path(sys.argv[1]) if len(sys.argv) > 1 else ROOT / "README.md"
    bad = missing(readme)
    for b in bad:
        print(f"{readme.name}: missing path {b}")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
