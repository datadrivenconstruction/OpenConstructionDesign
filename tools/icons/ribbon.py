# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""Ribbon direction: flat role colours, 1 px ink edge, oblique building elements (ICONS_V4.md).

Usage: python tools/icons/ribbon.py [--check] [--preview]
Writes assets/icons3/<16|20|24>/<name>.svg for every glyph of the set and assets/icons3/preview.html.
Colours are the light values of the `icon.*` tokens in docs/design/tokens.json; the app swaps them
for the theme's values (crates/ui/src/icons.rs). Neutral control glyphs stay `currentColor`.
Geometry: the hero building elements below are drawn here; every other glyph reuses its drawing
from glyphs_*.py, re-snapped for a 1 px edge, with fills and lines coloured by the glyph's role."""
import argparse
import io
import json
import os
import re
import shutil
import sys

HERE = os.path.dirname(os.path.abspath(__file__))
ROOT = os.path.abspath(os.path.join(HERE, "..", ".."))
OUT = os.path.join(ROOT, "assets", "icons3")
sys.path.insert(0, HERE)

import core  # noqa: E402
from core import G, V, fmt  # noqa: E402

TOKENS = json.load(open(os.path.join(ROOT, "docs", "design", "tokens.json"), encoding="utf-8"))
LIGHT = {k[5:]: v for k, v in TOKENS["color"]["light"].items() if k.startswith("icon.")}
DARK = {k[5:]: v for k, v in TOKENS["color"]["dark"].items() if k.startswith("icon.")}
assert len(set(LIGHT.values())) == len(LIGHT), "icon.* light values must be unique"
SIZES = (24, 20, 16)

# ---------------------------------------------------------------- badges keep their kind
_badge = core.badge


def _tagged_badge(g, kind):
    return [("b" + kind + ":" + r[1:], d) for r, d in _badge(g, kind)]


core.badge = _tagged_badge
import gen  # noqa: E402

BADGE = {"plus": "ok", "tick": "ok", "in": "ok", "minus": "cut.strong", "x": "cut.strong",
         "out": "action.strong", "up": "action.strong", "down": "action.strong",
         "refresh": "action.strong", "link": "action.strong", "pencil": "action.strong",
         "spark": "ai"}


class G1(G):
    """1 px edge at every master: edge coordinates (.75/.25 on the 24 grid) land on pixel centres."""

    def __init__(self, size, ax=(1.0, 0.0), ay=(1.0, 0.0)):
        super().__init__(size, ax, ay)
        self.w = 1.0

    def sub(self, x0, y0, x1, y1):
        c = super().sub(x0, y0, x1, y1)
        return G1(self.n, c.ax, c.ay)

    def _snap(self, v, a):
        if isinstance(v, V):
            if self.ident and v.a[self.n] is not None:
                x = v.a[self.n]
                f = round((x % 1) * 100)
                return x + 0.25 if f == 25 else x - 0.25 if f == 75 else x
            v = v.a[24]
        u = (a[0] * v + a[1]) * self.k
        if core._edge(v) is None:
            return round(u, 3)
        return round(u - 0.5) + 0.5


# ---------------------------------------------------------------- roles
# role: (fill tone, line colour). Fill tones with `.top` / `.side` give oblique faces.
ROLES = {
    "subject": ("subject", "line"), "edit": ("subject", "action.strong"),
    "annot": ("annot", "annot.strong"), "paper": ("paper", "line"),
    "context": ("context", "line"), "wood": ("wood", "line"), "glass": ("glass", "line"),
    "mep": ("mep", "mep.side"), "cut": ("cut", "cut.strong"), "ok": ("ok", "ok"),
    "warn": ("action", "action.strong"), "ai": ("ai.light", "ai"),
}


def _words(s):
    return s.split()


