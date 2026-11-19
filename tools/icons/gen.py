"""DDC icon generator: every glyph module -> docs/design/icons_v4/<direction>/<px>/<name>.svg.

Usage: python tools/icons/gen.py [--out DIR]
The generated SVGs are never edited by hand: change the drawing here and rerun."""
import argparse
import importlib
import os
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)

import core  # noqa: E402

MODULES = ["glyphs_base", "glyphs_arch", "glyphs_edit", "glyphs_doc", "glyphs_form"]


def load():
    """Imports the glyph modules (ICON_MODULES=a,b limits them while a module is half drawn)."""
    want = [m for m in os.environ.get("ICON_MODULES", "").split(",") if m] or MODULES
    for m in want:
        if os.path.exists(os.path.join(HERE, m + ".py")):
            importlib.import_module(m)
    return core.ICONS


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--out", default=os.path.join(HERE, "..", "..", "docs", "design", "icons_v4"))
    a = ap.parse_args()
    icons = load()
    out = os.path.abspath(a.out)
    for mode in core.MODES:
        for size in core.SIZES:
            d = os.path.join(out, mode, str(size))
            if os.path.isdir(d):
                shutil.rmtree(d)
            os.makedirs(d)
            for name, _ru, fn in icons:
                with open(os.path.join(d, name + ".svg"), "w", encoding="utf-8", newline="\n") as f:
                    f.write(core.svg(name, fn, size, mode))
    print(len(icons), "icons ->", out)


if __name__ == "__main__":
    main()
