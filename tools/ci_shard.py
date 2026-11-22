# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Split the tests of one crate into N CI shards that together run exactly what `cargo test` runs.

  python tools/ci_shard.py plan  <crate> <k>/<n>   print the units of shard k
  python tools/ci_shard.py build <crate> <k>/<n>   compile what shard k runs
  python tools/ci_shard.py run   <crate> <k>/<n>   run shard k (every unit, failures collected)
  python tools/ci_shard.py check <crate> <n>       assert the n shards cover every target once

Targets come from `cargo metadata`, so a new test file is always picked up. The lib of a crate
listed in LIB_GROUPS is cut by test-name filters; its rest unit skips the same strings, so the
lib tests are split exactly. Units go to shards by measured minutes (largest first). A test
binary `tests/<dir>/main.rs` weighs the sum of its member files, which keep their own entries.
"""
import json
import os
import re
import subprocess
import sys

# Minutes per target on a 4-core CI runner (debug build); unlisted targets weigh DEFAULT_MIN.
WEIGHTS = {
    "ddc-gen": {
        "lib": 2, "locks": 30, "hosted": 18, "lift": 12, "add_storey": 11, "openings": 8,
        "keys": 6.5, "bench": 6.5, "lattice_tower_t01": 6, "dc": 4.5, "fire_tower": 3.5,
        "assembly": 3.2, "conflict": 2.3, "fire_egress": 2, "fire": 1.6, "golden": 1.6,
        "design": 1.4, "hvac3": 1.4, "levels": 1.4, "plant": 1.2, "checks": 1.2, "brief": 1,
        "eval": 1, "landscape": 1, "anygeo": 0.8, "freeform": 0.7, "basement": 0.6,
        "hvac2": 0.6, "foundation": 0.4, "curve": 0.3, "mep5": 0.3, "place": 0.3,
        "apartment_counts": 0.1, "block": 0.1, "bridge_approach": 0.1, "gap_kinds": 0.1,
        "h05_defaults": 0.1, "hangar": 0.1, "longspan": 0.1,
    },
    "ddc-app": {
        "lib:qa": 21, "lib:layout": 25, "lib:ai": 12, "lib:rest": 28,
        "command_invariants": 25, "e2e_ai_checkpoints": 4, "ai_contract": 1, "aifam_eval": 0.1, "big_bodies": 0.1,
        "big_model": 0.1, "big_model_rows": 0.1, "conn_bench": 0.1, "detail_bench": 0.1,
        "dialog_probe_coverage": 0.1, "e2e_ai_anygeo": 0.1,
    },
}
DEFAULT_MIN = {"ddc-gen": 3.0, "ddc-app": 1.5}

# Lib test-name filters (substring match, as libtest does); one unit per group plus a rest unit.
LIB_GROUPS = {
    "ddc-app": {
        "lib:qa": ["qa_app_layout::"],
        "lib:layout": ["layout_contract::", "layout_fold::", "layout_looks_tests::"],
        "lib:ai": ["ai::"],
    },
}


def metadata(crate):
    out = subprocess.run(["cargo", "metadata", "--no-deps", "--format-version", "1", "--locked"],
                         capture_output=True, check=True).stdout
    for pkg in json.loads(out)["packages"]:
        if pkg["name"] == crate:
            return pkg
    sys.exit(f"ci_shard: no package {crate}")


def default_features(pkg):
    feats, on, todo = pkg.get("features", {}), set(), ["default"]
    while todo:
        f = todo.pop()
        if f in on or f not in feats:
            continue
        on.add(f)
        todo.extend(x for x in feats[f] if "/" not in x and not x.startswith("dep:"))
    return on


def units(pkg):
    """Every unit plain `cargo test -p <crate>` runs: (key, kind, arg)."""
    crate, on, out = pkg["name"], default_features(pkg), []
    for t in sorted(pkg["targets"], key=lambda t: (t["kind"], t["name"])):
        if not set(t.get("required-features", [])) <= on:
            continue  # the default run skips these too
        kind = t["kind"][0]
        if kind in ("lib", "rlib", "cdylib", "staticlib", "dylib", "proc-macro"):
            if t.get("test", True):
                groups = LIB_GROUPS.get(crate)
                if groups:
                    out += [(g, "lib", tuple(f)) for g, f in groups.items()]
                    out.append(("lib:rest", "lib-rest", tuple(s for f in groups.values() for s in f)))
                else:
                    out.append(("lib", "lib", None))
            if t.get("doctest", True):
                out.append(("doc", "doc", None))
        elif kind == "bin" and t.get("test", True):
            out.append((f"bin:{t['name']}", "bin", t["name"]))
        elif kind == "test" and t.get("test", True):
            out.append((t["name"], "test", t["name"]))
        elif kind == "example":
            out.append(("examples", "examples", None))
    return list(dict.fromkeys(out))  # one examples unit however many examples


def members(src):
    """Module files of a `tests/<dir>/main.rs` binary; each must be declared there exactly once.
    A `#[path]` module is a shared helper from outside the directory: neither a member nor weighed."""
    if os.path.basename(src) != "main.rs":
        return None
    d = os.path.dirname(src)
    with open(src, encoding="utf-8") as f:
        mods = [m for p, m in re.findall(r"^(#\[path[^\n]*\n)?mod (\w+);", f.read(), re.M) if not p]
    files = sorted(x[:-3] for x in os.listdir(d) if x.endswith(".rs") and x != "main.rs")
    assert sorted(mods) == files, f"{src}: mod lines {sorted(mods)} differ from files {files}"
    return mods


def assign(pkg, n):
    crate, w = pkg["name"], WEIGHTS.get(pkg["name"], {})
    default = DEFAULT_MIN.get(crate, 1.0)
    groups = {t["name"]: m for t in pkg["targets"] if "test" in t["kind"]
              for m in [members(t["src_path"])] if m}
    def weight(key):
        if key in groups:
            return sum(w.get(m, default) for m in groups[key])
        return w.get(key, 0.2 if key in ("doc", "examples") or key.startswith("bin:") else default)
    shards = [[] for _ in range(n)]
    load = [0.0] * n
    for u in sorted(units(pkg), key=lambda u: (-weight(u[0]), u[0])):
        i = min(range(n), key=lambda j: (load[j], j))
        shards[i].append(u)
        load[i] += weight(u[0])
    return shards, load


def commands(crate, shard, build):
    base = ["cargo", "test", "-p", crate, "--locked", "--no-fail-fast"]
    cmds, targets = [], []
    for _, kind, arg in shard:
        if kind == "lib":
            cmds.append(base + ["--lib"] + (["--", *arg] if arg else []))
        elif kind == "lib-rest":
            cmds.append(base + ["--lib", "--"] + [x for s in arg for x in ("--skip", s)])
        elif kind == "doc":
            cmds.append(base + ["--doc"])
        elif kind == "examples":
            cmds.append(["cargo", "build", "-p", crate, "--locked", "--examples"])
        else:
            targets += [f"--{kind}", arg]
    if targets:
        cmds.append(base + targets)
    if build:
        libs = any(k in ("lib", "lib-rest") for _, k, _ in shard)
        tgt = (["--lib"] if libs else []) + targets
        cmds = [c for c in cmds if c[1] == "build"]
        if tgt:
            cmds.insert(0, ["cargo", "test", "-p", crate, "--locked", "--no-run"] + tgt)
    return cmds


def check(pkg, n):
    all_units = units(pkg)
    shards, load = assign(pkg, n)
    got = [u for s in shards for u in s]
    assert sorted(got) == sorted(all_units) and len(got) == len(set(got)), "shards differ from targets"
    groups = LIB_GROUPS.get(pkg["name"], {})
    rest = [u for u in all_units if u[1] == "lib-rest"]
    assert not groups or (len(rest) == 1 and sorted(rest[0][2]) == sorted(s for f in groups.values() for s in f))
    for i, s in enumerate(shards):
        print(f"shard {i + 1}/{n}: {load[i]:5.1f} min, {len(s)} units")


def main(argv):
    if len(argv) != 4 or argv[1] not in ("plan", "build", "run", "check"):
        sys.exit(__doc__)
    mode, crate, spec = argv[1:]
    pkg = metadata(crate)
    if mode == "check":
        check(pkg, int(spec))
        return 0
    k, n = (int(x) for x in spec.split("/"))
    if not 1 <= k <= n:
        sys.exit(f"ci_shard: bad shard {spec}")
    check(pkg, n)
    shards, load = assign(pkg, n)
    shard = shards[k - 1]
    print(f"shard {k}/{n} of {crate}, about {load[k - 1]:.0f} min: " + " ".join(u[0] for u in shard))
    if mode == "plan":
        return 0
    failed = []
    for cmd in commands(crate, shard, mode == "build"):
        print("+ " + " ".join(cmd), flush=True)
        if subprocess.run(cmd).returncode != 0:
            failed.append(" ".join(cmd))
    for f in failed:
        print(f"FAILED: {f}")
    return 1 if failed else 0


if __name__ == "__main__":
    sys.exit(main(sys.argv))
