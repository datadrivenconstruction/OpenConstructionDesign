# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Fail if any workspace Cargo.toml can pull in closed code.

Closed bridges live in other repos and talk to the editor only over the published protocol,
so no manifest may have: path deps outside the repo, git deps, non-default registries,
dependency or feature names that look like closed modules.
Usage: python tools/closed_deps_check.py [repo_root]
"""
import os
import re
import sys
import tomllib

CLOSED_NAME = re.compile(r"(^|[-_])(rvt|rfa|closed|private|proprietary)([-_]|$)", re.I)
SKIP_DIRS = {"target", "_archive", ".git", "node_modules"}


def dep_tables(doc):
    for key in ("dependencies", "dev-dependencies", "build-dependencies"):
        yield key, doc.get(key, {})
    yield "workspace.dependencies", doc.get("workspace", {}).get("dependencies", {})
    for tgt, body in doc.get("target", {}).items():
        for key in ("dependencies", "dev-dependencies", "build-dependencies"):
            yield f"target.{tgt}.{key}", body.get(key, {})


def check_manifest(path, root):
    doc = tomllib.load(open(path, "rb"))
    rel = os.path.relpath(path, root)
    here = os.path.dirname(path)
    hits = []
    for table, deps in dep_tables(doc):
        for name, spec in deps.items():
            spec = spec if isinstance(spec, dict) else {}
            real = spec.get("package", name)
            if CLOSED_NAME.search(name) or CLOSED_NAME.search(real):
                hits.append(f"{rel}: [{table}] {name}: closed-looking name")
            if "git" in spec:
                hits.append(f"{rel}: [{table}] {name}: git dependency")
            if "registry" in spec or "registry-index" in spec:
                hits.append(f"{rel}: [{table}] {name}: non-default registry")
            if "path" in spec:
                target = os.path.realpath(os.path.join(here, spec["path"]))
                if os.path.commonpath([target, os.path.realpath(root)]) != os.path.realpath(root):
                    hits.append(f"{rel}: [{table}] {name}: path outside repo")
    for feat in doc.get("features", {}):
        if CLOSED_NAME.search(feat):
            hits.append(f"{rel}: feature {feat}: closed-looking name")
    return hits


def scan(root):
    hits = []
    for d, dirs, files in os.walk(root):
        dirs[:] = [x for x in dirs if x not in SKIP_DIRS]
        if "Cargo.toml" in files:
            hits += check_manifest(os.path.join(d, "Cargo.toml"), root)
    return hits


if __name__ == "__main__":
    root = os.path.abspath(sys.argv[1] if len(sys.argv) > 1 else ".")
    found = scan(root)
    print("\n".join(found) or "CLOSED DEPS CHECK PASS")
    sys.exit(1 if found else 0)