BY_NAME = {}
for role, names in {
    "mono": "chevron-down chevron-up chevron-left chevron-right check-mark sort-asc sort-desc close "
            "close-tab next-tab tabs plus arrow-left arrow-right move-up move-down layout-ribbon "
            "layout-context layout-split layout-focus layout-table layout-next layout-prev "
            "layout-settings fold-ribbon fold-options fold-status clean-screen recent-bar "
            "context-menu undo redo maximize tile repeat setup",
    "ai": "spark ai-build spark-why spark-fix fold-ai-work fold-ai-preview why tips ctx-tips",
    "cut": "delete demolish purge cut cut-geom unjoin-geom disconnect level-delete diff-deleted "
           "level-error clash clash-report mass-subtract roof-unjoin wall-detach section-caps",
    "ok": "check diff-added level-success system-check",
    "warn": "warning diff-changed issues merge-conflict diff-refused",
    "wood": "door stair railing material flip-facing flip-hand fascia paint",
    "glass": "window skylight curtain curtain-face",
    "context": "site toposolid pad boundary subregion contours flatten smooth edit-points underlay "
               "settings units theme language keyboard project-info params project-params schedules "
               "properties library browser types layers phases design-options object-styles "
               "template find query goto hud perf-stats record script filter group ungroup hide "
               "isolate reveal lock pin unpin setup gen-rules gfx-auto gfx-balanced gfx-quality "
               "gfx-speed snaps snap-3d ortho parallel projection walk zoom-fit zoom-sel "
               "palette help reference whats-new where-is tutorial globe level-info eyedropper "
               "slab footing-all raft",
    "paper": "select sheet sheet-duplicate sheet-duplicate-title sheet-list sheet-set "
             "sheets-levels drawing-set place-view drafting-new drafting-open to-detail table "
             "table-open file-new pdf-out import-pdf dxf-out import-cad reimport source xlsx-out "
             "xlsx-in xlsx-sync xlsx-delta gltf-out stl-out obj-out step-out 3mf-out ifc-out ifc-out-4 ifc-out-4x3 ifc-out-2x3 "
             "ifc-delta deliver print import save save-as autosave open open-big sample history "
             "diff compare-version merge-file legend legend-place numbering revision-issue "
             "native-build link link-add link-reload links load-family timeline-family "
             "rebar-schedule sel-sets set-save copy-params paste-params clip-copy",
    "annot": "text leader detail-line spot-elev spot-coord dim-angular dim-radial dim-chain "
             "dim-auto dimension tag tag-all region cloud level grid section elevation ref-plane "
             "workplane wp-level wp-face wp-points wp-ref room room-auto room-sep zone measure "
             "measure-3d at-grids grid-set grid-edit distribute plan-grid detail-comp "
             "section-here plan-here spot-elev level-up level-down level-edit",
    "mep": "duct pipe conduit tray fitting fixture equipment light device air-terminal route "
           "connect system system-browser",
}.items():
    for n in _words(names):
        BY_NAME[n] = role

BY_MODULE = {"glyphs_arch": "subject", "glyphs_base": "subject", "glyphs_doc": "paper",
             "glyphs_edit": "edit", "glyphs_form": "subject"}


def role_of(name, fn):
    if name in BY_NAME:
        return BY_NAME[name]
    for pre, r in (("level-", "annot"), ("dim-", "annot"), ("ifc-", "paper"), ("xlsx-", "paper"),
                   ("style-", "context"), ("sel-", "edit"), ("mass-", "subject"),
                   ("roof-", "subject"), ("wall-", "subject")):
        if name.startswith(pre):
            return r
    return BY_MODULE.get(fn.__module__, "subject")


# ---------------------------------------------------------------- hero drawings (oblique)
# Each returns elements (kind, d, colour): S fill + edge, F fill only, L line, K solid mark.

def _pp(pts, close=True):
    return "M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in pts) + ("Z" if close else "")


def _depth(g, d24):
    return {24: d24, 20: max(2, round(d24 * 0.8)), 16: max(2, round(d24 * 0.6))}[g.n]


def obox(g, x0, y0, x1, y1, d24, tone, top=True, side=True):
    """Front rectangle (24-grid edges) extruded up-right at 45 degrees by a whole-pixel depth."""
    a, b, t, u = g.sx(x0), g.sx(x1), g.sy(y0), g.sy(y1)
    k = _depth(g, d24)
    els = [("S", _pp([(a, t), (b, t), (b, u), (a, u)]), tone)]
    if top:
        els.append(("S", _pp([(a, t), (a + k, t - k), (b + k, t - k), (b, t)]), tone + ".top"))
    if side:
        els.append(("S", _pp([(b, t), (b + k, t - k), (b + k, u - k), (b, u)]), tone + ".side"))
    return els


