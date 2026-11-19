# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Builds THIRD_PARTY_LICENSES.txt and crates/app/data/licenses.txt from Cargo.lock + registry sources.

  python tools/gen_third_party.py           regenerate both files
  python tools/gen_third_party.py --check   fail on a non-allowed licence or a stale output (CI)

Shipped set = normal/build deps of the workspace for the win/mac/linux targets. Needs the
sources in the cargo registry (run `cargo fetch --locked` first).
"""
import json, os, re, subprocess, sys

ROOT = os.path.dirname(os.path.dirname(os.path.abspath(__file__)))
TARGETS = ["x86_64-pc-windows-msvc", "x86_64-apple-darwin", "aarch64-apple-darwin", "x86_64-unknown-linux-gnu"]
# Source of truth: legal/LICENSE_POLICY.md; keep in sync with deny.toml.
ALLOWED = {"MIT", "Apache-2.0", "BSD-2-Clause", "BSD-3-Clause", "ISC", "Zlib", "Unicode-3.0", "BSL-1.0",
           "CC0-1.0", "OFL-1.1", "Ubuntu-font-1.0", "MIT-0", "0BSD", "Unlicense"}
# Allowed only when the crate is shipped unmodified; always reported.
FLAGGED = {"MPL-2.0"}
LIC_FILE = re.compile(r"^(LICEN[CS]E|COPYING|UNLICEN[CS]E|NOTICE|OFL|UFL)|licen[cs]e.*\.txt$", re.I)
OUT_FULL = os.path.join(ROOT, "THIRD_PARTY_LICENSES.txt")
OUT_LIST = os.path.join(ROOT, "crates", "app", "data", "licenses.txt")

MIT_BODY = """Permission is hereby granted, free of charge, to any person obtaining a copy of this software and
associated documentation files (the "Software"), to deal in the Software without restriction, including
without limitation the rights to use, copy, modify, merge, publish, distribute, sublicense, and/or sell
copies of the Software, and to permit persons to whom the Software is furnished to do so, subject to the
following conditions:

The above copyright notice and this permission notice shall be included in all copies or substantial
portions of the Software.

