#!/usr/bin/env python3
"""Pick, per entity kind, the smallest corpus DWG that contains it (read-only).

usage: python tools/dwg_kind_fixtures.py --list corpus/dwg.txt [--out crates/io/tests/data/dwg_kinds.tsv]

Kinds are found in the reference DXF stored next to each input.dwg (same folder, reference.dxf).
The tsv holds paths only, never file contents. Columns: path, kind, expected_lowered,
expected_dropped; the two counts are '?' until filled from the first real build.
"""
import argparse
import sys
from pathlib import Path

KINDS = ["spline", "ellipse", "mtext", "dimension", "leader", "solid", "proxy"]


def kind_of(rec):
    """Same buckets as the importer's dropped_kind (dxf/bridge.rs)."""
    k = rec.strip().upper()
    if k == "SPLINE":
        return "spline"
    if k == "ELLIPSE":
        return "ellipse"
    if k == "MTEXT":
        return "mtext"
    if k in ("DIMENSION", "ARC_DIMENSION", "LARGE_RADIAL_DIMENSION"):
        return "dimension"
    if k in ("LEADER", "MULTILEADER", "MLEADER", "TOLERANCE"):
        return "leader"
    if k in ("SOLID", "TRACE", "3DSOLID", "REGION", "BODY", "SURFACE"):
        return "solid"
    if "PROXY" in k or k.startswith("ACAD_") or k.startswith("ACDB"):
        return "proxy"
    return None


def kinds_in_dxf(path):
    """Set of kinds among ENTITIES and BLOCKS records of a DXF, streamed line by line."""
    found, section, after_section, code = set(), None, False, None
    with open(path, "r", encoding="latin-1", errors="replace") as f:
        for line in f:
            s = line.strip()
            if code is None:
                code = s
                continue
            if code == "0":
                after_section = s == "SECTION"
                if s == "ENDSEC":
                    section = None
                elif section in ("ENTITIES", "BLOCKS"):
                    k = kind_of(s)
                    if k:
                        found.add(k)
            elif code == "2" and after_section:
                section, after_section = s, False
            code = None
            if len(found) == len(KINDS):
                break
    return found


def pick(paths):
    """kind -> (size, dwg path) of the smallest dwg whose reference holds the kind."""
    best = {}
    for p in paths:
        p = Path(p)
        ref = p.with_name("reference.dxf")
        if not p.is_file() or not ref.is_file():
            continue
        size = p.stat().st_size
        for k in kinds_in_dxf(ref):
            if k not in best or size < best[k][0]:
                best[k] = (size, str(p))
    return best


def read_list(lst):
    out = []
    for line in Path(lst).read_text(encoding="utf-8").splitlines():
        line = line.strip()
        if line and not line.startswith("#"):
            out.append(line)
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--list", required=True)
    ap.add_argument("--out", default="crates/io/tests/data/dwg_kinds.tsv")
    a = ap.parse_args()
    best = pick(read_list(a.list))
    rows = ["path\tkind\texpected_lowered\texpected_dropped"]
    for k in KINDS:
        if k in best:
            rows.append(f"{best[k][1]}\t{k}\t?\t?")
        else:
            print(f"no file holds {k}", file=sys.stderr)
    Path(a.out).parent.mkdir(parents=True, exist_ok=True)
    Path(a.out).write_text("\n".join(rows) + "\n", encoding="utf-8", newline="\n")
    print(f"{len(rows) - 1} kinds -> {a.out}")


if __name__ == "__main__":
    main()
