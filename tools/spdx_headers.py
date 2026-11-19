#!/usr/bin/env python3
"""SPDX headers for authored source files (header format: SPDX-License-Identifier and SPDX-FileCopyrightText lines).

  python tools/spdx_headers.py                 list files without a header (exit 1 if any)
  python tools/spdx_headers.py --apply         write the headers
  options: --license AGPL-3.0-only  --holder "Name"  --year 2026

Only git-tracked files; keeps line endings and a UTF-8 BOM; skips _archive/, generated files,
test data and files that already carry an SPDX-License-Identifier.
"""
import argparse
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parents[1]
COMMENT = {".rs": "//", ".wgsl": "//", ".py": "#", ".ps1": "#", ".sh": "#"}
SKIP_DIRS = ("_archive/", "target/", "crates/io/tests/data/", "tests/data/", "tests/fixtures/")
GENERATED = ("generated", "do not edit", "@generated")


def tracked():
    out = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True, check=True).stdout
    return [p for p in out.decode("utf-8").split("\0") if p]


def wants_header(rel, text):
    if Path(rel).suffix not in COMMENT or any(d in rel for d in SKIP_DIRS):
        return False
    head = text[:600].lower()
    return "spdx-license-identifier" not in head and not any(g in head for g in GENERATED)


def with_header(text, prefix, lic, holder, year):
    bom = "﻿" if text.startswith("﻿") else ""
    body = text[len(bom):]
    nl = "\r\n" if "\r\n" in body[:2000] else "\n"
    header = f"{prefix} SPDX-License-Identifier: {lic}{nl}{prefix} SPDX-FileCopyrightText: {year} {holder}{nl}"
    first, sep, rest = body.partition(nl)
    if first.startswith("#!") or first.startswith("# -*- coding"):
        return bom + first + sep + header + rest
    return bom + header + body


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--apply", action="store_true")
    ap.add_argument("--license", default="AGPL-3.0-only")
    ap.add_argument("--holder", default="Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors")
    ap.add_argument("--year", default="2026")
    a = ap.parse_args()
    if a.apply and a.holder.startswith("["):
        sys.exit("set --holder to the copyright holder's legal name before --apply")
    todo = []
    for rel in tracked():
        p = ROOT / rel
        try:
            text = p.read_bytes().decode("utf-8")
        except (UnicodeDecodeError, FileNotFoundError):
            continue
        if wants_header(rel, text):
            todo.append(rel)
            if a.apply:
                new = with_header(text, COMMENT[Path(rel).suffix], a.license, a.holder, a.year)
                p.write_bytes(new.encode("utf-8"))
    by_ext = {}
    for rel in todo:
        by_ext[Path(rel).suffix] = by_ext.get(Path(rel).suffix, 0) + 1
    print(("headers written: " if a.apply else "files without a header: ") + str(len(todo)), by_ext)
    return 0 if a.apply or not todo else 1


if __name__ == "__main__":
    sys.exit(main())
