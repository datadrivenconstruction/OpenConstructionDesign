"""Validate IFC files written by DDC Editor, outside the Rust code base.

Layers
  1. Built-in checks, no dependencies: STEP header and syntax, unique ids, resolved references,
     GlobalIds, attribute counts of the classes we emit (per file schema), exactly one IfcProject,
     every building element contained in a spatial element or a part of another, and a shape for
     every product (a body with items; triangle indices in range, no collapsed triangles).
  2. Optional external toolkit (schema rules and real geometry for every product). Set
     DDC_IFC_TOOLKIT to the importable module name of a toolkit that provides `open`, `validate`
     and `geom`; it is only ever imported here, never linked into the product. Without it this
     layer reports "skipped" (an error with --require-external).
  3. Coverage report: per class the products written, how many have a shape, how many shapes are
     valid; with --expect counts.json (class -> count, written by the model side) every difference
     is an error, so an element that silently went missing in a file is named.

usage: python tools/validate_ifc.py FILE.ifc... [--expect counts.json] [--json out.json]
                                   [--strict] [--require-external]
exit: 0 clean, 1 findings, 2 usage or unreadable file.
"""
from __future__ import annotations

import argparse
import importlib
import json
import os
import re
import sys
from collections import Counter, defaultdict

GUID_RE = re.compile(r"^[0-9A-Za-z_$]{22}$")
ENTITY_RE = re.compile(r"^#(\d+)\s*=\s*([A-Za-z0-9_]+)\s*\((.*)\)\s*$", re.S)

# Attribute counts (IFC4X3) of the classes this tool pins down; other classes are not counted.
ARITY = {
    "IFCPROJECT": 9,
    "IFCSITE": 14,
    "IFCBUILDING": 12,
    "IFCBUILDINGSTOREY": 10,
    "IFCWALL": 9,
    "IFCSLAB": 9,
    "IFCMEMBER": 9,
    "IFCSTAIR": 9,
    "IFCSTAIRFLIGHT": 13,
    "IFCRAMP": 9,
    "IFCRAMPFLIGHT": 9,
    "IFCRAILING": 9,
    "IFCRELAGGREGATES": 6,
    "IFCRELCONTAINEDINSPATIALSTRUCTURE": 6,
    "IFCRELASSOCIATESMATERIAL": 6,
    "IFCRELDEFINESBYPROPERTIES": 6,
    "IFCPROPERTYSET": 5,
    "IFCPROPERTYSINGLEVALUE": 4,
    "IFCMATERIAL": 3,
    "IFCLOCALPLACEMENT": 2,
    "IFCSHAPEREPRESENTATION": 4,
    "IFCPRODUCTDEFINITIONSHAPE": 3,
    "IFCTRIANGULATEDFACESET": 5,
    "IFCCARTESIANPOINTLIST3D": 2,
}
# Where the older schemas differ (None: the class does not exist there).
ARITY_BY_SCHEMA = {
    "IFC4": {"IFCCARTESIANPOINTLIST3D": 1},
    "IFC2X3": {
        "IFCWALL": 8,
        "IFCSTAIRFLIGHT": 12,
        "IFCRAMPFLIGHT": 8,
        "IFCMATERIAL": 1,
        "IFCTRIANGULATEDFACESET": None,
        "IFCCARTESIANPOINTLIST3D": None,
    },
}


def arity_for(schema: str) -> dict:
    s = schema.upper()
    key = "IFC2X3" if "IFC2X3" in s else "IFC4X3" if "IFC4X3" in s else "IFC4"
    return {**ARITY, **ARITY_BY_SCHEMA.get(key, {})}