THE SOFTWARE IS PROVIDED "AS IS", WITHOUT WARRANTY OF ANY KIND, EXPRESS OR IMPLIED, INCLUDING BUT NOT
LIMITED TO THE WARRANTIES OF MERCHANTABILITY, FITNESS FOR A PARTICULAR PURPOSE AND NONINFRINGEMENT. IN NO
EVENT SHALL THE AUTHORS OR COPYRIGHT HOLDERS BE LIABLE FOR ANY CLAIM, DAMAGES OR OTHER LIABILITY, WHETHER
IN AN ACTION OF CONTRACT, TORT OR OTHERWISE, ARISING FROM, OUT OF OR IN CONNECTION WITH THE SOFTWARE OR THE
USE OR OTHER DEALINGS IN THE SOFTWARE."""

# Not crates: (name, version, licence, text). "@path" reads a file relative to ROOT.
MANUAL = [
    ("SQLite", "bundled via rusqlite", "Public domain",
     "SQLite source code is in the public domain (https://sqlite.org/copyright.html). The rusqlite and\n"
     "libsqlite3-sys wrappers are MIT-licensed and listed with the crates above."),
    ("Lua", "5.4 bundled via mlua", "MIT",
     "Copyright (C) 1994-2024 Lua.org, PUC-Rio.\n\n" + MIT_BODY),
    ("Phosphor Icons", "2.1.1", "MIT", "Copyright (c) 2023 Phosphor Icons\n\n" + MIT_BODY),
    ("DDC Sans (derived from Inter)", "font files", "OFL-1.1", "@crates/ui/assets/fonts/OFL.txt"),
    ("IBM Plex Sans", "font files", "OFL-1.1", "@crates/ui/assets/fonts/OFL-IBMPlexSans.txt"),
    ("Noto Sans", "PDF export font", "OFL-1.1", "@crates/io/assets/fonts/OFL.txt"),
    ("Noto Sans interface fallback fonts", "subsets", "OFL-1.1",
     "Subsets of Noto Sans Arabic, Hebrew, Devanagari, Bengali, Thai, Latin (Vietnamese), Symbols 2,\n"
     "Math, Mono (block elements), Noto Emoji, and Noto Sans SC / JP / KR (crates/ui/assets/fonts/i18n/, built by\n"
     "tools/build_i18n_fonts.py from\n"
     "github.com/google/fonts). Copyright 2022 The Noto Project Authors (https://github.com/notofonts).\n"
     "Noto Sans Math: Copyright 2022 Google LLC. Noto Emoji: Copyright 2013 Google LLC.\n"
     "Licensed under the SIL Open Font License 1.1; the licence text is the one printed in the\n"
     "\"Noto Sans PDF export font\" section above and in crates/ui/assets/fonts/i18n/OFL-Noto.txt.\n"
     "The subsets are modified versions (instanced to Regular, reduced to the listed code points) and\n"
     "are distributed under the same licence, without the Reserved Font Name."),
    ("Content pack (Poly Haven models, ambientCG textures)", "", "CC0-1.0",
     "Credits, as a courtesy (CC0 requires no attribution): Poly Haven (polyhaven.com) models and\n"
     "ambientCG (ambientcg.com) textures, dedicated to the public domain under CC0 1.0. Per-item\n"
     "author, source URL and licence are recorded in the content pack's LICENCES.json."),
]


def metadata(target):
    r = subprocess.run(["cargo", "metadata", "--locked", "--offline", "--format-version", "1",
                        "--filter-platform", target], cwd=ROOT, capture_output=True, text=True, check=True)
    return json.loads(r.stdout)


def shipped():
    """Non-workspace crates reachable through non-dev edges on any target."""
    out = {}
    for t in TARGETS:
        m = metadata(t)
        pk = {p["id"]: p for p in m["packages"]}
        nodes = {n["id"]: n for n in m["resolve"]["nodes"]}
        seen, stack = set(), list(m["workspace_members"])
        while stack:
            i = stack.pop()
            if i in seen:
                continue
            seen.add(i)
            for d in nodes[i]["deps"]:
                if any(k["kind"] != "dev" for k in d["dep_kinds"]):
                    stack.append(d["pkg"])
        for i in seen:
            if pk[i].get("source"):
                out[i] = pk[i]
    return sorted(out.values(), key=lambda p: (p["name"].lower(), p["version"]))


def ok(expr):
    """(allowed, flagged ids). OR needs one allowed branch; AND needs all."""
    expr = re.sub(r"\s+WITH\s+\S+", "", expr).replace("/", " OR ")
    toks = re.findall(r"\(|\)|AND|OR|[A-Za-z0-9.\-+]+", expr)
    pos = 0
    flagged = set()

    def atom():
        nonlocal pos
        t = toks[pos]
        pos += 1
        if t == "(":
            v = orx()
            pos += 1
            return v
        t = t.rstrip("+")
        if t in FLAGGED:
            flagged.add(t)
        return t in ALLOWED or t in FLAGGED

    def andx():
        nonlocal pos
        v = atom()
        while pos < len(toks) and toks[pos] == "AND":
            pos += 1
            v = atom() and v
        return v

    def orx():
        nonlocal pos
        v = andx()
        while pos < len(toks) and toks[pos] == "OR":
            pos += 1
            v = andx() or v
        return v

    try:
        return orx(), flagged
    except IndexError:
        return False, flagged


def read_texts(p):
    d = os.path.dirname(p["manifest_path"])
    texts = []
    for dp, dn, fn in os.walk(d):
        dn[:] = [x for x in dn if x not in ("target", "tests", "benches", "examples", "src", ".git")]
        if os.path.relpath(dp, d).count(os.sep) > 1:
            dn[:] = []
        for f in sorted(fn):
            fp = os.path.join(dp, f)
            if (LIC_FILE.search(f) or (f.endswith(".txt") and os.path.basename(dp) == "fonts")) and os.path.getsize(fp) < 200_000:
                t = open(fp, encoding="utf-8", errors="replace").read().strip()
                if t:
                    texts.append(t.replace("\r\n", "\n"))
    return texts


def build():
    pkgs = shipped()
    bad, flagged = [], []
    for p in pkgs:
        lic = p.get("license") or ""
        good, fl = ok(lic) if lic else (False, set())
        if not good:
            bad.append(f"{p['name']} {p['version']}: {lic or 'NO LICENCE'}")
        if fl:
            flagged.append(f"{p['name']} {p['version']}: {lic}")
    groups = {}
    bare = {}
    for p in pkgs:
        texts = list(dict.fromkeys(read_texts(p)))
        for t in texts:
            groups.setdefault(t, []).append(f"{p['name']} {p['version']}")
        if not texts:
            bare.setdefault(p.get("license") or "unknown", []).append(f"{p['name']} {p['version']}")
    # packages published without a licence file: the SPDX licence their manifest declares
    for lic, users in bare.items():
        groups.setdefault(
            f"Licence: {lic}, as declared in the package manifest. These packages are published\n"
            "without a licence file.", []).extend(users)
    lst = "".join(f"{p['name']}|{p['version']}|{p.get('license') or 'unknown'}\n" for p in pkgs)
    lst += "".join(f"{n}|{v or '-'}|{l}\n" for n, v, l, _ in MANUAL)
    bar = "-" * 78
    full = ["DDC Editor third-party licences (generated by tools/gen_third_party.py; do not edit).\n"
            "Targets: " + ", ".join(TARGETS) + ".\n"
            "Where a crate offers a choice (MIT OR Apache-2.0 ...), DDC Editor uses it under the MIT or\n"
            "Apache-2.0 terms; the LGPL alternative of r-efi is not used.\n",
            "=" * 78 + "\nCrates\n" + "=" * 78]
    for t, users in sorted(groups.items(), key=lambda kv: kv[1][0].lower()):
        full.append(f"{bar}\nUsed by: " + ", ".join(users) + f"\n{bar}\n" + t + "\n")
    full.append("=" * 78 + "\nBundled components and assets\n" + "=" * 78)
    for n, v, l, body in MANUAL:
        if body.startswith("@"):
            body = open(os.path.join(ROOT, body[1:]), encoding="utf-8").read().strip()
        full.append(f"{bar}\n{n} {v} ({l})\n{bar}\n{body}\n")
    return lst, "\n".join(full), bad, flagged


def main():
    lst, full, bad, flagged = build()
    for f in flagged:
        print("FLAG (MPL-2.0 only if unmodified):", f)
    if bad:
        print("NOT ALLOWED:\n  " + "\n  ".join(bad))
        return 1
    if "--check" in sys.argv:
        cur = open(OUT_LIST, encoding="utf-8").read().replace("\r\n", "\n") if os.path.exists(OUT_LIST) else ""
        if cur != lst:
            print("licenses.txt is stale: run python tools/gen_third_party.py")
            return 1
        print("LICENCE CHECK PASS")
        return 0
    open(OUT_LIST, "w", encoding="utf-8", newline="\n").write(lst)
    open(OUT_FULL, "w", encoding="utf-8", newline="\n").write(full)
    print(f"wrote {OUT_LIST} and {OUT_FULL} ({len(full) // 1024} KiB)")
    return 0


if __name__ == "__main__":
    sys.exit(main())
