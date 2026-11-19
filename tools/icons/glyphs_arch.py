"""Building elements, structure, site and their tools (walls, floors, roofs, grids, levels)."""
import glyphs_base as B
from core import (V, badge, badged, chevron, cube, fmt, g_poly, icon, ring)


# ---------------------------------------------------------------- local helpers

def hl(g, y, x0, x1, role="L"):
    return (role, g.p(("M", x0, y), ("H", x1)))


def vl(g, x, y0, y1, role="L"):
    return (role, g.p(("M", x, y0), ("V", y1)))


def seg(g, a, b, role="L"):
    return (role, g.poly([a, b]))


def dots(g, pts, r=1.25):
    return [("K", ring(g, x, y, r)) for x, y in pts]


def face_badge(g):
    """'By face': a solid diamond in the modifier zone."""
    return [("bK", g.poly([(18.5, 3), (21, 5.5), (18.5, 8), (16, 5.5)], True))]


def with_face(base):
    def fn(g):
        return base(g.sub(2, 5, 19, 22)) + face_badge(g)
    return fn


def dbox(g, top, h):
    """Dimetric box from its top face (4 points, 24 grid) and height: hull + top face + front edge."""
    t = [(g.sx(x), g.sy(y)) for x, y in top]
    b = [(g.sx(x), g.sy(y + h)) for x, y in top]
    pts = t + b
    pts = sorted(set(pts))

    def cross(o, a, c):
        return (a[0] - o[0]) * (c[1] - o[1]) - (a[1] - o[1]) * (c[0] - o[0])
    lo, hi = [], []
    for p in pts:
        while len(lo) >= 2 and cross(lo[-2], lo[-1], p) <= 0:
            lo.pop()
        lo.append(p)
    for p in reversed(pts):
        while len(hi) >= 2 and cross(hi[-2], hi[-1], p) <= 0:
            hi.pop()
        hi.append(p)
    hull = lo[:-1] + hi[:-1]
    fi = max(range(4), key=lambda i: t[i][1])
    f, fb = t[fi], b[fi]
    det = (g_poly(t)[:-1] + "Z" + f"M{fmt(f[0])} {fmt(f[1])}V{fmt(fb[1])}")
    return [("FD", g_poly(t)), ("SF", g_poly(hull)), ("D", det)]


def terrain_top():
    return [(2.75, 13.25), (7.75, 10.75), (12.75, 13.25), (17.75, 8.25), (21.25, 11.75)]


def terrain(g):
    pts = terrain_top() + [(21.25, 20.25), (2.75, 20.25)]
    return [("S", g.poly(pts, True))]


def band(g, x0, y0, x1, y1, t=3):
    """Roof slope band between (x0, y0) and (x1, y1), thickness t downwards."""
    return ("S", g.poly([(x0, y0), (x1, y1), (x1, y1 + t), (x0, y0 + t)], True))


def ground(g, y=20.25):
    return hl(g, y, 2.75, 21.25)


# ---------------------------------------------------------------- walls

@icon("wall-3d", "Wall 3d")
def wall_3d(g):
    return dbox(g, [(3.75, 6.5), (15.75, 12.5), (19.25, 10.75), (7.25, 4.75)], 8)


@icon("wall-attach", "Wall attach")
def wall_attach(g):
    return [seg(g, (2.75, 12.25), (21.25, 3)),
            ("S", g.poly([(6.75, 20.25), (6.75, 10.25), (17.25, 5), (17.25, 20.25)], True)),
            vl(g, 11.75, 19.25, 11.75, "D"), ("D", g.poly([(9.75, 13.75), (11.75, 11.75),
                                                          (13.75, 13.75)]))]


@icon("wall-detach", "Wall detach")
def wall_detach(g):
    return [seg(g, (2.75, 10.25), (21.25, 1.5 + 1.5)),
            ("S", g.rect(6.75, 13.75, 17.25, 20.25)),
            ("L", g.poly([(9.75, 9.75), (11.75, 11.75), (13.75, 9.75)])),
            vl(g, 11.75, 7.75, 11.75)]


def wall_low(g):
    """Wall corner turned to the bottom left: the top-right zone stays free for a badge."""
    out = [("S", g.poly([(3.75, 9.75), (9.25, 9.75), (9.25, 14.75), (20.25, 14.75), (20.25, 20.25),
                         (3.75, 20.25)], True))]
    for x in (3.75, 8.25, 12.75):
        out.append(("H", g.poly([(x, 20.25), (x + 5.5, 14.75)])))
    out.append(("H", g.poly([(3.75, 15.25), (9.25, 9.75)])))
    return out


