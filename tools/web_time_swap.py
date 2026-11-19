# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Browser build clock swap: Instant/SystemTime/UNIX_EPOCH from `web_time` in the editor crates.

std::time::Instant::now() panics on wasm32; web_time re-exports std on desktop (no behaviour change).
Idempotent and byte-preserving (line endings, encoding): re-run after merging other branches.
Test files are left alone; crates/model/tests/web_clock.rs holds the rule.
Usage: python tools/web_time_swap.py [--dry]
"""
import pathlib
import re
import sys

ROOT = pathlib.Path(__file__).resolve().parent.parent
CRATES = "model geom solver ops io render app ui ai gen cli link script bridge-proto".split()
CLOCK = rb"(?:Instant|SystemTime|SystemTimeError|UNIX_EPOCH)"
ONE = re.compile(rb"std::time::(" + CLOCK + rb")\b")
GROUP = re.compile(rb"std::time::\{([^}]*)\}")
TEST_FILE = re.compile(r"^(tests?|_?tests_\w*|\w*_tests)\.rs$")
DEP = b"web-time.workspace = true"
WS_DEP = [b"# Instant/SystemTime that also run in the browser build (re-exports std on desktop)", b'web-time = "1"']


def swap(b: bytes) -> bytes:
    b = GROUP.sub(lambda m: b"web_time::{" + m.group(1) + b"}" if re.search(CLOCK, m.group(1)) else m.group(0), b)
    return ONE.sub(rb"web_time::\1", b)


def insert_after(path: pathlib.Path, prefix: bytes, lines: list, dry: bool) -> bool:
    b = path.read_bytes()
    if lines[-1] in b:
        return False
    nl = b"\r\n" if b"\r\n" in b else b"\n"
    ls = b.split(nl)
    i = next(k for k, x in enumerate(ls) if x.startswith(prefix))
    ls[i + 1:i + 1] = lines
    if not dry:
        path.write_bytes(nl.join(ls))
    return True


def main() -> None:
    dry = "--dry" in sys.argv
    files = lines = 0
    used = set()
    for c in CRATES:
        for p in sorted((ROOT / "crates" / c / "src").rglob("*.rs")):
            if TEST_FILE.match(p.name):
                continue
            b = p.read_bytes()
            if b"web_time::" in b:
                used.add(c)
            n = swap(b)
            if n != b:
                used.add(c)
                files += 1
                lines += sum(x != y for x, y in zip(b.split(b"\n"), n.split(b"\n")))
                if not dry:
                    p.write_bytes(n)
    deps = []
    if used:
        insert_after(ROOT / "Cargo.toml", b"rusqlite = ", WS_DEP, dry)
    for c in sorted(used):
        if insert_after(ROOT / "crates" / c / "Cargo.toml", b"[dependencies]", [DEP], dry):
            deps.append(c)
    print(f"files {files}, lines {lines}, new deps {deps}{' (dry)' if dry else ''}")


if __name__ == "__main__":
    main()
