"""Checks the generated set the way crates/ui tests do, and draws a contact sheet.

Usage: python tools/icons/check.py [--sheet out.png] [--only a,b] [--zoom 4]
Exit 1 on any failure. Bars match crates/ui/src/icons.rs and icon_top.rs."""
import argparse
import json
import pathlib
import io
import os
import re
import sys

import numpy as np
import resvg_py
from PIL import Image, ImageDraw, ImageFont

HERE = os.path.dirname(os.path.abspath(__file__))
sys.path.insert(0, HERE)
import core  # noqa: E402
import gen  # noqa: E402

ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
TOP60_RS = os.path.join(ROOT, "crates", "ui", "src", "icon_top.rs")


# One AI glyph: ai-chat is the same drawing as spark on purpose.
TWINS = {("spark", "ai-chat")}


def master(px):
    return 16 if px < 20 else 20 if px < 24 else 24


def raster(svg_text, px):
    s = svg_text.replace("currentColor", "#FFFFFF")
    png = resvg_py.svg_to_bytes(svg_string=s, width=px, height=px, skip_system_fonts=True)
    im = Image.open(io.BytesIO(bytes(png))).convert("RGBA")
    return np.asarray(im)[:, :, 3].astype(np.float32) / 255.0


def share_differing(a, b):
    return float(np.abs(a - b).sum() / max(np.maximum(a, b).sum(), 1e-9))


def top60():
    src = open(TOP60_RS, encoding="utf-8").read()
    body = src.split("pub const TOP60")[1].split("];")[0]
    return re.findall(r'"([^"]+)"', body)


SEG = re.compile(r"([MLHVAQZh])([^MLHVAQZh]*)")


def axis_segments(d):
    """Axis-aligned segments (kind, fixed coord, a, b) of a path made of M L H V A Q Z."""
    out, x, y, sx, sy = [], 0.0, 0.0, 0.0, 0.0
    for c, args in SEG.findall(d):
        v = [float(t) for t in args.replace(",", " ").split()]
        if c == "M":
            x, y = v[0], v[1]
            sx, sy = x, y
        elif c == "L":
            nx, ny = v[0], v[1]
            if abs(ny - y) < 1e-6 and abs(nx - x) > 1e-6:
                out.append(("H", y, min(x, nx), max(x, nx)))
            elif abs(nx - x) < 1e-6 and abs(ny - y) > 1e-6:
                out.append(("V", x, min(y, ny), max(y, ny)))
            x, y = nx, ny
        elif c == "H":
            out.append(("H", y, min(x, v[0]), max(x, v[0])))
            x = v[0]
        elif c == "V":
            out.append(("V", x, min(y, v[0]), max(y, v[0])))
            y = v[0]
        elif c == "A":
            x, y = v[5], v[6]
        elif c == "Q":
            x, y = v[2], v[3]
        elif c == "h":
            x += v[0]
        elif c == "Z":
            if abs(sy - y) < 1e-6 and abs(sx - x) > 1e-6:
                out.append(("H", y, min(x, sx), max(x, sx)))
            elif abs(sx - x) < 1e-6 and abs(sy - y) > 1e-6:
                out.append(("V", x, min(y, sy), max(y, sy)))
            x, y = sx, sy
    return out


def stroke_paths(svg_text):
    """`d` of the stroked paths outside masks (the ones the outline draws)."""
    t = re.sub(r"<mask.*?</mask>", "", svg_text)
    out = []
    for m in re.finditer(r"<path ([^>]*)/>", t):
        a = m.group(1)
        if 'stroke="none"' in a or "fill=" in a:
            continue
        out.append(re.search(r'd="([^"]*)"', a).group(1))
    return out


def halo_pixels(svg20):
    """Pixels the badge halo knocks out of the base (the black paths of the mask), so the edge
    check samples only strokes the design keeps."""
    m = re.search(r"<mask[^>]*>(.*?)</mask>", svg20, re.S)
    if not m:
        return np.zeros((20, 20), bool)
    cut = re.sub(r"<rect[^>]*/>", "", m.group(1)).replace("#000", "#fff")
    s = ('<svg xmlns="http://www.w3.org/2000/svg" width="20" height="20" viewBox="0 0 20 20" '
         f'stroke-linecap="round" stroke-linejoin="round">{cut}</svg>')
    return raster(s, 20) > 0.02