icon("wall-edit", "Wall edit")(lambda g: wall_low(g) + badge(g, "pencil"))
icon("wall-pick", "Wall pick")(badged(B.wall, "cursor"))
@icon("wall-face", "Wall face")
def wall_face(g):
    return dbox(g, [(3.75, 9.5), (13.75, 14.5), (16.25, 13.25), (6.25, 8.25)], 6) + face_badge(g)


@icon("wall-profile", "Wall profile")
def wall_profile(g):
    return [("L", g.poly([(3.75, 20.25), (3.75, 9.75), (9.75, 9.75), (9.75, 4.75), (20.25, 4.75),
                          (20.25, 20.25)], True))] + dots(g, [(9.75, 9.75), (9.75, 4.75)], 1.5)


@icon("wall-room", "Wall room")
def wall_room(g):
    outer = g.poly([(3.75, 3.75), (13.25, 3.75), (13.25, 9.75), (20.25, 9.75), (20.25, 20.25),
                    (3.75, 20.25)], True)
    inner = g.poly([(6.75, 6.75), (6.75, 17.25), (17.25, 17.25), (17.25, 12.75), (10.25, 12.75),
                    (10.25, 6.75)], True)
    return [("S", outer + inner)]


@icon("wall-bearing", "Wall bearing")
def wall_bearing(g):
    out = [("S", g.rect(8.75, 2.75, 15.25, 21.25))]
    for y in (8.75, 14.75, 20.75):
        out.append(("H", g.poly([(8.75, y), (15.25, y - 6.5)])))
    return out


def wall_side(g, pts):
    return ("S", g.poly(pts, True))


@icon("wall-cornice", "Wall cornice")
def wall_cornice(g):
    return [wall_side(g, [(3.75, 2.75), (16.25, 2.75), (16.25, 5.75), (10.25, 11.75),
                          (10.25, 21.25), (3.75, 21.25)])]


@icon("wall-reveal", "Wall reveal")
def wall_reveal(g):
    return [wall_side(g, [(3.75, 2.75), (10.25, 2.75), (10.25, 9.75), (7.25, 9.75), (7.25, 14.25),
                          (10.25, 14.25), (10.25, 21.25), (3.75, 21.25)]),
            hl(g, 9.75, 13.75, 21.25), hl(g, 14.25, 13.75, 21.25)]


@icon("wall-sweep", "Wall sweep")
def wall_sweep(g):
    return [wall_side(g, [(3.75, 2.75), (10.25, 2.75), (10.25, 21.25), (3.75, 21.25)]),
            ("F", g.p(("M", 10.25, 8.75), ("A", 3.5, 0, 1, 10.25, 15.75), ("Z",))),
            ("L", g.p(("M", 10.25, 8.75), ("A", 3.5, 0, 1, 10.25, 15.75))),
            hl(g, 8.75, 16.75, 21.25), hl(g, 15.75, 16.75, 21.25)]


# ---------------------------------------------------------------- floors, slabs, ceilings

icon("floor-3d", "Floor 3d")(badged(B.floor, "cursor"))


@icon("floor-shape", "Floor shape")
def floor_shape(g):
    return [("S", g.poly([(2.75, 15.25), (11.75, 10.75), (20.75, 15.25), (11.75, 19.75)], True)),
            ("L", g.poly([(2.75, 15.25), (11.75, 6.25), (20.75, 15.25)])),
            ("L", g.p(("M", 11.75, 10.75), ("V", 6.25)))] + dots(g, [(11.75, 5.75)], 1.75)


def l_plan(g):
    return [("S", g.poly([(3.75, 3.75), (12.25, 3.75), (12.25, 10.75), (20.25, 10.75),
                          (20.25, 20.25), (3.75, 20.25)], True))]


icon("floor-sketch", "Floor sketch")(badged(l_plan, "pencil"))


def slab_section(g):
    out = [("S", g.rect(2.75, 5.75, 21.25, 11.25)),
           ("S", g.rect(5.75, 11.25, 9.25, 20.25)), ("S", g.rect(14.75, 11.25, 18.25, 20.25))]
    for x in (4.75, 10.25, 15.75):
        out.append(("H", g.poly([(x, 11.25), (x + 5.5, 5.75)])))
    return out


icon("slab", "Slab")(slab_section)


