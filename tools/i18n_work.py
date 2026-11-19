#!/usr/bin/env python3
"""Per-locale translation bookkeeping for crates/ui/data/i18n.

English is the source: data/i18n/ui.en.json, commands.en.json and en/*.json.
A locale <loc> (any of the 45 in Lang::ALL) keeps its strings in data/i18n/<loc>/<name>.json,
where <name> is the English file's name (ui.en.json -> ui.json, commands.en.json -> commands.json).
Missing keys fall back to the base language and then English at run time, so partial files are fine.

  status [--locale xx]            missing / stale / extra counts per locale
  work   --locale xx [--chunk N]  write small work files (target/i18n_work/<loc>/NNN.json)
  apply  --locale xx FILE...      validate translated work files and merge them into the locale
  check                           parse, keys subset of en, {placeholders} kept (exit 1 on error)

Stale = the English text changed since the key was last applied (hashes in <loc>/.en-hashes).
"""
import argparse, hashlib, json, re, sys
from pathlib import Path

ROOT = Path(__file__).resolve().parent.parent
DATA = ROOT / "crates/ui/data/i18n"
SRC_RE = re.compile(r"\{(\w+)\}")
# Locales that need more plural categories than en's one/other (CLDR).
PLURALS = {
    "ru": "one few many", "uk": "one few many", "pl": "one few many", "hr": "one few other",
    "cs": "one few other", "ro": "one few other", "ar": "zero one two few many other",
    "he": "one two other", "fr": "one other", "pt": "one other", "pt-BR": "one other",
    "ja": "other", "ko": "other", "zh": "other", "vi": "other", "th": "other", "id": "other",
}


# A `langs!` row: `Variant "code" rtl,` (endonyms live in data/languages.json).
LANG_ROW = re.compile(r'^    \w+ "([\w-]+)" (?:true|false),$', re.M)


def locale_codes(src):
    return LANG_ROW.findall(src)


def locales():
    """Locale codes from the Rust registry (single source of truth)."""
    src = (ROOT / "crates/ui/src/i18n.rs").read_text(encoding="utf-8")
    codes = locale_codes(src)
    if len(codes) != 45:
        sys.exit(f"registry parse: {len(codes)} locales, expected 45")
    return codes


def sources():
    """[(logical name, english path)] in load order."""
    out = [("ui", DATA / "ui.en.json"), ("commands", DATA / "commands.en.json")]
    out += [(p.stem, p) for p in sorted((DATA / "en").glob("*.json"))]
    return out


def target(loc, name):
    if loc == "ru":  # Russian predates this layout: base files sit at the root
        return DATA / (f"{name}.ru.json" if name in ("ui", "commands") else f"ru/{name}.json")
    return DATA / loc / f"{name}.json"


def read(p):
    if not p.exists():
        return {}
    try:
        return json.loads(p.read_text(encoding="utf-8"))
    except json.JSONDecodeError as e:
        sys.exit(f"{p}: invalid JSON: {e}")


def write(p, d):
    p.parent.mkdir(parents=True, exist_ok=True)
    p.write_text(json.dumps(d, ensure_ascii=False, indent=1) + "\n", encoding="utf-8", newline="\n")


def slots(v):
    if isinstance(v, str):
        return set(SRC_RE.findall(v))
    if isinstance(v, dict):
        return set().union(*[slots(x) for x in v.values()]) if v else set()
    return set()


def translatable(v):
    """False for values with no words (only slots, digits, punctuation, symbols)."""
    s = json.dumps(v, ensure_ascii=False) if not isinstance(v, str) else v
    return bool(re.search(r"[^\W\d_]", SRC_RE.sub("", s)))


def h(v):
    return hashlib.sha1(json.dumps(v, ensure_ascii=False, sort_keys=True).encode()).hexdigest()[:8]


def load_en():
    en, where = {}, {}
    for name, p in sources():
        for k, v in read(p).items():
            en[k] = v
            where[k] = name
    return en, where


def hashes_path(loc):
    return DATA / loc / ".en-hashes"


def load_hashes(loc):
    p = hashes_path(loc)
    return json.loads(p.read_text(encoding="utf-8")) if p.exists() else {}


def locale_keys(loc):
    d = {}
    for name, _ in sources():
        d.update(read(target(loc, name)))
    return d


def diff(loc, en):
    have, hs = locale_keys(loc), load_hashes(loc)
    todo = [k for k, v in en.items() if k not in have and translatable(v)]
    stale = [k for k in en if k in have and k in hs and hs[k] != h(en[k])]
    extra = [k for k in have if k not in en]
    return todo, stale, extra


