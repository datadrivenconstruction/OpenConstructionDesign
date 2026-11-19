# SPDX-License-Identifier: AGPL-3.0-only
# SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
"""The 24 masters of the v2 set (wall ... model-check): the reference drawings for every family."""
import math

from core import V, chevron, cube, fmt, g_poly, icon, model, px_circ


@icon("select", "Select")
def select(g):
    if g.n == 16 and g.ident:  # redrawn for 1 px: vertical spine and 45/2:1 edges stay solid
        return [("S", g_poly([(3.5, 2.5), (3.5, 13.5), (6.5, 10.5), (8.5, 14.5), (10.5, 13.5),
                               (8.5, 9.5), (10.5, 9.5)]))]
    pts = [(4.5, 4.5), (19.5, 9.6), (14.15, 11.65), (19.55, 17.05), (17.05, 19.55), (11.65, 14.15),
           (9.6, 19.5)]
    return [("S", g.poly(pts, True))]


@icon("undo", "Undo")
def undo(g):
    return [("L", g.poly([(8.25, 3.75), (3.75, 8.25), (8.25, 12.75)])),
            ("L", g.p(("M", 3.75, 8.25), ("H", 14.25), ("A", None, 0, 1, 14.25, 20.25), ("H", 8.25)))]


@icon("wall", "Wall")
def wall(g):
    t = V(9.25, 7.75, 6.5)
    L = g.poly([(3.75, 3.75), (20.25, 3.75), (20.25, t), (t, t), (t, 20.25), (3.75, 20.25)], True)
    a, b = g.sx(3.75), g.sx(t)
    ay, by = g.sy(3.75), g.sy(t)
    m = b - a
    x2 = b + m * 0.9
    my = by - ay
    y2 = by + my * 0.9
    h1 = f"M{fmt(a)} {fmt(by)}L{fmt(b)} {fmt(ay)}"
    h2 = f"M{fmt(x2-m)} {fmt(by)}L{fmt(x2)} {fmt(ay)}"
    h3 = f"M{fmt(a)} {fmt(y2)}L{fmt(b)} {fmt(y2-my)}"
    return [("S", L), ("H", h1), ("H", h2), ("H", h3)]


@icon("door", "Door")
def door(g):
    top = V(7.25, 6.25, 4.5)
    sec = g.p(("M", 7.25, 20.25), ("V", top), ("A", None, 0, 1, 20.25, 20.25), ("Z",))
    return [("F", sec), ("S", g.rect(4.75, top, 7.25, 20.25)),
            ("L", g.p(("M", 7.25, top), ("A", None, 0, 1, 20.25, 20.25))),
            ("L", g.p(("M", 2.75, 20.25), ("H", 4.75)))]


@icon("window", "Window")
def window(g):
    return [("S", g.rect(4.75, 3.75, 19.25, 17.25)),
            ("D", g.p(("M", 11.75, 3.75), ("V", 17.25))),
            ("D", g.p(("M", 4.75, V(9.75, 8.75, 6.5)), ("H", 19.25))),
            ("L", g.p(("M", 2.75, 20.25), ("H", 21.25)))]


@icon("floor", "Floor")
def floor(g):
    c, top = cube(g, 2.75, V(20.75, 16.75, 12.5), 6.75, 3.5)
    return [("FD", top)] + c


@icon("roof", "Roof")
def roof(g):
    band = g.poly([(2.75, 12.25), (12, 3.75), (21.25, 12.25), (21.25, 16.25), (12, 7.75),
                   (2.75, 16.25)], True)
    return [("S", band), ("L", g.p(("M", 6.75, 13), ("V", 20.25))),
            ("L", g.p(("M", 17.25, 13), ("V", 20.25)))]


@icon("column", "Column")
def column(g):
    c = V(11.75, 9.75, 7.5)
    return [("S", g.rect(7.75, 7.75, 16.25, 16.25)),
            # grid axes stop short of the column so its edges stay crisp at 20 px
            ("L", g.p(("M", 2.75, c), ("H", 4.75))), ("L", g.p(("M", 19.25, c), ("H", 21.25))),
            ("L", g.p(("M", c, 2.75), ("V", 4.75))), ("L", g.p(("M", c, 19.25), ("V", 21.25)))]


@icon("beam", "Beam")
def beam(g):
    f0, f1 = V(7.25, 6.25, 4.5), V(16.75, 13.75, 11.5)
    w0, w1 = V(9.75, 8.25, 6.5), V(14.25, 11.75, 9.5)
    return [("S", g.poly([(3.75, 3.75), (20.25, 3.75), (20.25, f0), (w1, f0), (w1, f1), (20.25, f1),
                          (20.25, 20.25), (3.75, 20.25), (3.75, f1), (w0, f1), (w0, f0), (3.75, f0)],
                         True))]