@icon("slab-edge", "Slab edge")
def slab_edge(g):
    out = [("S", g.poly([(2.75, 7.75), (20.25, 7.75), (20.25, 18.25), (15.75, 18.25),
                         (15.75, 12.25), (2.75, 12.25)], True))]
    out.append(("L", g.p(("M", 15.75, 7.75), ("V", 12.25))))
    for x in (4.75, 9.25):
        out.append(("H", g.poly([(x, 12.25), (x + 4.5, 7.75)])))
    return out


def slab_low(g):
    out = [("S", g.rect(2.75, 11.75, 21.25, 16.25)),
           ("S", g.rect(5.75, 16.25, 9.25, 20.25)), ("S", g.rect(14.75, 16.25, 18.25, 20.25))]
    for x in (4.75, 10.25, 15.75):
        out.append(("H", g.poly([(x, 16.25), (x + 4.5, 11.75)])))
    return out


icon("slab-face", "Slab face")(lambda g: slab_low(g) + face_badge(g))


@icon("slab-opening", "Slab opening")
def slab_opening(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 20.25)),
            ("D", g.rect(8.75, 8.75, 15.25, 15.25)),
            ("D", g.poly([(8.75, 8.75), (15.25, 15.25)])),
            ("D", g.poly([(15.25, 8.75), (8.75, 15.25)]))]


@icon("ceiling", "Ceiling")
def ceiling(g):
    return [hl(g, 3.75, 2.75, 21.25), vl(g, 6.75, 3.75, 10.75), vl(g, 17.25, 3.75, 10.75),
            ("S", g.rect(2.75, 10.75, 21.25, 14.25)),
            ("L", g.p(("M", 2.75, 20.25), ("H", 21.25)))]


@icon("ceiling-opening", "Ceiling opening")
def ceiling_opening(g):
    return [hl(g, 3.75, 2.75, 21.25),
            ("S", g.rect(2.75, 10.75, 8.75, 14.25)), ("S", g.rect(15.25, 10.75, 21.25, 14.25)),
            ("L", g.p(("M", 11.75, 20.25), ("V", 6.25))), chevron(g, 11.75, 6.25, "u", 3)]


@icon("opening", "Opening")
def opening(g):
    out = [("S", g.rect(2.75, 8.75, 8.25, 15.25)), ("S", g.rect(15.75, 8.75, 21.25, 15.25))]
    for x0, x1 in ((8.25, 9.75), (11.25, 12.75), (14.25, 15.75)):
        out.append(("L", g.poly([(x0, 8.75), (x1, 8.75)])))
        out.append(("L", g.poly([(x0, 15.25), (x1, 15.25)])))
    out += [("L", g.poly([(11.75, 2.75), (11.75, 5.75)])), chevron(g, 11.75, 2.75, "u", 2),
            ("L", g.poly([(11.75, 21.25), (11.75, 18.25)])), chevron(g, 11.75, 21.25, "d", 2)]
    return out


@icon("shaft", "Shaft")
def shaft(g):
    out = []
    for y in (5.75, 11.75, 17.75):
        out += [hl(g, y, 2.75, 7.75), hl(g, y, 16.25, 21.25)]
    out += [vl(g, 9.75, 2.75, 21.25, "L"), vl(g, 14.25, 2.75, 21.25, "L")]
    out.append(("F", g.rect(9.75, 2.75, 14.25, 21.25)))
    return out


# ---------------------------------------------------------------- roofs (elevation unless noted)

@icon("roof-gable", "Roof gable")
def roof_gable(g):
    return [("S", g.poly([(2.75, 13.25), (12, 4), (21.25, 13.25)], True)),
            ("L", g.p(("M", 4.75, 13.25), ("V", 20.25), ("H", 19.25), ("V", 13.25)))]


@icon("roof-hip", "Roof hip")
def roof_hip(g):
    return [("S", g.rect(2.75, 5.75, 21.25, 18.25)),
            ("D", g.p(("M", 8.75, 11.75), ("H", 15.25))),
            ("D", g.poly([(2.75, 5.75), (8.75, 11.75), (2.75, 18.25)])),
            ("D", g.poly([(21.25, 5.75), (15.25, 11.75), (21.25, 18.25)]))]


@icon("roof-shed", "Roof shed")
def roof_shed(g):
    return [band(g, 2.75, 12.25, 21.25, 3, 3),
            vl(g, 4.75, 16, 20.25), vl(g, 19.25, 7, 20.25), ground(g)]