def h_wall(g):
    els = obox(g, 2.75, 9.75, 17.25, 20.25, 4, "subject")
    if g.n > 16:  # coursing: reads as masonry at 20 and up, noise at 16
        a, b, t, u = g.sx(2.75), g.sx(17.25), g.sy(9.75), g.sy(20.25)
        m1, m2 = round((t * 2 + u) / 3) + 0.5, round((t + u * 2) / 3) + 0.5
        x1, x2, x3 = round(a + (b - a) / 3) + 0.5, round(a + (b - a) * 2 / 3) + 0.5, \
            round((a + b) / 2) + 0.5
        d = (f"M{fmt(a)} {fmt(m1)}H{fmt(b)}M{fmt(a)} {fmt(m2)}H{fmt(b)}"
             f"M{fmt(x1)} {fmt(t)}V{fmt(m1)}M{fmt(x2)} {fmt(t)}V{fmt(m1)}"
             f"M{fmt(x3)} {fmt(m1)}V{fmt(m2)}M{fmt(x1)} {fmt(m2)}V{fmt(u)}M{fmt(x2)} {fmt(m2)}V{fmt(u)}")
        els.append(("L", d, "subject.side"))
    return els


def h_floor(g):
    return obox(g, 2.75, 14.75, 15.25, 18.25, 6, "subject")


def h_slab(g):
    els = obox(g, 2.75, 12.75, 15.25, 19.25, 6, "context")
    if g.n > 16:  # cut concrete: two hatch strokes on the front
        a, b, t, u = g.sx(2.75), g.sx(15.25), g.sy(12.75), g.sy(19.25)
        h = u - t
        d = "".join(f"M{fmt(x)} {fmt(u)}L{fmt(x + h)} {fmt(t)}"
                    for x in (a + 2, a + 2 + (b - a) / 2.5) if x + h < b)
        els.append(("L", d, "context.side"))
    return els


def h_column(g):
    return obox(g, 7.75, 7.75, 12.25, 21.25, 4, "subject")


def h_beam(g):
    a, b, t, u = g.sx(2.75), g.sx(8.25), g.sy(14.75), g.sy(20.25)
    k = {24: 13, 20: 11, 16: 8}[g.n]
    return [("S", _pp([(a, t), (a + k, t - k), (b + k, t - k), (b, t)]), "subject.top"),
            ("S", _pp([(b, t), (b + k, t - k), (b + k, u - k), (b, u)]), "subject.side"),
            ("S", _pp([(a, t), (b, t), (b, u), (a, u)]), "subject")]


def h_footing(g):
    return (obox(g, 2.75, 15.75, 17.25, 20.25, 4, "context")
            + obox(g, 7.75, 6.75, 11.25, 15.75, 3, "subject"))


def h_stair(g):
    n = 4 if g.n > 16 else 3
    a, b, t, u = g.sx(2.75), g.sx(17.25), g.sy(6.75), g.sy(20.25)
    k = _depth(g, 4)
    sw, sh = (b - a) / n, (u - t) / n
    xs = [round(a + i * sw - 0.5) + 0.5 for i in range(n)] + [b]
    ys = [round(u - (i + 1) * sh - 0.5) + 0.5 for i in range(n)]
    prof = [(a, u), (a, ys[0])]
    for i in range(n):
        prof += [(xs[i + 1], ys[i])] + ([(xs[i + 1], ys[i + 1])] if i + 1 < n else [])
    prof += [(b, u)]
    els = [("S", _pp(prof), "wood")]
    for i in range(n):
        els.append(("S", _pp([(xs[i], ys[i]), (xs[i] + k, ys[i] - k), (xs[i + 1] + k, ys[i] - k),
                              (xs[i + 1], ys[i])]), "wood.top"))
    els.append(("S", _pp([(b, ys[-1]), (b + k, ys[-1] - k), (b + k, u - k), (b, u)]), "wood.side"))
    return els