# Building elements that must be contained in a spatial element or be a part (aggregated child).
ELEMENTS = {
    "IFCWALL", "IFCWALLSTANDARDCASE", "IFCSLAB", "IFCSTAIR", "IFCSTAIRFLIGHT", "IFCRAMP",
    "IFCRAMPFLIGHT", "IFCRAILING", "IFCCOLUMN", "IFCBEAM", "IFCROOF", "IFCCOVERING",
    "IFCCURTAINWALL", "IFCBUILDINGELEMENTPROXY", "IFCMEMBER", "IFCPLATE",
}
SPATIAL = {"IFCPROJECT", "IFCSITE", "IFCBUILDING", "IFCBUILDINGSTOREY"}
ROOTED = {"IFCPROJECT", "IFCPROPERTYSET", "IFCZONE", "IFCGROUP"}
# What the exporter may write for an element; anything else is listed in the coverage report.
NATIVE = {
    "IFCWALL", "IFCSLAB", "IFCROOF", "IFCCOLUMN", "IFCBEAM", "IFCMEMBER", "IFCDOOR",
    "IFCWINDOW", "IFCSTAIR", "IFCSTAIRFLIGHT", "IFCRAMP", "IFCRAMPFLIGHT", "IFCRAILING",
    "IFCCOVERING", "IFCCURTAINWALL", "IFCSPACE", "IFCBUILDINGELEMENTPROXY",
}


class Step:
    """Parsed STEP file: header facts and entities {id: (class, args)}."""

    def __init__(self, text: str):
        self.problems: list[str] = []
        self.entities: dict[int, tuple[str, list]] = {}
        self.order: list[int] = []
        head, _, data = text.partition("DATA;")
        if not head.lstrip().startswith("ISO-10303-21"):
            self.problems.append("S001 missing ISO-10303-21 header")
        self.schema = ""
        m = re.search(r"FILE_SCHEMA\s*\(\s*\(\s*'([^']*)'", head)
        if m:
            self.schema = m.group(1)
        else:
            self.problems.append("S002 missing FILE_SCHEMA")
        if self.schema and "IFC" not in self.schema.upper():
            self.problems.append(f"S006 FILE_SCHEMA {self.schema!r} is not an IFC schema")
        for need in ("FILE_DESCRIPTION", "FILE_NAME"):
            if not re.search(rf"\b{need}\s*\(", head):
                self.problems.append(f"S007 header lacks {need}")
        if "END-ISO-10303-21" not in text or "ENDSEC" not in data:
            self.problems.append("S003 file is cut off (no ENDSEC / END-ISO-10303-21)")
        for stmt in statements(data):
            m = ENTITY_RE.match(stmt)
            if not m:
                if stmt.strip() and not stmt.strip().startswith(("ENDSEC", "END-ISO")):
                    self.problems.append(f"S004 not an entity: {stmt[:60]!r}")
                continue
            n = int(m.group(1))
            if n in self.entities:
                self.problems.append(f"E001 duplicate id #{n}")
                continue
            try:
                args = parse_args(m.group(3))
            except ValueError as e:
                self.problems.append(f"S005 #{n} {m.group(2)}: {e}")
                continue
            self.entities[n] = (m.group(2).upper(), args)
            self.order.append(n)

    def cls(self, n: int) -> str | None:
        e = self.entities.get(n)
        return e[0] if e else None


def statements(data: str):
    """Statements split at ';' outside strings; comments dropped."""
    buf: list[str] = []
    q = False
    i = 0
    while i < len(data):
        c = data[i]
        if q:
            buf.append(c)
            if c == "'":
                if data[i + 1 : i + 2] == "'":
                    buf.append("'")
                    i += 1
                else:
                    q = False
        elif c == "'":
            q = True
            buf.append(c)
        elif c == "/" and data[i + 1 : i + 2] == "*":
            j = data.find("*/", i + 2)
            i = len(data) if j < 0 else j + 1
        elif c == ";":
            yield "".join(buf).strip()
            buf = []
        else:
            buf.append(c)
        i += 1
    tail = "".join(buf).strip()
    if tail:
        yield tail


TOKEN = re.compile(
    r"\s*(?:(?P<str>'(?:[^']|'')*')|(?P<ref>#\d+)|(?P<enum>\.[A-Za-z0-9_]+\.)"
    r"|(?P<num>[-+]?\d+\.?\d*(?:[eE][-+]?\d+)?)|(?P<name>[A-Za-z_][A-Za-z0-9_]*)"
    r"|(?P<p>[(),$*]))"
)