@icon("stair", "Stair")
def stair(g):
    a, b = V(9.25, 7.75, 6.5), V(14.75, 12.25, 9.5)
    return [("S", g.poly([(3.75, 20.25), (3.75, b), (a, b), (a, a), (b, a), (b, 3.75), (20.25, 3.75),
                          (20.25, 20.25)], True))]


@icon("room", "Room")
def room(g):
    return [("F", g.rect(3.75, 3.75, 20.25, 20.25)),
            ("L", g.p(("M", 13.75, 20.25), ("H", 20.25), ("V", 3.75), ("H", 3.75), ("V", 20.25),
                      ("H", 8.75))),
            ("D", g.rect(6.75, V(8.75, 6.75, 5.5), 17.25, V(14.75, 12.25, 9.5), 1)),
            ("D", g.p(("M", 9.75, V(11.75, 9.75, 7.5)), ("H", 14.25)))]


@icon("level", "Level")
def level(g):
    y = V(18.25, 15.25, 12.5)
    yt = V(13.75, 11.75, 9.5)
    return [("S", g.poly([(2.75, yt), (11.75, yt), (7.25, y)], True)),
            ("L", g.p(("M", 7.25, yt), ("V", 4.75), ("H", 18.25))),
            ("L", g.p(("M", 2.75, y), ("H", 21.25)))]


@icon("grid", "Grid")
def grid(g):
    x = V(11.75, 9.75, 7.5)
    if g.n == 16 and g.ident:  # 1 px: a larger bubble and an unbroken stem keep it solid
        head = ("M6 1.5H9A2.5 2.5 0 0 1 11.5 4V7A2.5 2.5 0 0 1 9 9.5H6A2.5 2.5 0 0 1 3.5 7V4"
                "A2.5 2.5 0 0 1 6 1.5Z")  # squircle: its flats stay solid at 1 px
        return [("S", head), ("L", "M7.5 9.5V14.5")]
    return [("S", g.circ(7.75, 3.75, 16.25, 12.25)),
            ("L", g.p(("M", x, V(14.75, 12.25, 10.5)), ("V", V(17.25, 14.25, 11.5)))),
            ("L", g.p(("M", x, V(20.25, 16.75, 13.5)), ("V", V(20.26, 16.76, 13.51))))]


@icon("section", "Section")
def section(g):
    y = V(16.25, 13.25, 10.5)
    out = [("L", g.p(("M", 2.75, y), ("H", V(8.25, 6.25, 4.5)))),
           ("L", g.p(("M", V(10.75, 9.25, 7.5), y), ("H", V(13.25, 10.75, 8.5)))),
           ("L", g.p(("M", V(15.75, 13.75, 11.5), y), ("H", 21.25)))]
    for cx in (4.75, 19.25):
        out.append(("L", g.p(("M", cx, y), ("V", 8))))
        out.append(("K", g.poly([(cx - 2, 8.25), (cx, 4.25), (cx + 2, 8.25)], True)))
    return out


@icon("elevation", "Elevation")
def elevation(g):
    x0, x1, y0, y1 = g.sx(3.75), g.sx(14.25), g.sy(6.75), g.sy(17.25)
    cx, cy, r = (x0 + x1) / 2, (y0 + y1) / 2, (x1 - x0) / 2
    tip = g.sx(20.75)
    d = tip - cx
    th = math.acos(r / d)
    tx, ty = cx + r * math.cos(th), r * math.sin(th)
    wedge = (f"M{fmt(tx)} {fmt(cy-ty)}L{fmt(tip)} {fmt(cy)}L{fmt(tx)} {fmt(cy+ty)}"
             f"A{fmt(r)} {fmt(r)} 0 0 0 {fmt(tx)} {fmt(cy-ty)}Z")
    return [("S", g.circ(3.75, 6.75, 14.25, 17.25)), ("K", wedge)]


@icon("sheet", "Sheet")
def sheet(g):
    return [("S", g.rect(2.75, 4.75, 21.25, 19.25, 1)),
            ("D", g.p(("M", 13.75, 19.25), ("V", V(14.25, 12.25, 9.5)), ("H", 21.25)))]