def h_roof(g):
    """House elevation under a pitched roof: the roof plane is the subject (wood tone)."""
    a, b, u = g.sx(2.75), g.sx(14.25), g.sy(20.25)
    k = {24: 7, 20: 6, 16: 4}[g.n]
    w = u - {24: 6, 20: 5, 16: 4}[g.n]
    ridge = (round((a + b) / 2 - 0.5) + 0.5, w - round((b - a) * 0.5))
    return [("S", _pp([(a, u), (a, w), ridge, (b, w), (b, u)]), "subject"),
            ("S", _pp([(b, w), (b + k, w - k), (b + k, u - k), (b, u)]), "subject.side"),
            ("S", _pp([ridge, (ridge[0] + k, ridge[1] - k), (b + k, w - k), (b, w)]), "wood")]


def h_door(g):
    a, b, t, u = g.sx(3.75), g.sx(20.25), g.sy(2.75), g.sy(21.25)
    i0, i1, it = g.sx(7.75), g.sx(16.25), g.sy(6.75)
    frame = _pp([(a, u), (a, t), (b, t), (b, u), (i1, u), (i1, it), (i0, it), (i0, u)])
    hx, hy = i1 - (2 if g.n > 16 else 1.5), (it + u) / 2 + 1
    r = 0.9 if g.n > 16 else 0.7
    knob = (f"M{fmt(hx - r)} {fmt(hy)}A{fmt(r)} {fmt(r)} 0 1 0 {fmt(hx + r)} {fmt(hy)}"
            f"A{fmt(r)} {fmt(r)} 0 1 0 {fmt(hx - r)} {fmt(hy)}Z")
    return [("S", frame, "context"), ("S", _pp([(i0, it), (i1, it), (i1, u), (i0, u)]), "wood"),
            ("K", knob, "edge")]


def h_window(g):
    a, b, t, u = g.sx(3.75), g.sx(20.25), g.sy(3.75), g.sy(19.25)
    f = 2 if g.n > 16 else 1
    mx = round((a + b) / 2 - 0.5) + 0.5
    my = round(t + (u - t) * 0.42 - 0.5) + 0.5
    sill = g.sy(21.25)
    return [("S", _pp([(a, t), (b, t), (b, u), (a, u)]), "context"),
            ("S", _pp([(a + f, t + f), (b - f, t + f), (b - f, u - f), (a + f, u - f)]), "glass"),
            ("L", f"M{fmt(mx)} {fmt(t + f)}V{fmt(u - f)}M{fmt(a + f)} {fmt(my)}H{fmt(b - f)}",
             "edge"),
            ("S", _pp([(a - 1, u), (b + 1, u), (b + 1, sill), (a - 1, sill)]), "context.side")]


def h_curtain(g):
    els = obox(g, 2.75, 6.75, 15.25, 20.25, 5, "glass", top=True, side=True)
    els[1] = ("S", els[1][1], "context.top")
    els[2] = ("S", els[2][1], "context.side")
    a, b, t, u = g.sx(2.75), g.sx(15.25), g.sy(6.75), g.sy(20.25)
    cols = 3 if g.n > 16 else 2
    rows = 3 if g.n > 16 else 2
    d = "".join(f"M{fmt(round(a + (b - a) * i / cols - 0.5) + 0.5)} {fmt(t)}V{fmt(u)}"
                for i in range(1, cols))
    d += "".join(f"M{fmt(a)} {fmt(round(t + (u - t) * i / rows - 0.5) + 0.5)}H{fmt(b)}"
                 for i in range(1, rows))
    return els + [("L", d, "edge")]


def h_opening(g):
    els = obox(g, 2.75, 7.75, 17.25, 20.25, 4, "subject")
    a, b, t, u = g.sx(6.75), g.sx(13.25), g.sy(10.75), g.sy(17.25)
    return els + [("S", _pp([(a, t), (b, t), (b, u), (a, u)]), "context.side")]


def h_ramp(g):
    a, b, t, u = g.sx(2.75), g.sx(17.25), g.sy(10.75), g.sy(20.25)
    k = _depth(g, 4)
    return [("S", _pp([(a, u), (b, t), (b, u)]), "subject"),
            ("S", _pp([(a, u), (a + k, u - k), (b + k, t - k), (b, t)]), "subject.top"),
            ("S", _pp([(b, t), (b + k, t - k), (b + k, u - k), (b, u)]), "subject.side")]