def parse_args(s: str) -> list:
    pos = 0

    def tok():
        nonlocal pos
        m = TOKEN.match(s, pos)
        if not m or m.end() == pos:
            raise ValueError(f"unreadable text near {s[pos:pos + 20]!r}")
        pos = m.end()
        return m.lastgroup, m.group(m.lastgroup)

    def value(k, v):
        nonlocal pos
        if k == "str":
            return v[1:-1].replace("''", "'")
        if k == "ref":
            return ("ref", int(v[1:]))
        if k == "enum":
            return ("enum", v.strip(".").upper())
        if k == "num":
            return float(v) if any(c in v for c in ".eE") else int(v)
        if k == "p" and v == "$":
            return None
        if k == "p" and v == "*":
            return "*"
        if k == "p" and v == "(":
            return seq()
        if k == "name":
            m = TOKEN.match(s, pos)
            if m and m.lastgroup == "p" and m.group("p") == "(":
                pos = m.end()
                return ("typed", v.upper(), seq())
            raise ValueError(f"bare name {v}")
        raise ValueError(f"unexpected {v!r}")

    def seq():
        out = []
        k, v = tok()
        if (k, v) == ("p", ")"):
            return out
        while True:
            out.append(value(k, v))
            k, v = tok()
            if (k, v) == ("p", ")"):
                return out
            if (k, v) != ("p", ","):
                raise ValueError("expected , or )")
            k, v = tok()

    out: list = []
    k, v = tok()
    while True:
        out.append(value(k, v))
        if not s[pos:].strip():
            return out
        k, v = tok()
        if (k, v) != ("p", ","):
            raise ValueError("expected ,")
        k, v = tok()


def refs_in(v):
    if isinstance(v, tuple) and v and v[0] == "ref":
        yield v[1]
    elif isinstance(v, tuple) and v and v[0] == "typed":
        yield from refs_in(v[2])
    elif isinstance(v, list):
        for x in v:
            yield from refs_in(x)


def is_ref(v) -> bool:
    return isinstance(v, tuple) and len(v) == 2 and v[0] == "ref"


def shape_problem(st: Step, rep_id: int) -> str | None:
    """None when the product shape has a usable body, else why not."""
    e = st.entities.get(rep_id)
    if not e or e[0] != "IFCPRODUCTDEFINITIONSHAPE":
        return "representation is not an IFCPRODUCTDEFINITIONSHAPE"
    reps = [r for r in e[1][2] if is_ref(r)] if len(e[1]) > 2 and isinstance(e[1][2], list) else []
    items = 0
    for r in reps:
        re_ = st.entities.get(r[1])
        if not re_ or len(re_[1]) < 4 or not isinstance(re_[1][3], list):
            return "shape representation has no item list"
        for it in re_[1][3]:
            if not is_ref(it) or it[1] not in st.entities:
                return "shape item does not resolve"
            items += 1
            bad = item_problem(st, it[1])
            if bad:
                return bad
    return None if items else "shape has no items"


def item_problem(st: Step, item: int) -> str | None:
    cls, args = st.entities[item]
    if cls != "IFCTRIANGULATEDFACESET":
        return None
    pts = st.entities.get(args[0][1]) if args and is_ref(args[0]) else None
    if not pts or not pts[1] or not isinstance(pts[1][0], list) or not pts[1][0]:
        return "face set without points"
    n = len(pts[1][0])
    tris = args[3] if len(args) > 3 else None
    if not isinstance(tris, list) or not tris:
        return "face set without triangles"
    for t in tris:
        if not (isinstance(t, list) and len(t) == 3 and all(isinstance(i, int) for i in t)):
            return "malformed triangle"
        if any(i < 1 or i > n for i in t):
            return "triangle index out of range"
        if len(set(t)) < 3:
            return "collapsed triangle"
    return None