@icon("dimension", "Dimension")
def dimension(g):
    y = V(8.75, 7.25, 5.5)
    t = 1.0
    xa, xb = 3.75, 20.25
    out = [("S", g.rect(3.75, V(15.75, 13.75, 10.5), 20.25, 20.25)),
           ("L", g.p(("M", 2.75, y), ("H", 21.25))),
           ("L", g.p(("M", xa, V(4.75, 3.75, 2.5)), ("V", V(12.75, 10.75, 8.5)))),
           ("L", g.p(("M", xb, V(4.75, 3.75, 2.5)), ("V", V(12.75, 10.75, 8.5))))]
    yy = g.sy(y)
    for x in (g.sx(xa), g.sx(xb)):
        tt = t * g.k * 1.2
        out.append(("L", f"M{fmt(x-tt)} {fmt(yy+tt)}L{fmt(x+tt)} {fmt(yy-tt)}"))
    return out


@icon("tag", "Tag")
def tag(g):
    return [("S", g.rect(8.75, 3.75, 21.25, 12.25, 1)),
            ("D", g.p(("M", 12.25, V(8.25, 7.25, 5.5)), ("H", 17.75))),
            ("L", g.poly([(11.5, 12.25), (5.5, 18.5)])),
            ("K", g.circ(3.75, 17.75, 6.25, 20.25))]


@icon("ifc-out", "Ifc out")
def ifc_out(g):
    return model(g) + [("L", g.p(("M", 15.75, 15.25), ("H", 21.25))),
                       ("L", g.poly([(18.75, 12.75), (21.25, 15.25), (18.75, 17.75)]))]


@icon("native-build", "Native build")
def native_build(g):
    return [("S", g.poly([(3.75, 3.75), (16.75, 3.75), (20.25, 7.25), (20.25, 20.25), (3.75, 20.25)],
                         True)),
            ("D", g.p(("M", 7.75, 3.75), ("V", V(8.25, 7.25, 5.5)), ("H", 14.25), ("V", 3.75))),
            ("D", g.p(("M", 7.25, 20.25), ("V", V(13.75, 11.75, 9.5)), ("H", 16.75), ("V", 20.25)))]


@icon("spark", "Spark")
def spark(g):
    b = g.p(("M", 5.25, 3.75), ("H", 18.75), ("A", 2.5, 0, 1, 21.25, 6.25), ("V", 14.75),
            ("A", 2.5, 0, 1, 18.75, 17.25), ("H", 10.25), ("L", 6.75, 20.75), ("V", 17.25),
            ("H", 5.25), ("A", 2.5, 0, 1, 2.75, 14.75), ("V", 6.25), ("A", 2.5, 0, 1, 5.25, 3.75),
            ("Z",))
    return [("S", b), ("AX", g.spark(12, 10.5, 5))]


@icon("ai-build", "Ai build")
def ai_build(g):
    return model(g) + [("A", g.spark(17.5, 6.5, 4.5))]


@icon("spark-why", "Spark why")
def spark_why(g):
    return [("S", g.circ(2.75, 2.75, 16.25, 16.25)),
            ("L", g.p(("M", 14.25, 14.25), ("L", 20.75, 20.75))),
            ("AX", g.spark(9.5, 9.5, 4.25))]


@icon("check", "Check")
def model_check(g):
    return model(g) + [("L", g.poly([(14.25, 5.75), (16.75, 8.25), (21.25, 3.75)]))]


# Control glyphs (replace the chevrons / sort arrows drawn in widget code); centred, keyline-free.
@icon("chevron-down", "Chevron down")
def chevron_down(g):
    return [chevron(g, 12, 15.25, "d", 5.5)]


@icon("chevron-up", "Chevron up")
def chevron_up(g):
    return [chevron(g, 12, 8.75, "u", 5.5)]


@icon("chevron-left", "Chevron left")
def chevron_left(g):
    return [chevron(g, 8.75, 12, "l", 5.5)]


@icon("chevron-right", "Chevron right")
def chevron_right(g):
    return [chevron(g, 15.25, 12, "r", 5.5)]


@icon("sort-asc", "Sort asc")
def sort_asc(g):
    return [("L", g.p(("M", 11.75, 20.25), ("V", 4.75))), chevron(g, 11.75, 4.75, "u", 5.5)]


@icon("sort-desc", "Sort desc")
def sort_desc(g):
    return [("L", g.p(("M", 11.75, 3.75), ("V", 19.25))), chevron(g, 11.75, 19.25, "d", 5.5)]


@icon("check-mark", "Check mark")
def check_mark(g):
    return [("L", g.poly([(5.25, 12.5), (10, 17.25), (18.75, 7.25)]))]
