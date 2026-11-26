# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Manifest `ddc-web-editor/1` of the browser bundle (stdlib only).

  assets <dist> [--ui crates/ui]                    on-demand files: i18n/<code>.json (locales other than
                                                    en/ru, fragments merged) and fonts/<file> (script faces)
  write <dist> --version V --entry ddc-editor.js   every file under <dist> with size and SHA-256
  check <dist>                                      manifest.json against the schema, then each file
  release <dist> --tag vX.Y.Z [--out DIR]           release zip (files + .br/.gz) and latest-web.json feed
                                                    (`ddc-web-release/1`); .br needs `pip install brotli`

The schema is the contract's `web-editor.schema.json` (copy beside this script); the checker
covers the keywords that schema uses.
"""
import argparse
import datetime
import gzip
import hashlib
import json
import re
import sys
import zipfile
from pathlib import Path

HERE = Path(__file__).resolve().parent
SCHEMA = HERE / "web-editor.schema.json"
RELEASE_SCHEMA = HERE / "web-release.schema.json"
REPO = "datadrivenconstruction/OpenConstructionDesign"
PRECOMPRESS = (".wasm", ".js", ".json", ".ttf")  # the ERP serves these precompressed, it does not compress on the fly
MANIFEST = "manifest.json"
BRIDGE_MAJORS = [1]
# fetched by the page when the interface language needs them (crates/ui/src/assets.rs), not at start
ON_DEMAND = ("i18n/", "fonts/")
# compiled into the browser build: Script::always in crates/ui/src/i18n_fonts.rs, en/ru in crates/ui/build.rs
BUILT_IN_FONTS = {"NotoSansSymbols2.ttf", "NotoSansMath.ttf", "NotoSansMono.ttf", "NotoEmoji.ttf"}
BUILT_IN_LOCALES = {"en", "ru"}


def _is(doc, t):
    if t == "integer":
        return isinstance(doc, int) and not isinstance(doc, bool)
    if t == "number":
        return isinstance(doc, (int, float)) and not isinstance(doc, bool)
    kinds = {"object": dict, "array": list, "string": str, "boolean": bool, "null": type(None)}
    return isinstance(doc, kinds[t])


def schema_errors(doc, schema, at="", root=None):
    """Problems of `doc` under the JSON Schema subset the contract schemas use (manifest and
    bridge messages: type, const, enum, $ref, anyOf, oneOf, bounds, pattern); [] = valid."""
    root = root if root is not None else schema
    if "$ref" in schema:
        node = root
        for part in schema["$ref"].lstrip("#/").split("/"):
            node = node[part]
        return schema_errors(doc, node, at, root)
    out = []
    t = schema.get("type")
    if t is not None and not any(_is(doc, x) for x in (t if isinstance(t, list) else [t])):
        return [f"{at or '/'}: not {t}"]
    for key, need in (("anyOf", lambda n: n >= 1), ("oneOf", lambda n: n == 1)):
        if key in schema:
            n = sum(not schema_errors(doc, s, at, root) for s in schema[key])
            if not need(n):
                out.append(f"{at or '/'}: {key} matches {n} branches")
    if "const" in schema and doc != schema["const"]:
        out.append(f"{at or '/'}: must be {schema['const']!r}")
    if "enum" in schema and doc not in schema["enum"]:
        out.append(f"{at or '/'}: not one of {schema['enum']}")
    if _is(doc, "number") and doc > schema.get("maximum", doc):
        out.append(f"{at}: above {schema['maximum']}")
    if isinstance(doc, str):
        if len(doc) < schema.get("minLength", 0) or len(doc) > schema.get("maxLength", 1 << 30):
            out.append(f"{at}: length out of range")
        if "pattern" in schema and not re.search(schema["pattern"], doc):
            out.append(f"{at}: does not match {schema['pattern']}")
    if _is(doc, "number") and doc < schema.get("minimum", doc):
        out.append(f"{at}: below {schema['minimum']}")
    if isinstance(doc, list):
        if len(doc) < schema.get("minItems", 0) or len(doc) > schema.get("maxItems", 1 << 30):
            out.append(f"{at}: item count out of range")
        for i, v in enumerate(doc):
            out += schema_errors(v, schema.get("items", {}), f"{at}/{i}", root)
    if isinstance(doc, dict):
        props = schema.get("properties", {})
        for k in schema.get("required", []):
            if k not in doc:
                out.append(f"{at}/{k}: missing")
        for k, v in doc.items():
            if k in props:
                out += schema_errors(v, props[k], f"{at}/{k}", root)
            elif schema.get("additionalProperties") is False:
                out.append(f"{at}/{k}: not allowed")
    return out


def sha256(p):
    h = hashlib.sha256()
    with open(p, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 20), b""):
            h.update(chunk)
    return h.hexdigest()


def bundle_files(dist):
    return sorted(
        p for p in Path(dist).rglob("*") if p.is_file() and p.name != MANIFEST
    )


def assets(dist, ui):
    """Write the on-demand files into `dist`; return their paths. A locale's fragments merge in
    file-name order, later keys winning, as `i18n::load` does."""
    dist, ui = Path(dist), Path(ui)
    out = []
    for d in sorted(p for p in (ui / "data/i18n").iterdir() if p.is_dir() and p.name not in BUILT_IN_LOCALES):
        merged = {}
        for f in sorted(d.glob("*.json")):
            merged.update(json.loads(f.read_text(encoding="utf-8")))
        if not merged:
            continue
        rel = f"i18n/{d.name}.json"
        (dist / "i18n").mkdir(parents=True, exist_ok=True)
        (dist / rel).write_text(json.dumps(merged, ensure_ascii=False, separators=(",", ":"), sort_keys=True),
                                encoding="utf-8")
        out.append(rel)
    for f in sorted((ui / "assets/fonts/i18n").glob("*.ttf")):
        if f.name in BUILT_IN_FONTS:
            continue
        rel = f"fonts/{f.name}"
        (dist / "fonts").mkdir(parents=True, exist_ok=True)
        (dist / rel).write_bytes(f.read_bytes())
        out.append(rel)
    return out


def build(dist, version, entry):
    dist = Path(dist)
    files = []
    for p in bundle_files(dist):
        rel = p.relative_to(dist).as_posix()
        f = {"path": rel, "bytes": p.stat().st_size, "sha256": sha256(p)}
        if rel.startswith(ON_DEMAND):
            f["load"] = "on_demand"
        files.append(f)
    return {"schema": "ddc-web-editor/1", "version": version, "bridge": BRIDGE_MAJORS,
            "entry": entry, "files": files}


def check(dist, schema=None):
    """Problems of `<dist>/manifest.json`: schema, entry listed, every file present with its size and hash."""
    dist = Path(dist)
    schema = schema or json.loads(SCHEMA.read_text(encoding="utf-8"))
    try:
        doc = json.loads((dist / MANIFEST).read_text(encoding="utf-8"))
    except (OSError, ValueError) as e:
        return [f"{MANIFEST}: {e}"]
    out = schema_errors(doc, schema)
    if out:
        return out
    listed = {f["path"]: f for f in doc["files"]}
    if len(listed) != len(doc["files"]):
        out.append("/files: a path is listed twice")
    if doc["entry"] not in listed:
        out.append(f"/entry: {doc['entry']} is not among the files")
    for path, f in listed.items():
        p = dist / path
        if not p.is_file():
            out.append(f"{path}: missing")
        elif p.stat().st_size != f["bytes"]:
            out.append(f"{path}: {p.stat().st_size} bytes, manifest says {f['bytes']}")
        elif sha256(p) != f["sha256"]:
            out.append(f"{path}: SHA-256 differs")
    on_disk = {p.relative_to(dist).as_posix() for p in bundle_files(dist)}
    for extra in sorted(on_disk - listed.keys()):
        out.append(f"{extra}: on disk but not in the manifest")
    return out


def release(dist, tag, out, repo=REPO, now=None):
    """Write `<out>/OpenConstructionDesign-<tag>-web.zip` and `<out>/latest-web.json`; return the feed."""
    dist, out = Path(dist), Path(out)
    problems = check(dist)
    if problems:
        raise ValueError("; ".join(problems))
    doc = json.loads((dist / MANIFEST).read_text(encoding="utf-8"))
    if tag != f"v{doc['version']}":
        raise ValueError(f"tag {tag} does not match manifest version {doc['version']}")
    import brotli  # MIT; only the release needs it

    out.mkdir(parents=True, exist_ok=True)
    name = f"OpenConstructionDesign-{tag}-web.zip"
    zpath = out / name
    with zipfile.ZipFile(zpath, "w", zipfile.ZIP_DEFLATED, compresslevel=9) as z:
        z.write(dist / MANIFEST, MANIFEST)
        for f in doc["files"]:
            raw = (dist / f["path"]).read_bytes()
            z.writestr(f["path"], raw)
            if f["path"].endswith(PRECOMPRESS):
                z.writestr(f["path"] + ".br", brotli.compress(raw, quality=11, lgwin=24), zipfile.ZIP_STORED)
                z.writestr(f["path"] + ".gz", gzip.compress(raw, 9, mtime=0), zipfile.ZIP_STORED)
    base = f"https://github.com/{repo}/releases/download/{tag}/{name}"
    now = now or datetime.datetime.now(datetime.timezone.utc)
    feed = {"schema": "ddc-web-release/1", "version": doc["version"], "tag": tag, "bridge": doc["bridge"],
            "zip": {"url": base, "bytes": zpath.stat().st_size, "sha256": sha256(zpath)},
            "signature_url": base + ".minisig", "published_at": now.strftime("%Y-%m-%dT%H:%M:%SZ")}
    errors = schema_errors(feed, json.loads(RELEASE_SCHEMA.read_text(encoding="utf-8")))
    if errors:
        raise ValueError("; ".join(errors))
    (out / "latest-web.json").write_text(json.dumps(feed, indent=2) + "\n", encoding="utf-8")
    return feed


def main(argv=None):
    ap = argparse.ArgumentParser(description=__doc__.splitlines()[0])
    sub = ap.add_subparsers(dest="cmd", required=True)
    w = sub.add_parser("write")
    w.add_argument("dist")
    w.add_argument("--version", required=True)
    w.add_argument("--entry", required=True)
    c = sub.add_parser("check")
    c.add_argument("dist")
    r = sub.add_parser("release")
    r.add_argument("dist")
    r.add_argument("--tag", required=True)
    r.add_argument("--out", default="dist/web-release")
    s = sub.add_parser("assets")
    s.add_argument("dist")
    s.add_argument("--ui", default=str(HERE.parent.parent / "crates/ui"))
    a = ap.parse_args(argv)
    if a.cmd == "assets":
        paths = assets(a.dist, a.ui)
        size = sum((Path(a.dist) / p).stat().st_size for p in paths)
        print(f"assets ok: {len(paths)} on-demand files, {size} bytes")
        return 0
    if a.cmd == "release":
        try:
            feed = release(a.dist, a.tag, a.out)
        except (ValueError, OSError) as e:
            print(e, file=sys.stderr)
            return 1
        print(f"release ok: {feed['zip']['url'].rsplit('/', 1)[1]}, {feed['zip']['bytes']} bytes")
        return 0
    if a.cmd == "write":
        doc = build(a.dist, a.version, a.entry)
        (Path(a.dist) / MANIFEST).write_text(json.dumps(doc, indent=2) + "\n", encoding="utf-8")
    problems = check(a.dist)
    for p in problems:
        print(p, file=sys.stderr)
    if not problems:
        doc = json.loads((Path(a.dist) / MANIFEST).read_text(encoding="utf-8"))
        total = sum(f["bytes"] for f in doc["files"])
        print(f"manifest ok: {len(doc['files'])} files, {total} bytes, entry {doc['entry']}")
    return 1 if problems else 0


if __name__ == "__main__":
    sys.exit(main())