def check(st: Step) -> dict:
    """Findings and the coverage table of one parsed file."""
    errors = list(st.problems)
    warnings: list[str] = []
    arity = arity_for(st.schema)
    for n in st.order:
        cls, args = st.entities[n]
        for r in refs_in(args):
            if r not in st.entities:
                errors.append(f"E002 #{n} {cls}: reference #{r} does not exist")
        want = arity.get(cls, 0)
        if want is None:
            errors.append(f"E011 #{n} {cls}: class is not in the file's schema {st.schema}")
        elif want and len(args) != want:
            errors.append(f"E005 #{n} {cls}: {len(args)} attributes, expected {want}")
        if cls == "IFCSTAIRFLIGHT" and len(args) == 13:
            # NumberOfRisers / NumberOfTreads are INTEGER, heights IfcPositiveLengthMeasure (REAL)
            for i, name in ((8, "NumberOfRisers"), (9, "NumberOfTreads")):
                if args[i] is not None and not (isinstance(args[i], int) and not isinstance(args[i], bool)):
                    errors.append(f"E012 #{n} {cls}: {name} must be an integer")
            for i, name in ((10, "RiserHeight"), (11, "TreadLength")):
                if args[i] is not None and not isinstance(args[i], float):
                    errors.append(f"E012 #{n} {cls}: {name} must be a real number")
    projects = [n for n in st.order if st.entities[n][0] == "IFCPROJECT"]
    if len(projects) != 1:
        errors.append(f"E008 exactly one IFCPROJECT required, found {len(projects)}")
    covered: set[int] = set()
    for n in st.order:
        cls, args = st.entities[n]
        if cls == "IFCRELCONTAINEDINSPATIALSTRUCTURE" and len(args) == 6:
            covered.update(refs_in(args[4]))
            host = st.cls(args[5][1]) if is_ref(args[5]) else None
            if host not in SPATIAL or host == "IFCPROJECT":
                errors.append(f"E010 #{n} {cls}: container is not a storey, building or site")
        elif cls == "IFCRELAGGREGATES" and len(args) == 6:
            covered.update(refs_in(args[5]))
    for n in st.order:
        cls = st.entities[n][0]
        if cls in ELEMENTS and n not in covered:
            errors.append(f"E009 #{n} {cls}: neither contained in a spatial element nor a part")
    def product(n: int) -> bool:
        """A rooted element with a placement: wall, stair, storey, space and so on."""
        cls, args = st.entities[n]
        if len(args) < 7 or not isinstance(args[0], str):
            return False
        place = args[5]
        return is_ref(place) and (st.cls(place[1]) or "").endswith("PLACEMENT")

    def has_rep(n: int) -> bool:
        args = st.entities[n][1]
        return len(args) > 6 and args[6] is not None

    guids: dict[str, int] = {}
    parts = defaultdict(list)
    for n in st.order:
        cls, args = st.entities[n]
        if cls == "IFCRELAGGREGATES" and len(args) >= 6 and is_ref(args[4]) and isinstance(args[5], list):
            parts[args[4][1]].extend(r[1] for r in args[5] if is_ref(r) and r[1] in st.entities)
        rooted = cls.startswith("IFCREL") or cls in ROOTED or product(n)
        if rooted and args and isinstance(args[0], str):
            if not GUID_RE.match(args[0]):
                errors.append(f"E003 #{n} {cls}: bad GlobalId {args[0]!r}")
            elif args[0] in guids:
                errors.append(f"E004 #{n} {cls}: GlobalId also on #{guids[args[0]]}")
            else:
                guids[args[0]] = n
    cov: dict[str, Counter] = defaultdict(Counter)
    for n in st.order:
        if not product(n):
            continue
        cls, args = st.entities[n]
        c = cov[cls]
        c["products"] += 1
        rep = args[6]
        if rep is None:
            if cls in SPATIAL or any(has_rep(p) for p in parts.get(n, [])):
                c["by_parts"] += 1
            else:
                c["no_shape"] += 1
                warnings.append(f"W001 #{n} {cls}: no shape")
            continue
        if not is_ref(rep):
            errors.append(f"E007 #{n} {cls}: representation is not a reference")
            continue
        c["with_shape"] += 1
        why = shape_problem(st, rep[1])
        if why:
            c["bad_shape"] += 1
            errors.append(f"E006 #{n} {cls}: {why}")
        else:
            c["shape_ok"] += 1
    for cls in sorted(cov):
        if cls not in NATIVE and cls not in SPATIAL:
            warnings.append(f"W002 {cls}: {cov[cls]['products']} product(s) of a class the exporter does not write natively")
    return {
        "schema": st.schema,
        "entities": len(st.entities),
        "errors": errors,
        "warnings": warnings,
        "coverage": {k: dict(v) for k, v in sorted(cov.items())},
    }