def h_3d(g):
    return obox(g, 4.75, 9.75, 15.25, 20.25, 6, "subject")


def h_duct(g):
    a, b, t, u = g.sx(2.75), g.sx(9.25), g.sy(12.75), g.sy(19.25)
    k = {24: 10, 20: 8, 16: 6}[g.n]
    return [("S", _pp([(a, t), (a + k, t - k), (b + k, t - k), (b, t)]), "mep.top"),
            ("S", _pp([(b, t), (b + k, t - k), (b + k, u - k), (b, u)]), "mep.side"),
            ("S", _pp([(a, t), (b, t), (b, u), (a, u)]), "mep")]


def _circle(cx, cy, r):
    return (f"M{fmt(cx - r)} {fmt(cy)}A{fmt(r)} {fmt(r)} 0 1 0 {fmt(cx + r)} {fmt(cy)}"
            f"A{fmt(r)} {fmt(r)} 0 1 0 {fmt(cx - r)} {fmt(cy)}Z")


def h_pipe(g):
    s = g.n / 24
    r = round(3.5 * s * 2) / 2
    cx, cy = round(7 * s) + 0.5, round(17 * s) - 0.5
    k = {24: 9, 20: 7, 16: 5}[g.n]
    q = r * 0.7071
    body = _pp([(cx + q, cy + q), (cx + q + k, cy + q - k), (cx - q + k, cy - q - k),
                (cx - q, cy - q)])
    return [("S", _circle(cx + k, cy - k, r), "mep.side"), ("S", body, "mep"),
            ("S", _circle(cx, cy, r), "mep.top")]


def h_equipment(g):
    els = obox(g, 2.75, 8.75, 15.25, 20.25, 5, "mep")
    a, b, t, u = g.sx(2.75), g.sx(15.25), g.sy(8.75), g.sy(20.25)
    r = round((b - a) * 0.28 * 2) / 2
    return els + [("S", _circle((a + b) / 2, (t + u) / 2, r), "mep.top")]


HEROES = {"wall": h_wall, "floor": h_floor, "slab": h_slab, "column": h_column, "beam": h_beam,
          "footing": h_footing, "stair": h_stair, "roof": h_roof, "door": h_door,
          "window": h_window, "curtain": h_curtain, "opening": h_opening, "ramp": h_ramp,
          "3d-view": h_3d, "duct": h_duct, "pipe": h_pipe, "equipment": h_equipment}


# ---------------------------------------------------------------- conversion of the v4 drawings

SURF = ("S", "F", "SF", "FD")


def _convert(els, role):
    """v4 elements -> ribbon (kind, d, colour) in paint order: fills, edges, lines, marks."""
    fill, line = ROLES[role]
    tone = lambda t: t if t in LIGHT else fill  # noqa: E731
    filled = any(r in SURF for r, _ in els)
    fills, edges, lines, marks = [], [], [], []
    for r, d in els:
        if r in ("S", "F", "SF"):
            fills.append(("F", d, fill))
        elif r == "FD":
            fills.append(("F", d, tone(fill + ".top")))
        if r in ("S", "SF"):
            edges.append(("L", d, "edge"))
        elif r == "D":  # inner edges over a fill stay dark in both themes
            lines.append(("L", d, "inner" if filled else line))
        elif r == "H":
            lines.append(("L", d, "detail"))
        elif r == "L":
            lines.append(("L", d, line))
        elif r in ("K", "KX"):
            marks.append(("K", d, "inner" if filled else line))
        elif r in ("A", "AX"):
            marks.append(("K", d, "ai"))
    return fills + edges + lines + marks


def _surface(els):
    return any(r in SURF
               for r, _ in els if not r.startswith("b"))


def _paint(els, w=None):
    out = []
    sw = "" if w is None else f' stroke-width="{fmt(w)}"'
    for kind, d, c in els:
        c = LIGHT[c]
        if kind == "S":
            out.append(f'<path d="{d}" fill="{c}" stroke="{LIGHT["edge"]}"{sw}/>')
        elif kind == "F":
            out.append(f'<path d="{d}" fill="{c}"/>')
        elif kind == "L":
            out.append(f'<path d="{d}" stroke="{c}"{sw}/>')
        else:
            out.append(f'<path d="{d}" fill="{c}" stroke="none"/>')
    return out


