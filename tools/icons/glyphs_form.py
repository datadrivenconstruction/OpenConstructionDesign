"""Forms (solid/void/mass), families, work planes, meshes, MEP and model tools."""
import math

from core import *  # noqa: F401,F403
from core import V, arrow, badge, badged, chevron, cube, cursor, fmt, g_poly, icon, magnifier, \
    model, page, ring

# ---------------------------------------------------------------- helpers


def arcpts(cx, cy, rx, ry, a0, a1, n=12):
    """Points on an ellipse arc, angles in degrees (0 = +x, 90 = +y, screen space)."""
    return [(cx + rx * math.cos(math.radians(a0 + (a1 - a0) * i / n)),
             cy + ry * math.sin(math.radians(a0 + (a1 - a0) * i / n))) for i in range(n + 1)]


def snapped(g, pts):
    return [(g.sx(x), g.sy(y)) for x, y in pts]


def pl(g, pts, close=False):
    """Polyline through 24-grid points (edges snap, arc samples stay free)."""
    P = snapped(g, pts)
    return "M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in P) + ("Z" if close else "")


def dashed(g, pts, on=2.0, off=2.5, close=False):
    """Dashed polyline as separate short strokes (a void outline); lengths in grid units."""
    P = snapped(g, pts)
    if close:
        P.append(P[0])
    k = g.k * g.ax[0]
    on, off = on * k, off * k
    segs, cur, draw, left = [], [P[0]], True, on
    for (x0, y0), (x1, y1) in zip(P, P[1:]):
        ln = math.hypot(x1 - x0, y1 - y0)
        t = 0.0
        while ln - t > 1e-6:
            step = min(left, ln - t)
            t += step
            q = (x0 + (x1 - x0) * t / ln, y0 + (y1 - y0) * t / ln)
            if draw:
                cur.append(q)
            left -= step
            if left <= 1e-6:
                if draw and len(cur) > 1:
                    segs.append(cur)
                draw = not draw
                left = on if draw else off
                cur = [q]
    if draw and len(cur) > 1:
        segs.append(cur)
    return "".join("M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in s) for s in segs)


def ell(g, cx, cy, rx, ry):
    x, y = g.sx(cx), g.sy(cy)
    a, b = g.r(rx), g.r(ry)
    return (f"M{fmt(x-a)} {fmt(y)}A{fmt(a)} {fmt(b)} 0 1 1 {fmt(x+a)} {fmt(y)}"
            f"A{fmt(a)} {fmt(b)} 0 1 1 {fmt(x-a)} {fmt(y)}Z")


def rhomb(a, b, yt):
    """2:1 plane (top face of a dimetric box) as 24-grid points."""
    cx, hw = (a + b) / 2, (b - a) / 2
    return [(cx, yt), (b, yt + hw / 2), (cx, yt + hw), (a, yt + hw / 2)]


def hexpts(a, b, yt, hh):
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    return [(cx, yt), (b, ys), (b, ys + hh), (cx, yc + hh), (a, ys + hh), (a, ys)]


# ---------------------------------------------------------------- forms: outline + details

def f_extrude():
    a, b, yt, hh = 2.75, 16.75, 10.75, 3.5
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    out = [hexpts(a, b, yt, hh)]
    det = [[(a, ys), (cx, yc), (b, ys)], [(cx, yc), (cx, yc + hh)]]
    lines = [[(18.75, 18.25), (18.75, 3.75)], [(16.25, 6.25), (18.75, 3.75), (21.25, 6.25)]]
    return out, det, rhomb(a, b, yt), lines


def f_blend():
    top = rhomb(7.75, 15.75, 2.75)
    bot = rhomb(2.75, 20.75, 12.25)
    out = [[top[0], top[1], bot[1], bot[2], bot[3], top[3]]]
    det = [[top[1], top[2], top[3]], [top[2], bot[2]]]
    return out, det, top, []


def f_revolve():
    rx, ry = 7.25, 3.5
    side = ([(4.75, 6.25), (4.75, 17.75)] + arcpts(12, 17.75, rx, ry, 180, 0)[1:-1]
            + [(19.25, 17.75), (19.25, 6.25)] + arcpts(12, 6.25, rx, ry, 0, -180)[1:-1])
    det = [arcpts(12, 6.25, rx, ry, 180, 0)]
    top = arcpts(12, 6.25, rx, ry, 0, 360)
    axis = [[(12, 2.75), (12, 21.25)]]
    return [side], det, top, axis


def f_sweep():
    o = [(3.75, 21.25), (3.75, 12.75)] + arcpts(12.75, 12.75, 9, 9, 180, 270)[1:-1] + \
        [(12.75, 3.75), (21.25, 3.75), (21.25, 8.75), (12.75, 8.75)] + \
        arcpts(12.75, 12.75, 4, 4, 270, 180)[1:-1] + [(8.75, 12.75), (8.75, 21.25)]
    return [o], [], None, []


def f_swept_blend():
    o = [(2.75, 21.25), (2.75, 12.75)] + arcpts(12.75, 12.75, 10, 10, 180, 270)[1:-1] + \
        [(12.75, 2.75), (21.25, 2.75), (21.25, 6.25), (12.75, 6.25)] + \
        arcpts(12.75, 12.75, 6.5, 6.5, 270, 180)[1:-1] + [(6.25, 12.75), (6.25, 13.25),
                                                          (10.25, 21.25)]
    return [o], [], None, []


def f_cube():
    a, b, yt, hh = 3.75, 19.75, 3.75, 9.25
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    return [hexpts(a, b, yt, hh)], [[(a, ys), (cx, yc), (b, ys)], [(cx, yc), (cx, yc + hh)]], \
        rhomb(a, b, yt), []


def solid(form):
    def fn(g):
        out, det, top, extra = form()
        els = [("SF", pl(g, o, True)) for o in out]
        if top:
            els.insert(0, ("FD", pl(g, top, True)))
        els += [("D", pl(g, d)) for d in det]
        for e in extra:
            if e[0][0] == e[-1][0] and len(e) == 2 and form is f_revolve:
                els.append(("L", dashed(g, e, 1.5, 1.5)))
            else:
                els.append(("L", pl(g, e)))
        return els
    return fn


def void(form):
    def fn(g):
        out, det, _top, extra = form()
        els = [("L", dashed(g, o, close=True)) for o in out]
        els += [("L", dashed(g, d)) for d in det]
        for e in extra:
            if form is f_revolve:
                els.append(("L", dashed(g, e, 1.5, 1.5)))
            else:
                els.append(("L", pl(g, e)))
        return els
    return fn


for nm, ru, f in (("extrude", "Extrude", f_extrude), ("blend", "Blend", f_blend),
                  ("revolve", "Revolve", f_revolve), ("sweep", "Sweep", f_sweep),
                  ("swept-blend", "Swept blend", f_swept_blend)):
    icon("solid-" + nm, "Solid: " + ru)(solid(f))
    icon("void-" + nm, "Void: " + ru)(void(f))


@icon("void-form", "Void form")
def void_form(g):
    out, det, _t, _e = f_cube()
    return [("L", dashed(g, o, close=True)) for o in out] + \
        [("L", dashed(g, d)) for d in det] + badge(g, "minus")


# ---------------------------------------------------------------- massing

A_SQ, B_SQ = (3.75, 3.75, 15.25, 15.25), (8.75, 8.75, 20.25, 20.25)


@icon("mass-union", "Mass union")
def mass_union(g):
    return [("S", g.poly([(3.75, 3.75), (15.25, 3.75), (15.25, 8.75), (20.25, 8.75), (20.25, 20.25),
                          (8.75, 20.25), (8.75, 15.25), (3.75, 15.25)], True))]


@icon("mass-intersect", "Mass intersect")
def mass_intersect(g):
    h = [f"M{fmt(g.sx(8.75))} {fmt(g.sy(15.25))}L{fmt(g.sx(15.25))} {fmt(g.sy(8.75))}"]
    return [("F", g.rect(8.75, 8.75, 15.25, 15.25)), ("L", g.rect(*A_SQ)), ("L", g.rect(*B_SQ))] + \
        [("H", d) for d in h]


@icon("mass-subtract", "Mass subtract")
def mass_subtract(g):
    return [("S", g.poly([(3.75, 3.75), (15.25, 3.75), (15.25, 8.75), (8.75, 8.75), (8.75, 15.25),
                          (3.75, 15.25)], True)),
            ("L", dashed(g, [(15.25, 8.75), (20.25, 8.75), (20.25, 20.25), (8.75, 20.25),
                             (8.75, 15.25)], 2.25, 1.75)),
            ("L", dashed(g, [(15.25, 8.75), (15.25, 15.25), (8.75, 15.25)], 2.25, 1.75))]


@icon("mass-floors", "Mass floors")
def mass_floors(g):
    a, b, yt, hh = 4.75, 18.75, 2.75, 11.5
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    els = [("FD", pl(g, rhomb(a, b, yt), True)), ("SF", pl(g, hexpts(a, b, yt, hh), True)),
           ("D", pl(g, [(a, ys), (cx, yc), (b, ys)])), ("D", pl(g, [(cx, yc), (cx, yc + hh)]))]
    for k in (hh / 3, 2 * hh / 3):
        els.append(("D", pl(g, [(a, ys + k), (cx, yc + k), (b, ys + k)])))
    return els


@icon("shell", "Shell")
def shell(g):
    a, b, yt, hh = 2.75, 20.75, 4.75, 7.5
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    inner = [(cx, ys - 2.6), (cx + 5.2, ys), (cx, ys + 2.6), (cx - 5.2, ys)]
    return [("SF", pl(g, hexpts(a, b, yt, hh), True)),
            ("D", pl(g, [(a, ys), (cx, yc), (b, ys)])), ("D", pl(g, [(cx, yc), (cx, yc + hh)])),
            ("D", pl(g, inner, True)),
            ("D", pl(g, [inner[3], (cx - 5.2, ys + 2.5), (cx, ys + 5.1), (cx + 5.2, ys + 2.5),
                         inner[1]]))]


# ---------------------------------------------------------------- families

def fam_cube(g):
    a, b, yt, hh = 2.75, 20.75, 2.75, 9.5
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    return [("FD", pl(g, rhomb(a, b, yt), True)), ("SF", pl(g, hexpts(a, b, yt, hh), True)),
            ("D", pl(g, [(a, ys), (cx, yc), (b, ys)])), ("D", pl(g, [(cx, yc), (cx, yc + hh)]))]


icon("family-new", "Family new")(badged(fam_cube, "plus"))
icon("family-editor", "Family editor")(badged(fam_cube, "pencil"))


@icon("family-flex", "Family flex")
def family_flex(g):
    s = g.sub(5, 2, 19, 16)
    return fam_cube(s) + [("L", g.p(("M", 2.75, 19.25), ("H", 21.25))),
                          chevron(g, 2.75, 19.25, "l", 2), chevron(g, 21.25, 19.25, "r", 2)]


# ---------------------------------------------------------------- planes

@icon("ref-plane", "Ref plane")
def ref_plane(g):
    return [("L", dashed(g, [(6.75, 11.75), (17.25, 11.75)], 2.5, 1.5)),
            ("L", g.p(("M", 2.75, 7.75), ("V", 15.75))), ("L", g.p(("M", 21.25, 7.75), ("V", 15.75))),
            ("L", g.p(("M", 2.75, 11.75), ("H", 5.25))), ("L", g.p(("M", 18.75, 11.75), ("H", 21.25)))]


WP = rhomb(2.75, 20.75, 12.25)


def wp_base(g, grid=False):
    els = [("S", pl(g, WP, True))]
    return els


@icon("workplane", "Workplane")
def workplane(g):
    P = WP
    (t, r, b, l) = P
    mid = lambda p, q, k: (p[0] + (q[0] - p[0]) * k, p[1] + (q[1] - p[1]) * k)  # noqa: E731
    els = [("S", pl(g, P, True))]
    for k in (1 / 3, 2 / 3):
        els.append(("D", pl(g, [mid(l, t, k), mid(b, r, k)])))
        els.append(("D", pl(g, [mid(t, r, k), mid(l, b, k)])))
    return els + [("L", g.p(("M", 11.75, 12.25), ("V", 2.75))), chevron(g, 11.75, 2.75, "u", 2.5)]


@icon("wp-level", "Wp level")
def wp_level(g):
    return wp_base(g) + [("S", g.poly([(2.75, 2.75), (8.75, 2.75), (5.75, 7.25)], True)),
                         ("L", g.p(("M", 5.75, 7.25), ("H", 21.25)))]


@icon("wp-face", "Wp face")
def wp_face(g):
    a, b, yt, hh = 7.75, 15.75, 2.25, 5.5
    cx, hw = (a + b) / 2, (b - a) / 2
    ys, yc = yt + hw / 2, yt + hw
    return wp_base(g) + [("F", pl(g, rhomb(a, b, yt), True)),
                         ("S", pl(g, hexpts(a, b, yt, hh), True)),
                         ("K", pl(g, rhomb(a, b, yt), True)),
                         ("D", pl(g, [(cx, yc), (cx, yc + hh)]))]


@icon("wp-points", "Wp points")
def wp_points(g):
    P = rhomb(4.75, 18.75, 12.25)
    t, r, b, l = P
    return [("L", pl(g, P, True))] + [("K", ring(g, x, y, 1.5)) for x, y in (l, t, r)] + \
        [("L", dashed(g, [l, (11.75, 5), r], 1.75, 1.75)), ("K", ring(g, 11.75, 5, 1.5))]


@icon("wp-ref", "Wp ref")
def wp_ref(g):
    return wp_base(g) + [("L", dashed(g, [(6.75, 5.75), (17.25, 5.75)], 2.5, 1.5)),
                         ("L", g.p(("M", 2.75, 2.75), ("V", 8.75))),
                         ("L", g.p(("M", 21.25, 2.75), ("V", 8.75))),
                         ("L", g.p(("M", 2.75, 5.75), ("H", 5.25))),
                         ("L", g.p(("M", 18.75, 5.75), ("H", 21.25)))]


@icon("sweep-profiles", "Sweep profiles")
def sweep_profiles(g):
    prof = g.p(("M", 3.75, 20.25), ("V", 3.75), ("H", 9.25), ("V", 7.25), ("H", 12.25),
               ("A", None, 0, 1, 20.25, 15.25), ("V", 20.25), ("Z",))
    return [("S", prof), ("D", g.p(("M", 3.75, 15.25), ("H", 20.25)))]


# ---------------------------------------------------------------- meshes

def mesh(g):
    a, b, yt, hh = 2.75, 20.75, 2.75, 9.5
    P = hexpts(a, b, yt, hh)
    c = (11.75, 12)
    els = [("S", pl(g, P, True))]
    for p in P:
        els.append(("D", pl(g, [c, p])))
    return els


icon("mesh-edit", "Mesh edit")(badged(mesh, "pencil"))
icon("mesh-convert", "Mesh convert")(badged(mesh, "refresh"))
icon("mesh-cache", "Mesh cache")(badged(mesh, "clock"))


# ---------------------------------------------------------------- MEP

@icon("duct", "Duct")
def duct(g):
    return [("S", g.rect(4.75, 4.75, 19.25, 19.25)),
            ("D", g.poly([(4.75, 4.75), (19.25, 19.25)])), ("D", g.poly([(19.25, 4.75), (4.75, 19.25)]))]


@icon("pipe", "Pipe")
def pipe(g):
    return [("S", g.p(("M", 2.75, 6.75), ("H", 11.75), ("A", None, 0, 1, 21.25, 16.25), ("V", 21.25),
                      ("H", 15.75), ("V", 16.25), ("A", None, 0, 0, 11.75, 12.25), ("H", 2.75),
                      ("Z",)))]


@icon("conduit", "Conduit")
def conduit(g):
    return [("L", g.p(("M", 7.25, 7.75), ("H", 11.75), ("A", None, 0, 1, 17.75, 13.75), ("V", 16.75))),
            ("L", ring(g, 5, 7.75, 2.25)), ("L", ring(g, 17.75, 19, 2.25))]


@icon("tray", "Tray")
def tray(g):
    return [("L", g.p(("M", 2.75, 6.75), ("H", 21.25))), ("L", g.p(("M", 2.75, 17.25), ("H", 21.25))),
            ("L", g.p(("M", 6.75, 6.75), ("V", 17.25))), ("L", g.p(("M", 11.75, 6.75), ("V", 17.25))),
            ("L", g.p(("M", 16.75, 6.75), ("V", 17.25)))]


@icon("fitting", "Fitting")
def fitting(g):
    return [("S", g.poly([(2.75, 4.75), (21.25, 4.75), (21.25, 11.25), (15.25, 11.25), (15.25, 21.25),
                          (8.75, 21.25), (8.75, 11.25), (2.75, 11.25)], True)),
            ("D", g.p(("M", 6.75, 4.75), ("V", 11.25))), ("D", g.p(("M", 17.25, 4.75), ("V", 11.25))),
            ("D", g.p(("M", 8.75, 17.25), ("H", 15.25)))]


@icon("fixture", "Fixture")
def fixture(g):
    x0, x1 = g.sx(6.75), g.sx(17.25)
    y0, y1 = g.sy(8.25), g.sy(12.75)
    rx, ry = (x1 - x0) / 2, g.r(8.5)
    bowl = (f"M{fmt(x0)} {fmt(y0)}V{fmt(y1)}A{fmt(rx)} {fmt(ry)} 0 0 0 {fmt(x1)} {fmt(y1)}"
            f"V{fmt(y0)}Z")
    return [("S", g.rect(4.75, 2.75, 19.25, 8.25, 1)), ("S", bowl),
            ("D", ell(g, 12, 15.5, 2.5, 3))]


@icon("equipment", "Equipment")
def equipment(g):
    return [("S", g.rect(2.75, 4.75, 21.25, 19.25, 1)),
            ("D", g.circ(5.75, 7.75, 14.25, 16.25)),
            ("D", g.p(("M", 16.75, 8.75), ("H", 18.25))), ("D", g.p(("M", 16.75, 11.75), ("H", 18.25))),
            ("D", g.p(("M", 16.75, 14.75), ("H", 18.25)))]


@icon("light", "Light")
def light(g):
    # fixture symbol: a lamp disc crossed by an X that runs past it
    return [("S", g.circ(6.75, 6.75, 17.25, 17.25)),
            ("L", g.poly([(4.25, 4.25), (19.75, 19.75)])),
            ("L", g.poly([(19.75, 4.25), (4.25, 19.75)]))]


@icon("device", "Device")
def device(g):
    return [("S", g.circ(7.75, 4.75, 21.25, 18.25)),
            ("L", g.p(("M", 2.75, 9.75), ("H", 7.75))), ("L", g.p(("M", 2.75, 13.25), ("H", 7.75))),
            ("D", g.p(("M", 14.25, 8.75), ("V", 14.25)))]


@icon("air-terminal", "Air terminal")
def air_terminal(g):
    return [("S", g.rect(3.75, 3.75, 20.25, 20.25)), ("D", g.rect(9.25, 9.25, 14.75, 14.75)),
            ("D", g.poly([(3.75, 3.75), (9.25, 9.25)])), ("D", g.poly([(20.25, 3.75), (14.75, 9.25)])),
            ("D", g.poly([(3.75, 20.25), (9.25, 14.75)])), ("D", g.poly([(20.25, 20.25), (14.75, 14.75)]))]


@icon("route", "Route")
def route(g):
    return [("S", g.circ(2.75, 16.75, 7.25, 21.25)),
            ("L", g.p(("M", 4.75, 16.75), ("V", 9.75), ("H", 12.25), ("V", 14.25), ("H", 19.25),
                      ("V", 7.25))),
            ("K", g.poly([(16.75, 7.25), (19.25, 2.75), (21.25, 7.25)], True))]


def terminals(g):
    return [("S", g.rect(2.75, 8.25, 7.25, 15.75, 1)), ("S", g.rect(16.75, 8.25, 21.25, 15.75, 1))]


@icon("connect", "Connect")
def connect(g):
    return terminals(g) + [("L", g.p(("M", 7.25, 11.75), ("H", 16.75)))]


@icon("disconnect", "Disconnect")
def disconnect(g):
    return terminals(g) + [("L", g.p(("M", 7.25, 11.75), ("H", 9.75))),
                           ("L", g.p(("M", 14.25, 11.75), ("H", 16.75))),
                           ("L", g.poly([(10, 16), (14, 8)]))]


def system_tree(g):
    return [("S", g.rect(8.75, 2.75, 15.25, 8.25, 1)),
            ("L", g.p(("M", 11.75, 8.25), ("V", 12.75))),
            ("L", g.p(("M", 5.75, 15.75), ("V", 12.75), ("H", 18.25), ("V", 15.75))),
            ("S", g.circ(2.75, 15.75, 8.25, 21.25)), ("S", g.circ(15.75, 15.75, 21.25, 21.25))]


icon("system", "System")(system_tree)
icon("system-check", "System check")(badged(system_tree, "tick"))


@icon("system-browser", "System browser")
def system_browser(g):
    return [("S", g.rect(2.75, 2.75, 7.25, 7.25)),
            ("L", g.p(("M", 4.75, 7.25), ("V", 17.75), ("H", 8.75))),
            ("L", g.p(("M", 4.75, 11.75), ("H", 8.75))),
            ("S", g.circ(8.75, 9.75, 12.75, 13.75)), ("S", g.circ(8.75, 15.75, 12.75, 19.75)),
            ("L", g.p(("M", 10.75, 4.75), ("H", 21.25))), ("L", g.p(("M", 15.75, 11.75), ("H", 21.25))),
            ("L", g.p(("M", 15.75, 17.75), ("H", 21.25)))]


# ---------------------------------------------------------------- model tools

@icon("paint", "Paint")
def paint(g):
    return [("S", g.rect(3.75, 3.75, 17.25, 8.25, 1)),
            ("L", g.p(("M", 17.25, 6.25), ("H", 20.25), ("V", 11.75), ("H", 11.75), ("V", 14.75))),
            ("S", g.rect(10.25, 14.75, 13.25, 21.25, 1))]


@icon("clash", "Clash")
def clash(g):
    h = [g.poly([(8.75, 15.25), (15.25, 8.75)])]
    return [("F", g.rect(8.75, 8.75, 15.25, 15.25)),
            ("L", g.rect(2.75, 8.75, 21.25, 15.25)), ("L", g.rect(8.75, 2.75, 15.25, 21.25))] + \
        [("H", d) for d in h]


@icon("clash-report", "Clash report")
def clash_report(g):
    return page(g) + [("D", g.circ(7.75, 10.75, 13.25, 16.25)), ("D", g.circ(10.75, 10.75, 16.25, 16.25))]


@icon("classify", "Classify")
def classify(g):
    return [("S", g.p(("M", 3.75, 3.75), ("H", 11.75), ("L", 20.25, 12.25), ("L", 12.25, 20.25),
                      ("L", 3.75, 11.75), ("Z",))),
            ("D", ring(g, 8, 8, 1.4))]


@icon("purge", "Purge")
def purge(g):
    u = (-1 / math.sqrt(2), 1 / math.sqrt(2))
    v = (1 / math.sqrt(2), 1 / math.sqrt(2))
    o = (13.5, 10.5)
    P = lambda s, t: (o[0] + s * u[0] + t * v[0], o[1] + s * u[1] + t * v[1])  # noqa: E731
    head = [P(0, -2.5), P(0, 2.5), P(9.5, 5), P(9.5, -5)]
    return [("L", g.poly([(20.75, 3.25), P(0, 0)])), ("S", g.poly(head, True)),
            ("D", g.poly([P(4, -1.6), P(9.5, -2.4)])), ("D", g.poly([P(4, 1.6), P(9.5, 2.4)]))]


@icon("perf-stats", "Perf stats")
def perf_stats(g):
    return [("L", g.p(("M", 3.75, 2.75), ("V", 20.25), ("H", 21.25))),
            ("S", g.rect(7.75, 12.75, 10.25, 20.25)), ("S", g.rect(12.75, 5.75, 15.25, 20.25)),
            ("S", g.rect(17.75, 9.75, 20.25, 20.25))]


@icon("record", "Record")
def record(g):
    return [("L", g.circ(2.75, 2.75, 21.25, 21.25)), ("KX", ring(g, 12, 12, 4.5)),
            ("F", g.circ(2.75, 2.75, 21.25, 21.25))]


@icon("script", "Script")
def script(g):
    return [("S", g.rect(2.75, 3.75, 9.25, 9.25, 1)), ("S", g.rect(14.75, 14.75, 21.25, 20.25, 1)),
            ("L", g.p(("M", 9.25, 6.75), ("H", 11.75), ("V", 17.25), ("H", 14.75))),
            ("S", g.rect(14.75, 3.75, 21.25, 9.25, 1)), ("L", g.p(("M", 11.75, 6.75), ("H", 14.75)))]


@icon("goto", "Goto")
def goto(g):
    return [("S", g.rect(9.75, 9.75, 21.25, 21.25, 1))] + arrow(g, 2.75, 2.75, 15.25, 15.25, 4.5)


@icon("query", "Query")
def query(g):
    rx, ry = 6.25, 2.75
    x0, x1 = g.sx(2.75), g.sx(15.25)
    yt, yb = g.sy(5.5), g.sy(11.5)
    a, b = (x1 - x0) / 2, g.r(ry)
    cx = (x0 + x1) / 2
    body = (f"M{fmt(x0)} {fmt(yt)}V{fmt(yb)}A{fmt(a)} {fmt(b)} 0 0 0 {fmt(x1)} {fmt(yb)}V{fmt(yt)}"
            f"A{fmt(a)} {fmt(b)} 0 0 0 {fmt(x0)} {fmt(yt)}Z")
    mid = f"M{fmt(x0)} {fmt(g.sy(8.5))}A{fmt(a)} {fmt(b)} 0 0 0 {fmt(x1)} {fmt(g.sy(8.5))}"
    front = f"M{fmt(x0)} {fmt(yt)}A{fmt(a)} {fmt(b)} 0 0 0 {fmt(x1)} {fmt(yt)}"
    return [("S", body), ("D", mid), ("D", front),
            ("bS", g.circ(12.75, 12.75, 19.25, 19.25)), ("bL", g.poly([(18.3, 18.3), (21.25, 21.25)]))]


@icon("find", "Find")
def find(g):
    return magnifier(g) + [("D", g.p(("M", 7.25, 8.75), ("H", 12.75))),
                           ("D", g.p(("M", 7.25, 11.75), ("H", 10.25)))]


@icon("hud", "Hud")
def hud(g):
    return [cursor(g, 2.75, 2.75, 0.5), ("S", g.rect(9.75, 9.75, 21.25, 20.25, 1)),
            ("D", g.p(("M", 12.75, 13.75), ("H", 18.25))), ("D", g.p(("M", 12.75, 16.75), ("H", 16.25)))]


@icon("material", "Material")
def material(g):
    x0, x1, y = g.sx(2.75), g.sx(21.25), g.sy(12)
    eq = f"M{fmt(x0)} {fmt(y)}A{fmt((x1-x0)/2)} {fmt(g.r(3.5))} 0 0 0 {fmt(x1)} {fmt(y)}"
    return [("S", g.circ(2.75, 2.75, 21.25, 21.25)), ("D", eq),
            ("D", g.p(("M", 12, 2.75), ("A", 4.5 + 0.0, 0, 0, 12, 21.25)))]


@icon("object-styles", "Object styles")
def object_styles(g):
    dots = "".join(g.dot(x, 18.25) for x in (3.75, 7.25, 10.75, 14.25, 17.75, 21.25 - 0.0))
    return [("L", g.p(("M", 3.75, 5.75), ("H", 20.25))),
            ("L", dashed(g, [(3.75, 11.75), (20.25, 11.75)], 3.5, 2.25)),
            ("K", dots)]


@icon("phases", "Phases")
def phases(g):
    return [("S", g.rect(2.75, 3.75, 11.25, 8.25, 1)), ("S", g.rect(7.75, 9.75, 16.25, 14.25, 1)),
            ("S", g.rect(12.75, 15.75, 21.25, 20.25, 1))]


@icon("design-options", "Design options")
def design_options(g):
    return [("S", g.circ(2.75, 8.75, 8.25, 14.25)),
            ("L", g.p(("M", 8.25, 11.75), ("H", 11.75))),
            ("L", g.p(("M", 15.75, 5.75), ("H", 11.75), ("V", 17.75), ("H", 15.75))),
            ("S", g.circ(15.75, 2.75, 21.25, 8.25)), ("S", g.circ(15.75, 15.75, 21.25, 21.25))]


@icon("template", "Template")
def template(g):
    return [("S", g.rect(3.75, 2.75, 20.25, 21.25, 1)),
            ("D", g.p(("M", 3.75, 7.75), ("H", 20.25))), ("D", g.p(("M", 9.75, 7.75), ("V", 21.25))),
            ("D", g.p(("M", 12.75, 11.75), ("H", 17.25))), ("D", g.p(("M", 12.75, 15.25), ("H", 17.25)))]


@icon("layers", "Layers")
def layers(g):
    top = rhomb(2.75, 21.25, 2.75)
    return [("S", pl(g, top, True)),
            ("L", pl(g, [(2.75, 11.875), (12, 16.5), (21.25, 11.875)])),
            ("L", pl(g, [(2.75, 16.375), (12, 21), (21.25, 16.375)]))]


@icon("gridshell", "Gridshell")
def gridshell(g):
    dome = arcpts(12, 19.25, 9.25, 13, 180, 360, 16)
    return [("S", pl(g, dome, True)),
            ("D", pl(g, arcpts(12, 19.25, 4.5, 13, 180, 360, 12))),
            ("D", g.p(("M", 11.75, 6.25), ("V", 19.25))),
            ("D", g.p(("M", 4.25, 12.75), ("H", 19.75))),
            ("K", ring(g, 12, 12.75, 1.25))]


def sheet_edge(x0, y0, x1, y1, amp, n=12):
    """Curved edge of a freeform sheet: chord x0,y0 -> x1,y1 bowed up by `amp`."""
    return [(x0 + (x1 - x0) * i / n, y0 + (y1 - y0) * i / n - amp * math.sin(math.pi * i / n))
            for i in range(n + 1)]


def sheet(g, top=(2.75, 9.75, 21.25, 7.75), drop=8.0, amp=3.0):
    x0, y0, x1, y1 = top
    up = sheet_edge(x0, y0, x1, y1, amp)
    down = sheet_edge(x0, y0 + drop, x1, y1 + drop, amp)
    return up, down, pl(g, up + down[::-1], True)


@icon("freeform", "Freeform")
def freeform(g):
    up, down, outline = sheet(g)
    mid = [((a[0] + b[0]) / 2, (a[1] + b[1]) / 2) for a, b in zip(up, down)]
    return [("S", outline), ("D", pl(g, mid)),
            ("K", ring(g, up[6][0], up[6][1], 1.25))]


@icon("panelize", "Panelize")
def panelize(g):
    up, down, outline = sheet(g)
    out = [("S", outline)]
    x0, y0, x1, y1, amp, drop = 2.75, 9.75, 21.25, 7.75, 3.0, 8.0
    for x in (8.75, 15.75):
        t = (x - x0) / (x1 - x0)
        y = y0 + (y1 - y0) * t - amp * math.sin(math.pi * t)
        out.append(("D", g.p(("M", x, y), ("V", y + drop))))
    mid = [((a[0] + b[0]) / 2, (a[1] + b[1]) / 2) for a, b in zip(up, down)]
    out.append(("D", pl(g, mid)))
    return out


@icon("surface-shell", "Surface shell")
def surface_shell(g):
    up, down, outline = sheet(g, top=(2.75, 8.75, 21.25, 6.75), drop=7.0)
    under = [(x, y + 3.5) for x, y in down]
    return [("S", outline), ("L", pl(g, under)),
            ("L", pl(g, [down[0], under[0]])), ("L", pl(g, [down[-1], under[-1]]))]


@icon("thicken", "Thicken")
def thicken(g):
    up, down, outline = sheet(g, top=(2.75, 13.75, 21.25, 11.75), drop=7.0, amp=2.5)
    return [("S", outline)] + arrow(g, 11.75, 9.25, 11.75, 2.75)