def cmd_status(a):
    en, _ = load_en()
    print(f"{'locale':8} {'done':>6} {'missing':>8} {'stale':>6} {'extra':>6}  of {len(en)} keys")
    for loc in [a.locale] if a.locale else [c for c in locales() if c != "en"]:
        todo, stale, extra = diff(loc, en)
        print(f"{loc:8} {len(en) - len(todo):6} {len(todo):8} {len(stale):6} {len(extra):6}")


def cmd_work(a):
    en, where = load_en()
    todo, stale, _ = diff(a.locale, en)
    keys = sorted(set(todo) | set(stale), key=lambda k: (where[k], k))
    out = Path(a.out) / a.locale
    out.mkdir(parents=True, exist_ok=True)
    for old in out.glob("*.json"):
        old.unlink()
    chunks, cur, cur_file = [], {}, None
    for k in keys:
        if cur and (where[k] != cur_file or len(cur) >= a.chunk):
            chunks.append((cur_file, cur))
            cur = {}
        cur_file = where[k]
        cur[k] = en[k]
    if cur:
        chunks.append((cur_file, cur))
    cats = PLURALS.get(a.locale, "one other")
    for i, (f, items) in enumerate(chunks, 1):
        write(out / f"{i:03}.json", {
            "locale": a.locale,
            "file": f,
            "rules": [
                "Translate the values in `items` from English; keep keys unchanged.",
                "Keep every {placeholder} exactly as is; never translate or drop one.",
                "Plural objects: give exactly these CLDR categories (plus any `=N` keys already present): " + cats + ". `#` stands for the number.",
                "Keep the UI terse; keep product and file-format names (IFC, DWG, RVT, XLSX) as they are.",
                "Return the same JSON with only `items` changed.",
            ],
            "items": items,
        })
    print(f"{a.locale}: {len(keys)} keys ({len(todo)} missing, {len(stale)} stale) -> {len(chunks)} files in {out}")


def cmd_apply(a):
    en, where = load_en()
    have = {name: read(target(a.locale, name)) for name, _ in sources()}
    hs = load_hashes(a.locale)
    ok = bad = 0
    for f in a.files:
        d = read(Path(f))
        if d.get("locale") != a.locale:
            sys.exit(f"{f}: locale {d.get('locale')!r} != {a.locale!r}")
        for k, v in d.get("items", {}).items():
            if k not in en:
                print(f"REJECT {k}: not an English key"); bad += 1; continue
            if slots(v) != slots(en[k]):
                print(f"REJECT {k}: placeholders {sorted(slots(v))} != {sorted(slots(en[k]))}"); bad += 1; continue
            if isinstance(v, dict) != isinstance(en[k], dict) or (not isinstance(v, (str, dict))):
                print(f"REJECT {k}: shape differs from English"); bad += 1; continue
            have[where[k]][k] = v
            hs[k] = h(en[k])
            ok += 1
    for name, _ in sources():
        order = {k: i for i, k in enumerate(read(dict(sources())[name]))}
        d = dict(sorted(have[name].items(), key=lambda kv: order.get(kv[0], 1 << 30)))
        if d:
            write(target(a.locale, name), d)
    if ok:
        hashes_path(a.locale).write_text(
            json.dumps(dict(sorted(hs.items())), ensure_ascii=False, indent=0) + "\n", encoding="utf-8", newline="\n")
    print(f"{a.locale}: applied {ok}, rejected {bad}")
    sys.exit(1 if bad else 0)


def cmd_check(a):
    en, _ = load_en()
    errs = 0
    known = set(locales())
    for d in sorted(p for p in DATA.iterdir() if p.is_dir()):
        if d.name not in known:
            print(f"{d.name}: not a registered locale"); errs += 1
    for loc in known - {"en"}:
        for k, v in locale_keys(loc).items():
            if k not in en:
                print(f"{loc}: {k}: not in en"); errs += 1
            elif loc != "ru" and slots(v) != slots(en[k]):  # ru: 14 legacy named-slot texts
                print(f"{loc}: {k}: placeholders differ"); errs += 1
    print("ok" if not errs else f"{errs} problems")
    sys.exit(1 if errs else 0)


def main():
    ap = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    sub = ap.add_subparsers(dest="cmd", required=True)
    s = sub.add_parser("status"); s.add_argument("--locale"); s.set_defaults(f=cmd_status)
    w = sub.add_parser("work"); w.add_argument("--locale", required=True)
    w.add_argument("--chunk", type=int, default=150); w.add_argument("--out", default=str(ROOT / "target/i18n_work")); w.set_defaults(f=cmd_work)
    p = sub.add_parser("apply"); p.add_argument("--locale", required=True); p.add_argument("files", nargs="+"); p.set_defaults(f=cmd_apply)
    c = sub.add_parser("check"); c.set_defaults(f=cmd_check)
    a = ap.parse_args()
    if getattr(a, "locale", None) and a.locale not in locales():
        sys.exit(f"unknown locale {a.locale}")
    a.f(a)


if __name__ == "__main__":
    main()
