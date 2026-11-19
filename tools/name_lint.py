#!/usr/bin/env python3
"""Name lint: third-party product, company and AI-vendor names must not appear in the tree.

The term list is kept as SHA-256 digests (tools/name_lint/terms.txt) so the repository does
not itself carry the names. A line is split into words ([A-Za-z0-9]+); runs of up to four words
joined by one space, "-", "_" (written as " ") or "." (kept) are lower-cased and hashed, and only
whole runs count (not preceded or followed by a letter, digit or "_").
Format names (.rvt, .dwg, ...) and open standards are not on the list.

Usage: python tools/name_lint.py [paths...]   (default: every file tracked by git)
Exit 1 on any hit that tools/name_lint/allow.tsv does not justify.
"""
import fnmatch
import hashlib
import re
import subprocess
import sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
HERE = ROOT / "tools" / "name_lint"
WORD = re.compile(r"[A-Za-z0-9]+")
JOIN = re.compile(r"^[ ._-]?$")
MAX_WORDS = 4
SKIP_EXT = {".png", ".jpg", ".jpeg", ".gif", ".ico", ".ttf", ".otf", ".woff", ".woff2", ".pdf",
            ".zip", ".gz", ".ddc", ".glb", ".bin", ".wasm", ".exe", ".dll", ".lock"}
SKIP_DIRS = ("tools/name_lint/",)


_CACHE = {}


def digest(s):
    h = _CACHE.get(s)
    if h is None:
        h = _CACHE[s] = hashlib.sha256(s.encode("utf-8")).hexdigest()
    return h


FIRST = set()  # digests of words that start a multi-word term


def load_terms():
    exact, prefix = set(), {}
    for line in (HERE / "terms.txt").read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if not line or line.startswith("#"):
            continue
        kind, h = line.split(":", 1)
        if kind == "w":
            exact.add(h)
        elif kind == "f":
            FIRST.add(h)
        else:  # "p<n>": a word starting with the n-character stem
            prefix.setdefault(int(kind[1:]), set()).add(h)
    return exact, prefix


def load_allow():
    rows = []
    p = HERE / "allow.tsv"
    if p.exists():
        for line in p.read_text(encoding="utf-8").splitlines():
            if line.strip() and not line.startswith("#"):
                glob, term, _why = (line.split("\t") + ["", ""])[:3]
                rows.append((glob, term))
    return rows


def hits_in_line(text, exact, prefix):
    words = [(m.start(), m.end(), m.group().lower()) for m in WORD.finditer(text)]
    bound = lambda k: k < 0 or k >= len(text) or not (text[k].isalnum() or text[k] == "_")
    out = []
    for i in range(len(words)):
        if not bound(words[i][0] - 1):
            continue
        joined = words[i][2]
        last = i + MAX_WORDS if digest(joined) in FIRST else i + 1
        for j in range(i, min(last, len(words))):
            if j > i:
                sep = text[words[j - 1][1]:words[j][0]]
                if not JOIN.match(sep) or not sep:
                    break
                joined += ("." if sep == "." else " ") + words[j][2]
            if bound(words[j][1]):
                h = digest(joined)
                if h in exact:
                    out.append((h, text[words[i][0]:words[j][1]]))
        w = words[i][2]
        for n, hs in prefix.items():
            if len(w) >= n and digest(w[:n]) in hs:
                out.append((digest(w[:n]), text[words[i][0]:words[i][1]]))
    return out


def files(args):
    if args:
        return [Path(a) for a in args]
    ls = subprocess.run(["git", "ls-files", "-z"], cwd=ROOT, capture_output=True)
    if ls.returncode != 0 or not (ROOT / ".git").exists():  # an unpacked tree without git
        return [p for p in ROOT.rglob("*") if p.is_file()]
    return [ROOT / p for p in ls.stdout.decode("utf-8").split("\0") if p]


def main():
    exact, prefix = load_terms()
    allow = load_allow()
    n = 0
    for f in files(sys.argv[1:]):
        try:
            rel = f.resolve().relative_to(ROOT).as_posix()
        except ValueError:
            rel = f.as_posix()
        if f.suffix.lower() in SKIP_EXT or rel.startswith(SKIP_DIRS) or not f.is_file():
            continue
        try:
            text = f.read_bytes().decode("utf-8")
        except UnicodeDecodeError:
            continue
        vocab = set(WORD.findall(text.lower()))
        if not any(digest(w) in exact or digest(w) in FIRST
                   or any(len(w) >= n and digest(w[:n]) in hs for n, hs in prefix.items()) for w in vocab):
            continue  # no word of the file can start a hit
        for ln, line in enumerate(text.splitlines(), 1):
            for h, shown in hits_in_line(line, exact, prefix):
                if any(fnmatch.fnmatch(rel, g) and t in ("*", h) for g, t in allow):
                    continue
                n += 1
                print(f"{rel}:{ln}: third-party name '{shown}' (term {h[:12]})")
    print(f"name lint: {n} hit(s)")
    return 1 if n else 0


if __name__ == "__main__":
    sys.exit(main())