def edge_problems(name, svg20, img):
    """Every axis-aligned stroke of the 20 master: centre on .25/.75, the outer edge pixel full
    along the run and clean outside at least once (samples keep 1.5 px off the joins)."""
    bad = []
    halo = halo_pixels(svg20)
    for d in stroke_paths(svg20):
        for kind, c, a, b in axis_segments(d):
            if b - a < 3:
                continue
            f = round((c % 1) * 100)
            if f not in (25, 75, 37, 38, 62, 63):  # 20 master: w/2 and 1 - w/2
                bad.append(f"{name}: {kind} at {c} off grid")
                continue
            if f in (75, 62, 63):
                edge = c - core.W[20] / 2
                inside, outside = int(edge), int(edge) - 1
            else:
                edge = c + core.W[20] / 2
                inside, outside = int(edge) - 1, int(edge)
            run = range(int(np.ceil(a + 1.5)), int(np.floor(b - 1.5)))  # clear of corner joins
            if not run:
                continue
            full, clean, seen = True, False, 0
            for t in run:
                if not (0 <= t < 20 and 0 <= inside < 20):
                    continue
                cells = [(inside, t), (outside, t)] if kind == "H" else [(t, inside), (t, outside)]
                if any(0 <= r < 20 and 0 <= q < 20 and halo[r, q] for r, q in cells):
                    continue
                seen += 1
                vin = img[inside, t] if kind == "H" else img[t, inside]
                if vin < 0.9:
                    full = False
                if not (0 <= outside < 20):
                    clean = True
                    continue
                vout = img[outside, t] if kind == "H" else img[t, outside]
                if vout <= 0.1:
                    clean = True
            if seen and (not full or not clean):
                bad.append(f"{name}: {kind} {c} [{a},{b}] full={full} clean={clean}")
    return bad


def run_checks(icons, svgs, only=None):
    bad = []
    names = [n for n, _, _ in icons]
    m = {}
    for mode in ("outline", "filled", "duotone"):
        for px in (16, 20, 30, 40):
            for n in names:
                m[(mode, px, n)] = raster(svgs[(mode, master(px), n)], px)
    # ink share and margin
    mine = [n for n in names if not only or n in only]
    for n in mine:
        for mode, mx in (("outline", 0.7), ("filled", 0.8), ("duotone", 0.8)):
            for px in (16, 20, 30, 40):
                a = m[(mode, px, n)]
                frac = (a > 0).sum() / (px * px)
                if not (0.03 < frac < mx):
                    bad.append(f"ink {n} {mode}@{px}: {frac:.2f}")
            a = m[(mode, 20, n)]
            edge = np.concatenate([a[0], a[-1], a[:, 0], a[:, -1]]).max()
            if edge >= 0.1:
                bad.append(f"margin {n} {mode}: {edge:.2f}")
    # pairwise distinctness
    for mode, px, bar in (("outline", 20, 0.08), ("filled", 20, 0.08), ("outline", 16, 0.04)):
        ms = [(n, m[(mode, px, n)]) for n in names]
        for i, (a, ma) in enumerate(ms):
            for b, mb in ms[i + 1:]:
                if only and a not in only and b not in only:
                    continue
                if (a, b) in TWINS:
                    continue
                d = share_differing(ma, mb)
                if d < bar:
                    bad.append(f"alike {mode}@{px}: {a} ~ {b}: {d:.3f}")
    roofs = [n for n in names if n.startswith("roof") and (not only or n in only)]
    for px in (16, 20):
        for i, a in enumerate(roofs):
            for b in roofs[i + 1:]:
                d = share_differing(m[("outline", px, a)], m[("outline", px, b)])
                if d < 0.15:
                    bad.append(f"roof alike @{px}: {a} ~ {b}: {d:.3f}")
    t60 = top60()
    for n in t60:
        if only and n not in only:
            continue
        if n not in names:
            bad.append(f"top60 {n}: not drawn")
            continue
        a = m[("outline", 16, n)]
        solid, touched = int((a >= 0.8).sum()), int((a > 0.2).sum())
        if solid < 12 or touched > 156:
            bad.append(f"top60 weak {n}: solid {solid}, touched {touched}")
        for o in names:
            if o == n or (o in t60 and t60.index(o) < t60.index(n)):
                continue
            d = share_differing(a, m[("outline", 16, o)])
            if d < 0.10:
                bad.append(f"top60 alike {n} ~ {o}: {d:.3f}")
    # filled reads differently wherever the glyph has a surface
    for n, _, fn in icons:
        if only and n not in only:
            continue
        d = share_differing(m[("outline", 20, n)], m[("filled", 20, n)])
        if core.has_surface(fn):
            if d < 0.02:
                bad.append(f"fill same as outline: {n} {d:.3f}")
        elif svgs[("outline", 20, n)] != svgs[("filled", 20, n)]:
            bad.append(f"line-only glyph {n}: filled must equal outline")
    for n in mine:
        bad += edge_problems(n, svgs[("outline", 20, n)], m[("outline", 20, n)])
    return bad, m