@icon("roof-gambrel", "Roof gambrel")
def roof_gambrel(g):
    return [("S", g.poly([(2.75, 19.25), (5.75, 13.25), (12, 7), (18.25, 13.25), (21.25, 19.25)],
                         True)),
            ("D", g.p(("M", 5.75, 13.25), ("H", 18.25)))]


@icon("roof-mansard", "Roof mansard")
def roof_mansard(g):
    return [("S", g.poly([(2.75, 19.25), (6.75, 11.25), (17.25, 11.25), (21.25, 19.25)], True)),
            ("L", g.poly([(6.75, 11.25), (9.75, 5.75), (14.25, 5.75), (17.25, 11.25)]))]


@icon("roof-barrel", "Roof barrel")
def roof_barrel(g):
    return [("S", g.p(("M", 2.75, 20.25), ("V", 12.75), ("A", 9.25, 0, 1, 21.25, 12.75),
                      ("V", 20.25), ("Z",))),
            ("D", g.p(("M", 2.75, 12.75), ("H", 21.25)))]


@icon("roof-extrude", "Roof extrude")
def roof_extrude(g):
    return [("S", g.poly([(2.75, 17.25), (7.25, 12.75), (11.75, 17.25)], True)),
            ("L", g.poly([(7.25, 12.75), (16.75, 8)])), ("L", g.poly([(11.75, 17.25), (21.25, 12.5)])),
            ("L", g.poly([(16.75, 8), (21.25, 12.5)])), ("L", g.poly([(2.75, 17.25), (2.75, 20.25)])),
            ("L", g.poly([(11.75, 17.25), (11.75, 20.25)]))]


icon("roof-face", "Roof face")(with_face(B.roof))


@icon("roof-edges", "Roof edges")
def roof_edges(g):
    return [band(g, 7.25, 12.75, 21.25, 5.75, 3),
            ("S", g.rect(2.75, 12.75, 7.25, 18.25)),
            ("H", g.poly([(2.75, 18.25), (7.25, 13.75)]))]


@icon("roof-join", "Roof join")
def roof_join(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 11.25)),
            ("S", g.p(("M", 7.75, 11.25), ("V", 20.25), ("H", 15.75), ("V", 11.25))),
            ("D", g.poly([(7.75, 11.25), (11.75, 7.25), (15.75, 11.25)])),
            ("D", g.p(("M", 11.75, 7.25), ("V", 20.25))), ("D", g.p(("M", 2.75, 7.25), ("H", 21.25)))]


@icon("roof-unjoin", "Roof unjoin")
def roof_unjoin(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 9.25)), ("D", g.p(("M", 2.75, 6.75), ("H", 21.25))),
            ("S", g.rect(7.75, 13.75, 15.75, 20.25)), ("D", g.p(("M", 11.75, 13.75), ("V", 20.25)))]


@icon("roof-taper", "Roof taper")
def roof_taper(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 20.25)),
            ("D", g.p(("M", 17.25, 11.75), ("H", 6.75))),
            ("D", g.poly([(10.25, 8.25), (6.75, 11.75), (10.25, 15.25)]))]


@icon("roof-opening", "Roof opening")
def roof_opening(g):
    return [("S", g.poly([(2.75, 16.25), (12, 7), (21.25, 16.25)], True)),
            ("D", g.rect(9.75, 12.75, 14.25, 16.25)),
            ("L", g.p(("M", 4.75, 16.25), ("V", 20.25))), ("L", g.p(("M", 19.25, 16.25), ("V", 20.25)))]


def roof_band_low(g):
    return band(g, 2.75, 17.25, 21.25, 8, 3)


@icon("dormer", "Dormer")
def dormer(g):
    return [roof_band_low(g),
            ("S", g.poly([(8.75, 14.25), (8.75, 8.75), (11.5, 6), (14.25, 8.75), (14.25, 11.5)], True))]


@icon("skylight", "Skylight")
def skylight(g):
    return [roof_band_low(g),
            ("S", g.poly([(8.75, 14.25), (14.75, 11.25), (14.75, 6.75), (8.75, 9.75)], True)),
            ("D", g.p(("M", 11.75, 12.75), ("V", 8.25)))]


def eave(g):
    return [seg(g, (21.25, 3.75), (5.75, 11.5)), vl(g, 14.75, 7.25, 20.25)]


@icon("fascia", "Fascia")
def fascia(g):
    return eave(g) + [("S", g.rect(3.75, 10.75, 6.25, 17.25))]


