# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Icon style pack: four more drawing styles derived from the two-tone set, plus contact sheets.

Usage: python tools/icons/styles.py [--check] [--sheets]
Reads docs/design/icons_v4/duotone/<16|20|24>/<name>.svg (tools/icons/gen.py) and writes
assets/icons_pick/<style>/<16|20|24>/<name>.svg for the styles
  hierarchical  one ink at three opacities (stroke full, surface .25, spark .55); mono, tinted at draw time
  color         opaque role-coloured surfaces, 1 px edge ink, warm spark
  monoline      thin single-weight strokes, no surface fill, cool spark
  isometric     the colour glyph on an isometric wall plate with top and side faces
Colour styles use only the light `icon.*` token values of docs/design/tokens.json: the app swaps them
for the dark theme's (crates/ui/src/icons.rs). Deterministic; every file is generated, never edited.
`--sheets` writes docs/design/icons_pick/<style>_<light|dark>.png for the six pickable styles."""
import argparse
import io
import json
import os
import re
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
SRC = os.path.join(ROOT, "docs", "design", "icons_v4")
OUT = os.path.join(ROOT, "assets", "icons_pick")
SHEETS = os.path.join(ROOT, "docs", "design", "icons_pick")
SIZES = (16, 20, 24)
STYLES = ("hierarchical", "color", "monoline", "isometric")
ACCENT = "#2457D6"

TOK = json.load(open(os.path.join(ROOT, "docs", "design", "tokens.json"), encoding="utf-8"))["color"]
LIGHT = {k[5:]: v for k, v in TOK["light"].items() if k.startswith("icon.")}
DARK = {k[5:]: v for k, v in TOK["dark"].items() if k.startswith("icon.")}

SWAP = {v: DARK[k] for k, v in LIGHT.items()}
SURF = f'fill="{ACCENT}" fill-opacity=".2"'
SPARK = f'fill="{ACCENT}"'
ROOT_RE = re.compile(r'^(<svg [^>]*?) fill="none" stroke="currentColor" stroke-width="([0-9.]+)"([^>]*>)')


def split(svg):
    m = ROOT_RE.match(svg)
    assert m, svg[:120]
    head, w, tail = m.group(1), float(m.group(2)), m.group(3)
    inner = svg[m.end():].rsplit("</svg>", 1)[0]
    return head, w, tail, inner


def num(v):
    return ("%.2f" % v).rstrip("0").rstrip(".")


def recolour(inner, surf, spark, ink):
    """Surface and spark fills, then the solid ink fills."""
    inner = inner.replace(SURF, surf)
    inner = inner.replace(SPARK, spark)
    return inner.replace('fill="currentColor"', f'fill="{ink}"')


def hierarchical(svg, size):
    head, w, tail, inner = split(svg)
    inner = inner.replace(SURF, 'fill="currentColor" fill-opacity=".25"')
    inner = inner.replace(SPARK, 'fill="currentColor" fill-opacity=".55"')
    return f'{head} fill="none" stroke="currentColor" stroke-width="{num(w)}"{tail}{inner}</svg>\n'


def color(svg, size):
    head, w, tail, inner = split(svg)
    ink = LIGHT["line"]  # follows the theme: the dark edge token would vanish on a dark panel
    inner = recolour(inner, f'fill="{LIGHT["subject"]}"', f'fill="{LIGHT["action"]}"', ink)
    return f'{head} fill="none" stroke="{ink}" stroke-width="{num(w)}"{tail}{inner}</svg>\n'


def monoline(svg, size):
    head, w, tail, inner = split(svg)
    ink = LIGHT["line"]
    inner = re.sub(r'<path [^>]*' + re.escape(SURF) + r'[^>]*/>', "", inner)
    inner = recolour(inner, "", f'fill="{LIGHT["subject.side"]}"', ink)
    w = w if size == 16 else w * 5 / 6
    return f'{head} fill="none" stroke="{ink}" stroke-width="{num(w)}"{tail}{inner}</svg>\n'


# isometric wall plate, in a 24 unit box: glyph square S on the left wall, depth D to the right
S, DEPTH = 13.5, 3.2
DX, DY = 0.866 * DEPTH, -0.5 * DEPTH
X0 = (24 - (0.866 * S + DX)) / 2
Y0 = (24 - (0.5 * S + S - DY)) / 2 - DY


def isometric(svg, size):
    head, w, tail, inner = split(svg)
    edge = LIGHT["edge"]
    inner = recolour(inner, f'fill="{LIGHT["subject"]}"', f'fill="{LIGHT["action"]}"', edge)
    p00 = (X0, Y0)
    p10 = (X0 + 0.866 * S, Y0 + 0.5 * S)
    p01 = (X0, Y0 + S)
    p11 = (p10[0], p10[1] + S)

    def poly(pts, fill):
        d = " ".join(f"{num(x)},{num(y)}" for x, y in pts)
        return (f'<polygon points="{d}" fill="{fill}" stroke="{edge}" stroke-width=".6" '
                f'stroke-linejoin="round"/>')

    def add(p):
        return (p[0] + DX, p[1] + DY)

    plate = (poly([p10, p11, add(p11), add(p10)], LIGHT["context.side"])
             + poly([p00, p10, add(p10), add(p00)], LIGHT["context.top"])
             + poly([p00, p10, p11, p01], LIGHT["paper"]))
    k = S / size
    gw = 2.0 if size != 16 else 1.5
    m = " ".join(num(v) for v in (0.866 * k, 0.5 * k, 0, k, X0, Y0))
    glyph = (f'<g transform="matrix({m})" fill="none" stroke="{edge}" stroke-width="{num(gw)}" '
             f'stroke-linecap="round" stroke-linejoin="round">{inner}</g>')
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
            f'viewBox="0 0 24 24" fill="none">{plate}{glyph}</svg>\n')


MAKE = {"hierarchical": hierarchical, "color": color, "monoline": monoline, "isometric": isometric}


def names():
    return sorted(f[:-4] for f in os.listdir(os.path.join(SRC, "duotone", "24")) if f.endswith(".svg"))


def read(d, size, n):
    with open(os.path.join(SRC, d, str(size), n + ".svg"), encoding="utf-8") as f:
        return f.read()


def generate():
    ns = names()
    for st in STYLES:
        base = os.path.join(OUT, st)
        if os.path.isdir(base):
            shutil.rmtree(base)
        for size in SIZES:
            os.makedirs(os.path.join(base, str(size)))
            for n in ns:
                with open(os.path.join(base, str(size), n + ".svg"), "w", encoding="utf-8", newline="\n") as f:
                    f.write(MAKE[st](read("duotone", size, n), size))
    print(len(ns), "glyphs x", len(STYLES), "styles x 3 masters ->", OUT)


def check():
    ns = names()
    allowed = set(LIGHT.values()) | {"none", "currentColor", "#fff", "#000", "#FFFFFF", "#000000"}
    bad = []
    for st in STYLES:
        for size in SIZES:
            for n in ns:
                p = os.path.join(OUT, st, str(size), n + ".svg")
                if not os.path.exists(p):
                    bad.append(f"missing {st}/{size}/{n}")
                    continue
                s = open(p, encoding="utf-8").read()
                if ACCENT in s:
                    bad.append(f"file accent left in {st}/{size}/{n}")
                for c in re.findall(r'(?:fill|stroke)="(#[0-9A-Fa-f]{3,6}|currentColor)"', s):
                    if st != "hierarchical" and c not in allowed:
                        bad.append(f"off-palette {c} in {st}/{size}/{n}")
                    if st != "hierarchical" and c == "currentColor":
                        bad.append(f"currentColor in colour style {st}/{size}/{n}")
    print("check:", "ok" if not bad else f"{len(bad)} problems")
    for b in bad[:20]:
        print(" ", b)
    return not bad


def sheets():
    import resvg_py
    from PIL import Image, ImageDraw
    ns = names()
    cell, px, cols = 56, 48, 24
    rows = -(-len(ns) // cols)
    # (sheet name, source dir, kind): kind = mono (ink + accent) or colour
    six = [("a3_duotone", os.path.join(SRC, "duotone"), "mono"), ("a1_outline", os.path.join(SRC, "outline"), "mono"),
           ("b1_color", os.path.join(OUT, "color"), "colour"),
           ("b2_hierarchical", os.path.join(OUT, "hierarchical"), "mono"),
           ("c1_isometric", os.path.join(OUT, "isometric"), "colour"),
           ("c2_monoline", os.path.join(OUT, "monoline"), "colour")]
    os.makedirs(SHEETS, exist_ok=True)
    for dark in (False, True):
        bg = "#1E242C" if dark else "#FFFFFF"
        ink = DARK["line"] if dark else LIGHT["line"]
        acc = "#6E9BFF" if dark else ACCENT
        for sid, d, kind in six:
            im = Image.new("RGBA", (cols * cell, rows * cell), bg)
            for i, n in enumerate(ns):
                s = open(os.path.join(d, "24", n + ".svg"), encoding="utf-8").read()
                if kind == "colour" and dark:
                    s = re.sub("#[0-9A-F]{6}", lambda m: SWAP.get(m.group(0), m.group(0)), s)
                s = s.replace("currentColor", ink).replace(ACCENT, acc)
                png = resvg_py.svg_to_bytes(svg_string=s, width=px, height=px, skip_system_fonts=True)
                g = Image.open(io.BytesIO(bytes(png))).convert("RGBA")
                im.alpha_composite(g, ((i % cols) * cell + 4, (i // cols) * cell + 4))
            ImageDraw.Draw(im)
            im.convert("RGB").save(os.path.join(SHEETS, f"{sid}_{'dark' if dark else 'light'}.png"))
    print("sheets ->", SHEETS)


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--sheets", action="store_true")
    a = ap.parse_args()
    if a.check:
        sys.exit(0 if check() else 1)
    generate()
    if a.sheets:
        sheets()


if __name__ == "__main__":
    main()