def _badges(fn, size):
    """Badge elements drawn on the v4 grid (its 1.5 px weight), coloured by kind."""
    out = []
    for r, d in fn(G(size)):
        if not r.startswith("b"):
            continue
        kind, _, rr = r[1:].rpartition(":")
        c = BADGE.get(kind, "line")
        if rr in ("S", "SF", "F"):
            out.append(("S", d, "paper" if c == "line" else c))
        elif rr in ("K", "KX", "A", "AX"):
            out.append(("K", d, c))
        else:
            out.append(("L", d, c))
    return out


def render(name, fn, size):
    """SVG text of `name` at master `size`, or None for a neutral glyph (written as outline)."""
    role = role_of(name, fn)
    if role == "mono":
        return core.svg(name, _plain(fn), size, "outline")
    if name in HEROES:
        base, badge, w = HEROES[name](G1(size)), [], 1.0
    else:
        probe = fn(G(24))
        if _surface(probe):
            g = G1(size)
            base, w = _convert([(r, d) for r, d in fn(g) if not r.startswith("b")], role), 1.0
        else:
            g = G(size)
            base, w = _convert([(r, d) for r, d in fn(g) if not r.startswith("b")], role), \
                core.W[size]
        badge = _badges(fn, size)
    body = _paint(base)
    if badge:
        bw = core.W[size]
        cut = "".join(f'<path d="{d}" fill="{"#000" if k != "L" else "none"}" stroke="#000" '
                      f'stroke-width="{fmt(bw + 2 * core.HALO)}"/>' for k, d, _ in badge)
        hid = f"r-{name}-{size}"
        body = ([f'<mask id="{hid}" maskUnits="userSpaceOnUse" x="0" y="0" width="{size}" '
                 f'height="{size}"><rect width="{size}" height="{size}" fill="#fff"/>{cut}</mask>',
                 f'<g mask="url(#{hid})">'] + body + ["</g>"] + _paint(badge, bw))
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
            f'viewBox="0 0 {size} {size}" fill="none" stroke-width="{fmt(w)}" '
            f'stroke-linecap="round" stroke-linejoin="round">' + "".join(body) + "</svg>\n")


def _plain(fn):
    """Badge roles back to the v4 form for neutral glyphs drawn by core.svg."""
    def f(g):
        return [("b" + r.rpartition(":")[2].lstrip("b") if r.startswith("b") else r, d) for r, d in fn(g)]
    return f


def build():
    icons = gen.load()
    files = {}
    for size in SIZES:
        for name, _ru, fn in icons:
            files[(size, name)] = render(name, fn, size)
    return icons, files


def write(files):
    for size in SIZES:
        d = os.path.join(OUT, str(size))
        if os.path.isdir(d):
            shutil.rmtree(d)
        os.makedirs(d)
    for (size, name), s in files.items():
        with open(os.path.join(OUT, str(size), name + ".svg"), "w", encoding="utf-8",
                  newline="\n") as f:
            f.write(s)


def to_dark(svg):
    for k, v in LIGHT.items():
        svg = svg.replace(v, DARK[k])
    return svg


# ---------------------------------------------------------------- check (mirrors the Rust tests)