@icon("gutter", "Gutter")
def gutter(g):
    return eave(g) + [("S", g.p(("M", 2.75, 12.75), ("V", 14.25), ("A", 2.75, 0, 0, 8.25, 14.25),
                                ("V", 12.75), ("Z",)))]


@icon("soffit", "Soffit")
def soffit(g):
    return eave(g) + [("S", g.rect(4.75, 12.75, 14.75, 17.25))]


# ---------------------------------------------------------------- curtain, railing, ramp, misc

def curtain_g(g):
    return [("S", g.rect(3.75, 2.75, 20.25, 21.25)),
            ("D", g.p(("M", 9.25, 2.75), ("V", 21.25))), ("D", g.p(("M", 14.75, 2.75), ("V", 21.25))),
            ("D", g.p(("M", 3.75, 8.75), ("H", 20.25))), ("D", g.p(("M", 3.75, 14.75), ("H", 20.25)))]


icon("curtain", "Curtain")(curtain_g)
def curtain_low(g):
    return [("S", g.poly([(3.75, 2.75), (12.25, 2.75), (12.25, 11.75), (20.25, 11.75), (20.25, 21.25),
                          (3.75, 21.25)], True)),
            ("D", g.p(("M", 7.75, 2.75), ("V", 21.25))), ("D", g.p(("M", 16.25, 11.75), ("V", 21.25))),
            ("D", g.p(("M", 3.75, 16.75), ("H", 20.25)))]


@icon("curtain-face", "Curtain face")
def curtain_face(g):
    return [("S", g.poly([(2.75, 11.25), (13.25, 6), (13.25, 15.75), (2.75, 21)], True)),
            ("D", g.p(("M", 8.25, 8.5), ("V", 18.25))),
            ("D", g.poly([(2.75, 16.25), (13.25, 11)]))] + face_badge(g)


icon("curtain-edit", "Curtain edit")(lambda g: curtain_low(g) + badge(g, "pencil"))


@icon("railing", "Railing")
def railing(g):
    out = [("S", g.rect(2.75, 3.75, 21.25, 6.25)), hl(g, 16.75, 3.75, 20.25)]
    for x in (3.75, 9.25, 14.75, 20.25):
        out.append(vl(g, x, 6.25, 20.25))
    return out


@icon("ramp", "Ramp")
def ramp(g):
    return [("S", g.poly([(2.75, 20.25), (21.25, 11), (21.25, 20.25)], True)),
            ("L", g.poly([(4.75, 13.25), (15.75, 7.75)])),
            ("L", g.poly([(12.25, 6.25), (15.75, 7.75), (14.25, 11.25)]))]


@icon("component", "Component")
def component(g):
    return [("S", g.rect(3.75, 4.75, 20.25, 19.25, 1)),
            ("D", g.p(("M", 7.75, 19.25), ("V", 9.25), ("H", 16.25), ("V", 19.25)))]


@icon("flip-facing", "Flip facing")
def flip_facing(g):
    return [("S", g.rect(2.75, 9.75, 21.25, 14.25)),
            vl(g, 11.75, 3.75, 7.75), chevron(g, 11.75, 3.75, "u", 2.5),
            vl(g, 11.75, 16.25, 20.25), chevron(g, 11.75, 20.25, "d", 2.5)]


@icon("flip-hand", "Flip hand")
def flip_hand(g):
    return [("S", g.rect(10.75, 2.75, 13.25, 21.25)),
            hl(g, 11.75, 3.75, 7.75), chevron(g, 3.75, 11.75, "l", 2.5),
            hl(g, 11.75, 16.25, 20.25), chevron(g, 20.25, 11.75, "r", 2.5)]


def edges(g):
    out = []
    for y0, y1 in ((5.75, 9.25), (11.75, 15.25)):
        out.append(vl(g, 3.75, y0, y1))
    for x0, x1 in ((3.75, 7.25), (9.75, 13.25), (15.75, 19.25)):
        out.append(hl(g, 19.25, x0, x1))
    out.append(("S", g.rect(8.75, 11.75, 20.25, 16.25)))
    return out


icon("edge-accept", "Edge accept")(lambda g: edges(g) + badge(g, "tick"))
icon("edge-unlink", "Edge unlink")(lambda g: edges(g) + badge(g, "x"))


# ---------------------------------------------------------------- structure