def compare_expected(report: dict, expected: dict) -> list[str]:
    """Every class the model counted whose count in the file differs."""
    out = []
    got = {k: v["products"] for k, v in report["coverage"].items()}
    for cls in sorted(expected):
        a, b = got.get(cls, 0), expected[cls]
        if a != b:
            out.append(f"C001 {cls}: file has {a}, model has {b}")
    return out


def external(path: str, required: bool) -> tuple[str, list[str]]:
    """('ok'|'skipped'|'failed', findings) from the optional external toolkit."""
    name = os.environ.get("DDC_IFC_TOOLKIT", "").strip()
    if not name:
        return ("failed", ["X000 external toolkit required but DDC_IFC_TOOLKIT is not set"]) if required else ("skipped", [])
    try:
        tk = importlib.import_module(name)
        val = importlib.import_module(name + ".validate")
        geom = importlib.import_module(name + ".geom")
    except ImportError as e:
        return ("failed", [f"X000 external toolkit not importable: {e}"]) if required else ("skipped", [])
    out: list[str] = []
    try:
        f = tk.open(path)
        log = val.json_logger()
        val.validate(f, log)
        out += [f"X001 schema: {s.get('message', s)}" for s in getattr(log, "statements", [])]
        settings = geom.settings()
        for p in f.by_type("IfcProduct"):
            if getattr(p, "Representation", None) is None:
                continue
            try:
                geom.create_shape(settings, p)
            except Exception as e:  # the toolkit raises its own types
                out.append(f"X002 #{p.id()} {p.is_a()}: no shape ({str(e)[:80]})")
    except Exception as e:
        out.append(f"X003 toolkit failed: {str(e)[:120]}")
    return ("failed" if out else "ok", out)


def table(report: dict) -> str:
    rows = [("class", "products", "shape", "valid", "by parts", "none")]
    for cls, c in report["coverage"].items():
        rows.append((cls, c.get("products", 0), c.get("with_shape", 0), c.get("shape_ok", 0), c.get("by_parts", 0), c.get("no_shape", 0)))
    w = [max(len(str(r[i])) for r in rows) for i in range(6)]
    return "\n".join("  ".join(str(v).ljust(w[i]) for i, v in enumerate(r)) for r in rows)


def run(path: str, expected: dict | None, strict: bool, need_ext: bool) -> dict:
    with open(path, encoding="utf-8", errors="replace") as f:
        st = Step(f.read())
    rep = check(st)
    if expected is not None:
        rep["errors"] += compare_expected(rep, expected)
    status, found = external(path, need_ext)
    rep["external"] = status
    rep["errors"] += found
    if strict:
        rep["errors"] += rep["warnings"]
        rep["warnings"] = []
    rep["file"] = path
    return rep


def main(argv=None) -> int:
    ap = argparse.ArgumentParser(description=__doc__.split("\n")[0])
    ap.add_argument("files", nargs="+")
    ap.add_argument("--expect", help="JSON {class: count} from the model; differences are errors")
    ap.add_argument("--json", help="write the full report here")
    ap.add_argument("--strict", action="store_true", help="warnings count as errors")
    ap.add_argument("--require-external", action="store_true")
    a = ap.parse_args(argv)
    expected = None
    if a.expect:
        try:
            with open(a.expect, encoding="utf-8") as f:
                expected = json.load(f)
        except (OSError, ValueError) as e:
            print(f"cannot read {a.expect}: {e}", file=sys.stderr)
            return 2
    reports, bad = [], False
    for p in a.files:
        try:
            r = run(p, expected, a.strict, a.require_external)
        except OSError as e:
            print(f"cannot read {p}: {e}", file=sys.stderr)
            return 2
        reports.append(r)
        print(f"{p}: {r['schema'] or '?'}, {r['entities']} entities, external {r['external']}")
        print(table(r))
        for line in r["errors"][:50]:
            print("  ERROR", line)
        for line in r["warnings"][:50]:
            print("  warn ", line)
        bad = bad or bool(r["errors"])
    if a.json:
        with open(a.json, "w", encoding="utf-8") as f:
            json.dump(reports, f, indent=1)
    print("IFC VALIDATION FAIL" if bad else "IFC VALIDATION OK")
    return 1 if bad else 0


if __name__ == "__main__":
    sys.exit(main())