def check(icons, files):
    import numpy as np
    import resvg_py
    from PIL import Image

    panel = {"light": TOKENS["color"]["light"]["bg.panel"], "dark": TOKENS["color"]["dark"]["bg.panel"]}

    def rgba(svg, px):
        s = svg.replace("currentColor", "#FFFFFF")
        png = resvg_py.svg_to_bytes(svg_string=s, width=px, height=px, skip_system_fonts=True)
        return np.asarray(Image.open(io.BytesIO(bytes(png))).convert("RGBA")).astype(np.float32) / 255

    def lum(c):
        c = np.where(c <= 0.03928, c / 12.92, ((c + 0.055) / 1.055) ** 2.4)
        return 0.2126 * c[..., 0] + 0.7152 * c[..., 1] + 0.0722 * c[..., 2]

    def hexrgb(h):
        return np.array([int(h[i:i + 2], 16) / 255 for i in (1, 3, 5)], dtype=np.float32)

    bad, masks16, masks20 = [], [], []
    colours = set(LIGHT.values()) | {"#000", "#fff"}
    for name, _ru, _fn in icons:
        for px in (16, 20, 24, 32):
            m = 16 if px < 20 else 20 if px < 24 else 24
            s = files[(m, name)]
            for h in re.findall(r"#[0-9A-Fa-f]{3,6}\b", s):
                if h not in colours:
                    bad.append(f"{name}: colour {h}")
            for theme, svg in (("light", s), ("dark", to_dark(s))):
                im = rgba(svg, px)
                a = im[..., 3]
                frac = (a > 0).mean()
                if not 0.03 < frac < 0.85:
                    bad.append(f"{name}@{px} {theme}: ink {frac:.2f}")
                if "currentColor" not in s:
                    bg = hexrgb(panel[theme])
                    rgb = im[..., :3] / np.maximum(a[..., None], 1e-6)
                    lp = lum(np.clip(rgb, 0, 1))
                    lb = lum(bg[None, None, :])
                    hi, lo = np.maximum(lp, lb), np.minimum(lp, lb)
                    vis = ((hi + 0.05) / (lo + 0.05) >= 2.2) & (a > 0.6)
                    if vis.sum() < px * 0.75:
                        bad.append(f"{name}@{px} {theme}: {int(vis.sum())} px stand out of the panel")
                if px == 16 and theme == "light":
                    masks16.append((name, im))
                if px == 20 and theme == "light":
                    masks20.append((name, im))
                    edge = np.concatenate([a[0], a[-1], a[:, 0], a[:, -1]])
                    if edge.max() >= 0.1:
                        bad.append(f"{name}: ink on the 1 px margin at 20")

    def alike(ms, bar):
        out = []
        for i, (n1, a) in enumerate(ms):
            for n2, b in ms[i + 1:]:
                d = float(np.abs(a - b).sum() / max(np.maximum(a, b).sum(), 1e-9))
                if d < bar:
                    out.append(f"{n1} ~ {n2}: {d:.3f}")
        return out

    bad += alike(masks16, 0.03) + alike(masks20, 0.03)
    return bad


# ---------------------------------------------------------------- contact sheet

def _scoped(svg, suffix):
    ids = re.findall(r'id="([^"]+)"', svg)
    for i in ids:
        svg = svg.replace(f'id="{i}"', f'id="{i}{suffix}"').replace(f"url(#{i})", f"url(#{i}{suffix})")
    return svg