@icon("column-str", "Column str")
def column_str(g):
    return [("S", g.poly([(6.75, 6.75), (17.25, 6.75), (17.25, 9.25), (13.25, 9.25), (13.25, 14.75),
                          (17.25, 14.75), (17.25, 17.25), (6.75, 17.25), (6.75, 14.75), (10.75, 14.75),
                          (10.75, 9.25), (6.75, 9.25)], True)),
            hl(g, 11.75, 2.75, 4.75), hl(g, 11.75, 19.25, 21.25),
            vl(g, 11.75, 2.75, 4.75), vl(g, 11.75, 19.25, 21.25)]


@icon("column-3d", "Column 3d")
def column_3d(g):
    return dbox(g, [(7.75, 5.25), (11.75, 7.25), (15.75, 5.25), (11.75, 3.25)], 14)


@icon("beam-3d", "Beam 3d")
def beam_3d(g):
    return dbox(g, [(2.75, 8.75), (16.75, 15.75), (20.25, 14), (6.25, 7)], 4)


@icon("beam-grid", "Beam grid")
def beam_grid(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 20.25)),
            ("D", g.p(("M", 7.75, 3.75), ("V", 20.25))), ("D", g.p(("M", 11.75, 3.75), ("V", 20.25))),
            ("D", g.p(("M", 15.75, 3.75), ("V", 20.25)))]


def beam_elev(g):
    return [("S", g.rect(2.75, 7.75, 21.25, 12.25)),
            ("K", g.poly([(5, 15.5), (6.75, 12.25), (8.5, 15.5)], True)),
            ("K", g.poly([(15.5, 15.5), (17.25, 12.25), (19, 15.5)], True)),
            hl(g, 18.75, 2.75, 21.25)]


icon("beam-pick", "Beam pick")(badged(beam_elev, "cursor"))


@icon("brace", "Brace")
def brace(g):
    return [("S", g.rect(4.75, 3.75, 19.25, 6.25)),
            vl(g, 4.75, 6.25, 20.25), vl(g, 19.25, 6.25, 20.25),
            seg(g, (5.75, 19.25), (18.25, 6.75)), ground(g, 20.25)]


@icon("truss", "Truss")
def truss(g):
    return [("S", g.poly([(2.75, 15.75), (12, 6.5), (21.25, 15.75)], True)),
            ("D", g.p(("M", 11.75, 7.25), ("V", 15.75))),
            ("D", g.poly([(7.25, 11.25), (11.75, 15.75), (16.75, 11.25)])),
            ("K", g.poly([(2.75, 19.75), (4.5, 16.25), (6.25, 19.75)], True)),
            ("K", g.poly([(17.75, 19.75), (19.5, 16.25), (21.25, 19.75)], True))]


@icon("footing", "Footing")
def footing(g):
    return [("S", g.rect(4.75, 4.75, 19.25, 19.25)),
            ("KX", g.rect(9.75, 9.75, 14.25, 14.25))]


@icon("footing-all", "Footing all")
def footing_all(g):
    out = []
    for x0, x1 in ((3.75, 9.25), (14.75, 20.25)):
        for y0, y1 in ((3.75, 9.25), (14.75, 20.25)):
            out.append(("S", g.rect(x0, y0, x1, y1)))
            out.append(("KX", ring(g, (x0 + x1) / 2, (y0 + y1) / 2, 1.25)))
    return out


@icon("footing-strip", "Footing strip")
def footing_strip(g):
    return [("S", g.rect(9.75, 2.75, 14.25, 14.75)), ("S", g.rect(3.75, 14.75, 20.25, 20.25)),
            ("H", g.poly([(5.75, 20.25), (10.75, 15.25)])), ("H", g.poly([(13.25, 20.25), (18.25, 15.25)]))]


@icon("raft", "Raft")
def raft(g):
    out = [("S", g.rect(2.75, 13.75, 21.25, 20.25)),
           ("S", g.rect(5.75, 3.75, 8.25, 13.75)), ("S", g.rect(15.75, 3.75, 18.25, 13.75))]
    for x in (3.75, 10.25, 16.75):
        out.append(("H", g.poly([(x, 20.25), (x + 4.5, 15.75)])))
    return out


def rebar_g(g):
    return ([("S", g.rect(4.75, 3.75, 19.25, 20.25)),
             ("D", g.rect(7.75, 6.75, 16.25, 17.25, 1.5))]
            + [("KX", ring(g, x, y, 1.25)) for x in (10, 14) for y in (9, 15)])