def sheet(icons, svgs, path, zoom):
    font = ImageFont.load_default()
    cols = 8
    views = (("outline", 20), ("filled", 20), ("duotone", 20), ("outline", 16))
    cw = sum(px * zoom + 6 for _, px in views) + 10
    ch = 20 * zoom + 16
    rows = (len(icons) + cols - 1) // cols
    im = Image.new("RGB", (cols * cw, rows * ch), "white")
    dr = ImageDraw.Draw(im)
    for i, (n, _, _) in enumerate(icons):
        x, y = (i % cols) * cw + 4, (i // cols) * ch + 2
        for mode, px in views:
            ink = "#2457D6" if mode == "filled" else "#1F2329"
            s = svgs[(mode, px, n)].replace("currentColor", ink)
            png = resvg_py.svg_to_bytes(svg_string=s, width=px, height=px, skip_system_fonts=True)
            g = Image.open(io.BytesIO(bytes(png))).convert("RGBA")
            g = g.resize((px * zoom, px * zoom), Image.NEAREST)
            bg = Image.new("RGBA", g.size, "#EAF0FD" if mode == "filled" else "#F6F7F9")
            bg.alpha_composite(g)
            im.paste(bg, (x, y))
            x += px * zoom + 6
        dr.text(((i % cols) * cw + 4, y + 20 * zoom + 1), n[:40], fill="black", font=font)
    im.save(path)


def sheet_themes(icons, svgs, outdir):
    """icons_light.png / icons_dark.png: every icon at 16/20/24 (outline, 2x), name below."""
    os.makedirs(outdir, exist_ok=True)
    font = ImageFont.load_default()
    cols, z = 10, 2
    cw, ch = (16 + 20 + 24) * z + 5 * 6 + 8, 24 * z + 16
    rows = (len(icons) + cols - 1) // cols
    for theme, bg, ink, fg in (("light", "#FFFFFF", "#1F2329", "#555"), ("dark", "#15171B", "#E6E8EB", "#9AA")):
        im = Image.new("RGB", (cols * cw, rows * ch), bg)
        dr = ImageDraw.Draw(im)
        for i, (n, _, _) in enumerate(icons):
            x0, y = (i % cols) * cw + 4, (i // cols) * ch + 2
            x = x0
            for px in (16, 20, 24):
                svg = svgs[("outline", px, n)].replace("currentColor", ink)
                png = resvg_py.svg_to_bytes(svg_string=svg, width=px, height=px, skip_system_fonts=True)
                g = Image.open(io.BytesIO(bytes(png))).convert("RGBA").resize((px * z, px * z), Image.NEAREST)
                im.paste(g, (x, y + (24 - px) * z // 2), g)
                x += px * z + 6
            dr.text((x0, y + 24 * z + 1), n[:34], fill=fg, font=font)
        im.save(os.path.join(outdir, f"icons_{theme}.png"))


def svgs_names(icons):
    return [n for n, _, _ in icons]


def missing_command_icons(names):
    """Icon ids named by commands.d that the generator does not draw (Rust test mirror)."""
    root = pathlib.Path(__file__).resolve().parents[2] / "crates/ui/data/commands.d"
    want = set()
    for f in sorted(root.glob("*.json")):
        data = json.loads(f.read_text(encoding="utf-8"))
        rows = data if isinstance(data, list) else data.get("commands", [])
        want |= {r["icon"] for r in rows if r.get("icon")}
    return sorted(want - set(names))


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--sheet")
    ap.add_argument("--only", default="")
    ap.add_argument("--zoom", type=int, default=3)
    ap.add_argument("--themes", help="dir for icons_light.png / icons_dark.png (16/20/24)")
    ap.add_argument("--no-checks", action="store_true")
    a = ap.parse_args()
    icons = gen.load()
    only = set(x for x in a.only.split(",") if x)
    svgs = {(mode, size, n): core.svg(n, fn, size, mode)
            for n, _, fn in icons for mode in core.MODES for size in core.SIZES}
    if a.sheet:
        sel = [i for i in icons if not only or i[0] in only]
        sheet(sel, svgs, a.sheet, a.zoom)
    if a.themes:
        sheet_themes([i for i in icons if not only or i[0] in only], svgs, a.themes)
    if a.no_checks:
        return
    bad, _ = run_checks(icons, svgs, only or None)
    if not only:
        bad += [f"{n}: command icon not drawn" for n in missing_command_icons(svgs_names(icons))]
    for b in bad[:200]:
        print(b)
    print(len(icons), "icons,", len(bad), "problems")
    sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