def preview(icons, files, cmd_names):
    th = {t: TOKENS["color"][t] for t in ("light", "dark")}
    cells = []
    for name, ru, fn in icons:
        role = role_of(name, fn)
        row = []
        for t in ("light", "dark"):
            for px in (16, 24, 32):
                m = 16 if px < 20 else 20 if px < 24 else 24
                s = files[(m, name)]
                s = to_dark(s) if t == "dark" else s
                s = _scoped(s, f"-{t}-{px}")
                s = re.sub(r'width="\d+" height="\d+"', f'width="{px}" height="{px}"', s, count=1)
                row.append(f'<span class="z {t}">{s.strip()}</span>')
        hero = " hero" if name in HEROES else ""
        cmd = "" if name in cmd_names else " nocmd"
        cells.append(f'<div class="c{hero}{cmd}" data-role="{role}"><div class="r">{"".join(row)}'
                     f'</div><div class="n">{name}</div><div class="m">{role}</div></div>')
    legend = "".join(
        f'<span class="sw"><i style="background:{LIGHT[k]}"></i><i style="background:{DARK[k]}"></i>'
        f'icon.{k}</span>' for k in LIGHT)
    css = f"""
:root{{--bg:#EDEFF2;--fg:#1B1F27;--mut:#575E6D;--card:#FFFFFF;--line:#DEE1E7}}
@media (prefers-color-scheme: dark){{:root{{--bg:#15171B;--fg:#E6E9EE;--mut:#9AA3AF;--card:#1D2025;--line:#2C3138}}}}
body{{margin:0;padding:24px 16px;background:var(--bg);color:var(--fg);font:13px/1.45 system-ui,sans-serif}}
h1{{font-size:20px;margin:0 0 4px}}p{{max-width:960px;color:var(--mut);margin:0 0 10px}}
.grid{{display:grid;grid-template-columns:repeat(auto-fill,minmax(260px,1fr));gap:6px}}
.c{{background:var(--card);border:1px solid var(--line);border-radius:6px;padding:6px 8px}}
.c.hero{{border-color:#5F92D6}}
.r{{display:flex;align-items:center;gap:0}}
.z{{display:inline-flex;align-items:center;justify-content:center;width:40px;height:40px}}
.z.light{{background:{th['light']['bg.panel']};color:{th['light']['text.primary']}}}
.z.dark{{background:{th['dark']['bg.panel']};color:{th['dark']['text.primary']}}}
.n{{font:12px ui-monospace,monospace;margin-top:4px}}.m{{font-size:11px;color:var(--mut)}}
.sw{{display:inline-flex;align-items:center;gap:4px;margin:0 10px 4px 0;font:11px ui-monospace,monospace}}
.sw i{{display:inline-block;width:12px;height:12px;border:1px solid var(--line)}}
.bar{{margin:10px 0 14px}}select{{font:inherit}}"""
    roles = sorted(set(ROLES) | {"mono"})
    opts = "".join(f"<option>{r}</option>" for r in roles)
    js = ("document.getElementById('f').onchange=e=>{const v=e.target.value;"
          "document.querySelectorAll('.c').forEach(c=>{c.style.display=(!v||c.dataset.role===v)?'':'none'})}")
    n_cmd = sum(1 for n, _, _ in icons if n in cmd_names)
    return f"""<!doctype html><html lang="en"><head><meta charset="utf-8">
<meta name="viewport" content="width=device-width,initial-scale=1"><title>Ribbon Icon Set</title>
<style>{css}</style></head><body>
<h1>Ribbon icon set</h1>
<p>{len(icons)} glyphs ({n_cmd} named by commands; {len(HEROES)} oblique building elements outlined
in blue). Each row: 16, 24, 32 px on the light panel, then on the dark panel. 16 is its own master,
20 and 24 their own; 32 scales the 24 master. Colours are the <code>icon.*</code> tokens of
docs/design/tokens.json (light / dark below); neutral control glyphs follow the text colour.
Generated by tools/icons/ribbon.py.</p>
<div class="bar">{legend}</div>
<div class="bar">Role <select id="f"><option value="">all</option>{opts}</select></div>
<div class="grid">{"".join(cells)}</div><script>{js}</script></body></html>
"""


def command_icons():
    import glob
    out = set()
    data = os.path.join(ROOT, "crates", "ui", "data")
    for f in [os.path.join(data, "commands.json")] + glob.glob(os.path.join(data, "commands.d", "*.json")):
        d = json.load(open(f, encoding="utf-8"))
        items = d if isinstance(d, list) else d.get("commands", [])
        for c in items:
            if isinstance(c, dict) and c.get("icon"):
                out.add(c["icon"].removesuffix(".fill"))
    out |= set(json.load(open(os.path.join(data, "command_icons.json"), encoding="utf-8")).values())
    return out


def main():
    ap = argparse.ArgumentParser()
    ap.add_argument("--check", action="store_true")
    ap.add_argument("--no-write", action="store_true")
    a = ap.parse_args()
    icons, files = build()
    names = {n for n, _, _ in icons}
    cmds = command_icons()
    missing = sorted(cmds - names)
    assert not missing, f"commands without a glyph: {missing}"
    if not a.no_write:
        write(files)
        with open(os.path.join(OUT, "preview.html"), "w", encoding="utf-8", newline="\n") as f:
            f.write(preview(icons, files, cmds))
    print(len(icons), "glyphs,", len(files), "files,", len(cmds), "command icons ->", OUT)
    if a.check:
        bad = check(icons, files)
        print(len(bad), "problems")
        for b in bad[:60]:
            print(" ", b)
        sys.exit(1 if bad else 0)


if __name__ == "__main__":
    main()