icon("rebar", "Rebar")(rebar_g)
def rebar_low(g):
    return ([("S", g.rect(2.75, 10.75, 15.25, 21.25))]
            + [("KX", ring(g, x, y, 1.25)) for x in (6.25, 11.75) for y in (14.25, 17.75)])


icon("rebar-delete", "Rebar delete")(lambda g: rebar_low(g) + badge(g, "x"))
icon("rebar-show", "Rebar show")(lambda g: rebar_low(g) + badge(g, "eye"))


@icon("rebar-schedule", "Rebar schedule")
def rebar_schedule(g):
    return [("L", g.p(("M", 4.75, 3.75), ("V", 10.25), ("H", 19.25), ("V", 3.75))),
            hl(g, 14.75, 4.75, 19.25), hl(g, 19.25, 4.75, 19.25),
            ("K", g.dot(2.75, 14.75)), ("K", g.dot(2.75, 19.25))]


def analytical_g(g):
    return ([("L", g.p(("M", 5.75, 19.25), ("V", 5.75), ("H", 18.25), ("V", 19.25))),
             ("L", g.p(("M", 5.75, 12.75), ("H", 18.25))),
             ("L", g.p(("M", 2.75, 20.25), ("H", 8.75))), ("L", g.p(("M", 15.25, 20.25), ("H", 21.25)))]
            + [("K", ring(g, x, y, 1.75)) for x, y in ((5.75, 5.75), (18.25, 5.75),
                                                      (5.75, 12.75), (18.25, 12.75))])


icon("analytical", "Analytical")(analytical_g)
icon("analytical-out", "Analytical out")(badged(analytical_g, "out"))


# ---------------------------------------------------------------- grids and levels

icon("grid-edit", "Grid edit")(badged(B.grid, "pencil"))
icon("grid-pick", "Grid pick")(badged(B.grid, "cursor"))


@icon("grid-set", "Grid set")
def grid_set(g):
    out = []
    for x0, x1, xc in ((2.75, 10.25, 6.5), (13.75, 21.25, 17.5)):
        out.append(("S", g.circ(x0, 3.75, x1, 11.25)))
    out += [vl(g, 6.75, 11.25, 20.25), vl(g, 17.25, 11.25, 20.25), hl(g, 16.25, 2.75, 21.25)]
    return out


@icon("at-grids", "At grids")
def at_grids(g):
    out = [vl(g, 7.75, 2.75, 21.25), vl(g, 16.25, 2.75, 21.25),
           hl(g, 7.75, 2.75, 21.25), hl(g, 16.25, 2.75, 21.25)]
    out += [("K", ring(g, x, y, 1.75)) for x in (7.75, 16.25) for y in (7.75, 16.25)]
    return out


def level_low(g):
    """Level with a large head; the line stops short of the badge zone."""
    return [("S", g.poly([(2.75, 9.75), (13.75, 9.75), (8.25, 15.25)], True)),
            ("L", g.p(("M", 8.25, 9.75), ("V", 4.75), ("H", 12.75))),
            ("L", g.p(("M", 2.75, 15.25), ("H", 21.25))), hl(g, 19.75, 2.75, 21.25)]


def cube_badge(g):
    c, top = cube(g, 15.75, 21.25, 2.25, 3)
    return [("b" + r, d) for r, d in c]


def level_with(kind):
    def fn(g):
        base = level_low(g)
        if kind == "up":
            base = base[:-1]
        return base + (cube_badge(g) if kind == "cube" else badge(g, kind))
    return fn


icon("level-3d", "Level 3d")(level_with("cube"))
icon("level-delete", "Level delete")(level_with("x"))
icon("level-down", "Level down")(level_with("down"))
icon("level-up", "Level up")(level_with("up"))
icon("level-edit", "Level edit")(level_with("pencil"))


# ---------------------------------------------------------------- site and terrain

@icon("toposolid", "Toposolid")
def toposolid(g):
    return terrain(g) + dots(g, [(7.75, 10.75), (12.75, 13.25), (17.75, 8.25)], 1.5)


@icon("edit-points", "Edit points")
def edit_points(g):
    return ([("L", g.poly(terrain_top()))]
            + dots(g, [(7.75, 10.75), (17.75, 8.25)], 1.5) + dots(g, [(12.75, 13.25)], 2)
            + [vl(g, 12.75, 16.75, 20.75), chevron(g, 12.75, 16.75, "u", 2)])


