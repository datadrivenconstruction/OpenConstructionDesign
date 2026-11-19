#!/usr/bin/env python3
"""Check of the glTF library passport: crates/model/data/gltf/manifest.tsv.

Format (TSV, first line is the header, `#` is a comment):
  file sha256 licence source author date source_rev licence_sha256 declared_at
- licence: `CC0-1.0` | `own`. Anything else (incl. CC-BY-4.0) is rejected until the licence policy review.
- CC0-1.0: source/declared_at is the https URL where the licence is declared; source_rev is the 40-hex
  source commit; licence_sha256 = sha256 of licenses/CC0-1.0.txt.
- own: source=own, source_rev/licence_sha256/declared_at = `-`.
Errors: file without a row, row without a file, hash mismatch, licence not allowed, empty field.
usage: python tools/assets_check.py [--dir DIR] | --self-test
"""
import hashlib, re, sys, tempfile, pathlib

COLS = ["file", "sha256", "licence", "source", "author", "date", "source_rev", "licence_sha256", "declared_at"]
ALLOWED = {"CC0-1.0": "licenses/CC0-1.0.txt", "own": None}
EXTS = {".glb", ".gltf", ".bin"}
HEX64, HEX40, DATE = re.compile(r"^[0-9a-f]{64}$"), re.compile(r"^[0-9a-f]{40}$"), re.compile(r"^\d{4}-\d{2}-\d{2}$")


def sha(p):
    return hashlib.sha256(pathlib.Path(p).read_bytes()).hexdigest()


def check(d):
    d, errs, seen = pathlib.Path(d), [], set()
    mf = d / "manifest.tsv"
    if not mf.is_file():
        return [f"{mf}: manifest.tsv missing"]
    lines = [l for l in mf.read_text(encoding="utf-8").splitlines() if l.strip() and not l.startswith("#")]
    if not lines or lines[0].split("\t") != COLS:
        return [f"manifest.tsv: header must be {' '.join(COLS)}"]
    for n, line in enumerate(lines[1:], 2):
        f = line.split("\t")
        if len(f) != len(COLS):
            errs.append(f"line {n}: {len(f)} fields, need {len(COLS)}")
            continue
        r = dict(zip(COLS, f))
        name = r["file"]
        e = lambda m: errs.append(f"{name}: {m}")
        if any(not v.strip() for v in f):
            e("empty field")
            continue
        if name in seen:
            e("duplicate row")
        seen.add(name)
        p = d / name
        if not p.is_file():
            e("file listed in the manifest is missing")
        elif not HEX64.match(r["sha256"]) or sha(p) != r["sha256"]:
            e("sha256 mismatch")
        if not DATE.match(r["date"]):
            e("date is not YYYY-MM-DD")
        lic = r["licence"]
        if lic not in ALLOWED:
            e(f"licence '{lic}' not allowed (only CC0-1.0 or own)")
        elif lic == "CC0-1.0":
            for k in ("source", "declared_at"):
                if not r[k].startswith("https://"):
                    e(f"{k} must be an https URL")
            if not HEX40.match(r["source_rev"]):
                e("source_rev is not a 40-hex commit")
            lt = d / ALLOWED[lic]
            if not lt.is_file() or r["licence_sha256"] != sha(lt):
                e("licence_sha256 does not match the licence text")
        else:
            if r["source"] != "own" or any(r[k] != "-" for k in ("source_rev", "licence_sha256", "declared_at")):
                e("for own: source=own, other passport fields `-`")
    for p in sorted(d.iterdir()):
        if p.suffix.lower() in EXTS and p.name not in seen:
            errs.append(f"{p.name}: file has no row in manifest.tsv")
    return errs


def self_test():
    cc0 = b"cc0 text"
    h = lambda b: hashlib.sha256(b).hexdigest()
    base = lambda lic="CC0-1.0", fh=None, lh=None: "\t".join(
        ["a.glb", fh or h(b"x"), lic, "https://e.x/a", "au", "2026-01-01", "a" * 40, lh or h(cc0), "https://e.x/r"])

    def run(row, extra=True):
        with tempfile.TemporaryDirectory() as t:
            t = pathlib.Path(t)
            (t / "licenses").mkdir()
            (t / "licenses/CC0-1.0.txt").write_bytes(cc0)
            (t / "a.glb").write_bytes(b"x")
            if extra:
                (t / "b.glb").write_bytes(b"y")
            (t / "manifest.tsv").write_text("\t".join(COLS) + "\n" + row + "\n", encoding="utf-8")
            return check(t)

    assert run(base(), extra=False) == [], "good manifest"
    assert any("no row" in x for x in run(base())), "missing row"
    assert any("sha256" in x for x in run(base(fh=h(b"z")), False)), "hash mismatch"
    assert any("not allowed" in x for x in run(base(lic="CC-BY-4.0"), False)), "bad licence"
    assert any("empty" in x for x in run(base().replace("au", " "), False)), "empty field"
    assert any("licence_sha256" in x for x in run(base(lh=h(b"q")), False)), "licence hash"
    print("assets_check self-test OK")


if __name__ == "__main__":
    if "--self-test" in sys.argv:
        self_test()
        sys.exit(0)
    d = sys.argv[sys.argv.index("--dir") + 1] if "--dir" in sys.argv else str(pathlib.Path(__file__).resolve().parent.parent / "crates/model/data/gltf")
    errs = check(d)
    print("\n".join(errs) if errs else "assets OK")
    sys.exit(1 if errs else 0)