@icon("flatten", "Flatten")
def flatten(g):
    return [("S", g.poly([(2.75, 8.25), (7.25, 12.75), (16.75, 12.75), (21.25, 8.25), (21.25, 20.25),
                          (2.75, 20.25)], True)),
            vl(g, 11.75, 3.75, 9.25), chevron(g, 11.75, 9.25, "d", 2.5)]


@icon("smooth", "Smooth")
def smooth(g):
    return [("S", g.p(("M", 2.75, 12.25), ("Q", 7.25, 4.25, 12, 11.25), ("Q", 16.75, 18.25, 21.25, 8.25),
                      ("V", 20.25), ("H", 2.75), ("Z",)))]


@icon("pad", "Pad")
def pad(g):
    return [("S", g.poly([(2.75, 10.75), (6.75, 8.75), (6.75, 13.75), (17.25, 13.75), (17.25, 7.75),
                          (21.25, 9.75), (21.25, 20.25), (2.75, 20.25)], True)),
            ("H", g.poly([(9.75, 20.25), (14.25, 15.75)]))]


@icon("site", "Site")
def site(g):
    return [("S", g.poly([(2.75, 16.25), (8.75, 13.25), (14.75, 16.25), (21.25, 13), (21.25, 20.25),
                          (2.75, 20.25)], True)),
            ("S", g.circ(12.75, 2.75, 20.25, 10.25)), vl(g, 16.25, 10.25, 14.25)]


def pentagon():
    return [(4.75, 4.75), (13.75, 4.75), (19.25, 10.25), (19.25, 19.25), (4.75, 19.25)]


@icon("boundary", "Boundary")
def boundary(g):
    return [("S", g.poly(pentagon(), True))] + dots(g, pentagon(), 1.75)


@icon("subregion", "Subregion")
def subregion(g):
    return [("L", g.poly(pentagon(), True)), ("S", g.rect(8.75, 10.75, 15.25, 16.25)),
            ("H", g.poly([(8.75, 16.25), (14.25, 10.75)]))]


@icon("contours", "Contours")
def contours(g):
    return [("L", g.rect(2.75, 3.75, 21.25, 20.25, 7)), ("S", g.rect(6.75, 7.75, 17.25, 16.25, 4)),
            ("L", g.rect(10.75, 10.75, 14.25, 13.25, 1))]


# ---------------------------------------------------------------- rooms and outlines

@icon("room-auto", "Room auto")
def room_auto(g):
    return [("F", g.rect(3.75, 3.75, 20.25, 20.25)),
            ("L", g.rect(3.75, 3.75, 20.25, 20.25)), ("L", g.p(("M", 11.75, 3.75), ("V", 20.25))),
            ("D", g.rect(5.75, 9.75, 9.75, 13.25, 1)), ("D", g.rect(13.75, 9.75, 18.25, 13.25, 1))]


@icon("room-sep", "Room sep")
def room_sep(g):
    out = [("S", g.rect(3.75, 3.75, 20.25, 20.25))]
    for y0, y1 in ((6.75, 9.25), (11.25, 13.75), (15.75, 18.25)):
        out.append(("D", g.p(("M", 11.75, y0), ("V", y1))))
    return out


@icon("zone", "Zone")
def zone(g):
    return [("L", g.rect(2.75, 2.75, 21.25, 21.25, 2.5)),
            ("S", g.rect(6.75, 6.75, 12.25, 17.25)), ("S", g.rect(12.25, 6.75, 17.25, 12.25))]


def clean_base(inner):
    def fn(g):
        out = [("S", g.poly([(3.75, 3.75), (11.25, 3.75), (11.25, 11.75), (20.25, 11.75), (20.25, 20.25),
                             (14.25, 20.25), (12.75, 18.75), (11.25, 20.25), (3.75, 20.25)], True))]
        return out + inner(g) + badge(g, "tick")
    return fn


icon("outline-clean", "Outline clean")(clean_base(lambda g: []))
icon("outline-clean-ceiling", "Outline clean ceiling")(clean_base(
    lambda g: [("D", g.p(("M", 3.75, 15.75), ("H", 20.25))),
               ("D", g.p(("M", 8.25, 3.75), ("V", 20.25)))]))
icon("outline-clean-shaft", "Outline clean shaft")(clean_base(
    lambda g: [("D", g.poly([(3.75, 3.75), (11.25, 11.25)])),
               ("D", g.poly([(3.75, 20.25), (11.25, 12.75)]))]))
