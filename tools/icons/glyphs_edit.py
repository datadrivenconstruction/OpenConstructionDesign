"""Modify, selection, groups, visibility, views and display styles."""
import math

import glyphs_base as B
from core import *  # noqa: F401,F403
from core import V, arrow, badge, badged, chevron, cube, cursor, eye, fmt, g_poly, icon, magnifier
from core import model, pencil, refresh, ring


# ---------------------------------------------------------------- local motifs

def bdg(els):
    """Elements moved to the badge layer (drawn last, halo knocked out of the base)."""
    return [("b" + r, d) for r, d in els]


def dash_h(g, y, x0, x1, on=2.0, off=2.0, role="L"):
    out, x = [], x0
    while x < x1 - 0.01:
        e = min(x + on, x1)
        out.append((role, g.p(("M", x, y), ("H", e))))
        x = e + off
    return out


def dash_v(g, x, y0, y1, on=2.0, off=2.0, role="L"):
    out, y = [], y0
    while y < y1 - 0.01:
        e = min(y + on, y1)
        out.append((role, g.p(("M", x, y), ("V", e))))
        y = e + off
    return out


def dash_rect(g, x0, y0, x1, y1, on=2.0, off=2.0):
    return (dash_h(g, y0, x0, x1, on, off) + dash_h(g, y1, x0, x1, on, off)
            + dash_v(g, x0, y0 + on + off, y1 - on, on, off)
            + dash_v(g, x1, y0 + on + off, y1 - on, on, off))


def corners(g, x0, y0, x1, y1, k=4.5):
    """Four corner brackets of a frame."""
    return [("L", g.p(("M", x0, y0 + k), ("V", y0), ("H", x0 + k))),
            ("L", g.p(("M", x1 - k, y0), ("H", x1), ("V", y0 + k))),
            ("L", g.p(("M", x1, y1 - k), ("V", y1), ("H", x1 - k))),
            ("L", g.p(("M", x0 + k, y1), ("H", x0), ("V", y1 - k)))]


def face(g, cx, cy, hw, role="S"):
    """2:1 dimetric plane (diamond) centred at (cx, cy) with half width hw."""
    return (role, g.poly([(cx, cy - hw / 2), (cx + hw, cy), (cx, cy + hw / 2), (cx - hw, cy)], True))


def big_cube(g):
    """Model cube filling the field: (elements, top face path, key points)."""
    return cube(g, 2.75, 20.75, 2.75, 8.5)


def cube_pts(g):
    a, b = g.sx(2.75), g.sx(20.75)
    cx, hw = (a + b) / 2, (b - a) / 2
    yt = g.sy(2.75)
    hh = round(8.5 * g.k * g.ay[0] * 2) / 2
    ys, yc = yt + hw / 2, yt + hw
    return dict(a=a, b=b, cx=cx, yt=yt, ys=ys, yc=yc, hh=hh)


def sel_cursor(g, x=13.25, y=13.25, s=0.5):
    """Small solid selection arrow at the bottom right (badge layer)."""
    return bdg([cursor(g, x, y, s, "K")])


def slash(g):
    return bdg([("L", g.poly([(4, 4), (20, 20)]))])


def clipboard(g, x0=4.75, y0=3.75, x1=19.25, y1=21.25):
    cx = (x0 + x1) / 2
    return [("S", g.rect(x0, y0, x1, y1, 1)),
            ("D", g.rect(cx - 2.0, y0 - 1.0, cx + 2.0, y0 + 2.0, 1))]


def gauge(g, ang):
    """Performance gauge: half dial, ticks and a needle at `ang` degrees (0 = right, 90 = up)."""
    cx, cy, R = 12, 16.25, 9.25
    out = [("L", g.p(("M", 2.75, 16.25), ("A", R, 0, 1, 21.25, 16.25))),
           ("L", g.p(("M", 2.75, 19.25), ("H", 21.25)))]
    if ang is not None:
        a = math.radians(ang)
        r = 6.5
        out.append(("L", g.poly([(cx - 0.25, cy), (cx - 0.25 + r * math.cos(a), cy - r * math.sin(a))])))
        out.append(("K", ring(g, cx, cy, 1.25)))
    return out


def scissors(g):
    return [("S", g.circ(3.75, 14.75, 9.25, 20.25)), ("S", g.circ(14.75, 14.75, 20.25, 20.25)),
            ("L", g.poly([(8.4, 15.2), (15, 3)])), ("L", g.poly([(15.6, 15.2), (9, 3)])),
            ("K", ring(g, 12, 10.5, 1.5))]


def group_shapes(g):
    return [("S", g.rect(6.75, 6.75, 11.25, 11.25)), ("S", g.circ(12.75, 12.75, 17.25, 17.25))]


def frame_group(g):
    return [("L", g.rect(2.75, 2.75, 21.25, 21.25, 2))] + group_shapes(g)


def group_open(g):
    """The group frame left open at the top right, where a badge sits."""
    return [("L", g.p(("M", 21.25, 12.75), ("V", 21.25), ("H", 2.75), ("V", 2.75), ("H", 12.25)))] \
        + group_shapes(g)


def eye_off(g):
    return eye(g) + slash(g)


def magnet(g):
    return [("S", g.p(("M", 4.75, 3.75), ("V", 12.75), ("A", 7.25, 0, 0, 19.25, 12.75),
                      ("V", 3.75), ("H", 13.75), ("V", 12.75), ("A", 1.75, 0, 1, 10.25, 12.75),
                      ("V", 3.75), ("Z",))),
            ("D", g.p(("M", 4.75, 7.75), ("H", 10.25))), ("D", g.p(("M", 13.75, 7.75), ("H", 19.25)))]


def plan(g):
    return [("L", g.p(("M", 9.75, 20.25), ("H", 3.75), ("V", 3.75), ("H", 20.25), ("V", 20.25),
                      ("H", 14.75))),
            ("L", g.p(("M", 11.75, 3.75), ("V", 12.75), ("H", 20.25))),
            ("L", g.p(("M", 3.75, 12.75), ("H", 6.75)))]


def badge_local(g, kind):
    if kind == "home":
        els = [("L", g.poly([(15.75, 5.25), (18.5, 2.5), (21.25, 5.25)])),
               ("L", g.p(("M", 16.75, 4.75), ("V", 8.25), ("H", 20.25), ("V", 4.75)))]
    elif kind == "back":
        els = [("L", g.p(("M", 16.25, 4.75), ("H", 19.25), ("A", None, 0, 1, 19.25, 8.25),
                         ("H", 17.75))), chevron(g, 16.25, 4.75, "l", 1.75)]
    elif kind == "stack":
        els = [("L", g.rect(15.75, 4.75, 19.25, 8.25)),
               ("L", g.p(("M", 17.75, 2.75), ("H", 21.25), ("V", 6.25)))]
    elif kind == "cube":
        c, _ = cube(g, 15.25, 21.25, 2.25, 3)
        els = [("L", c[0][1])]
    elif kind == "cancel":
        els = [("L", ring(g, 18.5, 5.5, 3)), ("L", g.poly([(16.4, 7.6), (20.6, 3.4)]))]
    else:
        raise KeyError(kind)
    return bdg(els)


LOCAL_BADGES = ("home", "back", "stack", "cube", "cancel")


def _badged(base, kind, box=BASE_BOX):
    """`base` shrunk into `box` plus a badge (local kinds first, then the shared ones)."""
    def fn(g):
        mark = badge_local(g, kind) if kind in LOCAL_BADGES else badge(g, kind)
        return base(g.sub(*box)) + mark
    return fn


local_badged = _badged


def reg(name, ru, fn):
    icon(name, ru)(fn)


# ---------------------------------------------------------------- modify

@icon("move", "Move")
def move(g):
    c = 11.75
    return [("L", g.p(("M", 2.75, c), ("H", 21.25))), ("L", g.p(("M", c, 2.75), ("V", 21.25))),
            chevron(g, 2.75, c, "l"), chevron(g, 21.25, c, "r"),
            chevron(g, c, 2.75, "u"), chevron(g, c, 21.25, "d")]


@icon("copy", "Copy")
def copy(g):
    return [("L", g.p(("M", 8.75, 15.25), ("H", 3.75), ("V", 3.75), ("H", 15.25), ("V", 8.75))),
            ("S", g.rect(8.75, 8.75, 20.25, 20.25, 1))]


@icon("rotate", "Rotate")
def rotate(g):
    return [("L", g.p(("M", 12.75, 4.75), ("A", 8, 1, 1, 4.75, 12.75))),
            chevron(g, 4.75, 12.75, "u", 2.5),
            ("S", g.rect(9.75, 9.75, 15.25, 15.25))]


def _tri(g, x_edge, x_tip, role):
    return (role, g.poly([(x_edge, 6.25), (x_edge, 18.25), (x_tip, 18.25)], True))


@icon("mirror", "Mirror")
def mirror(g):
    return ([_tri(g, 9.25, 3.25, "S"), _tri(g, 14.75, 20.75, "L")]
            + dash_v(g, 11.75, 2.75, 21.25, 2.5, 1.5))


@icon("mirror-draw", "Mirror draw")
def mirror_draw(g):
    return [_tri(g, 9.25, 3.25, "S"), _tri(g, 14.75, 20.75, "L"),
            ("L", g.p(("M", 11.75, 6.25), ("V", 17.75))),
            ("K", ring(g, 11.75, 4.5, 1.5)), ("K", ring(g, 11.75, 19.5, 1.5))]


@icon("trim-extend", "Trim extend")
def trim_extend(g):
    return [("L", g.p(("M", 21.25, 8.75), ("H", 8.75), ("V", 21.25))),
            ("L", g.p(("M", 2.75, 8.75), ("H", 4.75))), ("L", g.p(("M", 8.75, 2.75), ("V", 4.75))),
            ("K", ring(g, 8.75, 8.75, 2))]


@icon("extend", "Extend")
def extend(g):
    return ([("L", g.p(("M", 20.25, 2.75), ("V", 21.25))),
             ("L", g.p(("M", 3.75, 11.75), ("H", 8.75)))]
            + dash_h(g, 11.75, 11.25, 13.75, 2.5, 1)
            + [chevron(g, 17.25, 11.75, "r", 3)]
            + [("L", g.p(("M", 3.75, 7.75), ("V", 15.75)))])


@icon("split", "Split")
def split(g):
    return [("L", g.p(("M", 2.75, 11.75), ("H", 8.75))), ("L", g.p(("M", 15.25, 11.75), ("H", 21.25))),
            ("L", g.poly([(15, 3), (9, 15)])),
            ("L", g.p(("M", 2.75, 8.75), ("V", 14.75))), ("L", g.p(("M", 21.25, 8.75), ("V", 14.75)))]


@icon("divide", "Divide")
def divide(g):
    return [("L", g.p(("M", 2.75, 11.75), ("H", 21.25))),
            ("L", g.p(("M", 2.75, 8.75), ("V", 14.75))), ("L", g.p(("M", 21.25, 8.75), ("V", 14.75))),
            ("L", g.p(("M", 8.75, 6.75), ("V", 16.75))), ("L", g.p(("M", 15.25, 6.75), ("V", 16.75)))]


@icon("align", "Align")
def align(g):
    return [("L", g.p(("M", 3.75, 2.75), ("V", 21.25))),
            ("S", g.rect(6.75, 4.75, 15.25, 9.25, 1)),
            ("L", g.p(("M", 20.25, 16.25), ("H", 9.75))), chevron(g, 6.75, 16.25, "l", 3, "L"),
            ("L", g.p(("M", 20.25, 13.25), ("V", 19.25)))]


@icon("offset", "Offset")
def offset(g):
    return [("L", g.p(("M", 3.75, 21.25), ("V", 3.75), ("H", 21.25))),
            ("S", g.p(("M", 9.75, 21.25), ("V", 9.75), ("H", 21.25))),
            ("L", g.poly([(15.5, 15.5), (19.5, 19.5)]))]


@icon("array", "Array")
def array(g):
    out = [("S", g.rect(2.75, 4.75, 6.25, 13.25, 1)),
           ("L", g.rect(10.25, 4.75, 13.75, 13.25, 1)), ("L", g.rect(17.75, 4.75, 21.25, 13.25, 1)),
           ("L", g.p(("M", 2.75, 18.75), ("H", 21.25)))]
    for x in (2.75, 10.25, 17.75):
        out.append(("L", g.p(("M", x, 16.75), ("V", 20.75))))
    return out


@icon("delete", "Delete")
def delete(g):
    return [("L", g.p(("M", 3.75, 5.75), ("H", 20.25))),
            ("L", g.p(("M", 8.75, 5.75), ("V", 2.75), ("H", 15.25), ("V", 5.75))),
            ("S", g.p(("M", 5.75, 5.75), ("V", 21.25), ("H", 18.25), ("V", 5.75))),
            ("D", g.p(("M", 9.75, 9.75), ("V", 17.25))), ("D", g.p(("M", 14.25, 9.75), ("V", 17.25)))]


@icon("demolish", "Demolish")
def demolish(g):
    return [("S", g.rect(3.75, 7.75, 20.25, 16.25)),
            ("D", g.poly([(3.75, 7.75), (20.25, 16.25)])), ("D", g.poly([(3.75, 16.25), (20.25, 7.75)])),
            ("L", g.p(("M", 2.75, 20.25), ("H", 21.25)))]


@icon("chamfer", "Chamfer")
def chamfer(g):
    return [("L", g.p(("M", 7.75, 21.25), ("V", 13.75), ("L", 13.75, 7.75), ("H", 21.25))),
            ("K", ring(g, 7.75, 13.75, 1.5)), ("K", ring(g, 13.75, 7.75, 1.5))] + _chamfer_ghost(g)


def _chamfer_ghost(g):
    return dash_v(g, 7.75, 2.75, 9.75, 2, 1.5) + dash_h(g, 7.75, 3.75, 10.25, 2, 1.5)


@icon("fillet", "Fillet")
def fillet(g):
    return [("L", g.p(("M", 7.75, 21.25), ("V", 17.75), ("A", 10, 0, 1, 17.75, 7.75), ("H", 21.25))),
            ("K", ring(g, 17.75, 17.75, 1.5))] + _chamfer_ghost(g)


@icon("join", "Join")
def join(g):
    return [("S", g.poly([(2.75, 7.75), (21.25, 7.75), (21.25, 12.25), (14.25, 12.25), (14.25, 21.25),
                          (9.75, 21.25), (9.75, 12.25), (2.75, 12.25)], True)),
            ("D", g.p(("M", 9.75, 9.75), ("H", 14.25)))]


reg("switch-join", "Switch join", _badged(join, "refresh", (2, 7.5, 16, 21.5)))


@icon("join-geom", "Join geom")
def join_geom(g):
    k = 14.5 - math.sqrt(5.75 ** 2 - 1.25 ** 2)
    return [("S", g.p(("M", 3.75, 3.75), ("H", 13.25), ("V", k), ("A", 5.75, 1, 1, k, 13.25),
                      ("H", 3.75), ("Z",))),
            ("D", g.p(("M", 13.25, k), ("V", 13.25), ("H", k)))]


@icon("unjoin-geom", "Unjoin geom")
def unjoin_geom(g):
    return [("S", g.rect(3.75, 3.75, 12.25, 12.25)), ("L", g.rect(11.75, 11.75, 20.25, 20.25)),
            ("L", g.poly([(15, 4), (20, 9)])), ("L", g.poly([(20, 4), (15, 9)]))]


@icon("cut-geom", "Cut geom")
def cut_geom(g):
    return [("S", g.p(("M", 3.75, 3.75), ("H", 15.25), ("V", 10), ("A", 5.25, 0, 0, 10, 15.25),
                      ("H", 3.75), ("Z",))),
            ("L", g.p(("M", 15.25, 10), ("A", 5.25, 1, 1, 10, 15.25)))]


@icon("scale", "Scale")
def scale(g):
    return ([("S", g.rect(3.75, 12.75, 11.25, 20.25))] + dash_rect(g, 3.75, 3.75, 20.25, 20.25, 2, 2)
            + arrow(g, 12.5, 11.5, 17.5, 6.5, 3))


@icon("pin", "Pin")
def pin(g):
    return [("S", g.poly([(9.75, 3.75), (14.25, 3.75), (14.25, 9.75), (17.25, 12.75), (6.75, 12.75),
                          (9.75, 9.75)], True)),
            ("L", g.p(("M", 7.75, 3.75), ("H", 16.25))),
            ("L", g.p(("M", 11.75, 12.75), ("V", 21.25)))]


reg("unpin", "Unpin", _badged(pin, "x"))


@icon("match-type", "Match type")
def match_type(g):
    return ([("S", g.rect(2.75, 2.75, 10.25, 10.25, 1))] + dash_rect(g, 13.75, 13.75, 21.25, 21.25, 1.5, 1.5)
            + [("L", g.p(("M", 6.75, 13.75), ("V", 17.75), ("H", 10.75))), chevron(g, 10.75, 17.75, "r", 2.5)])


@icon("swap-type", "Swap type")
def swap_type(g):
    return [("S", g.rect(2.75, 2.75, 10.25, 10.25, 1)), ("L", g.rect(13.75, 13.75, 21.25, 21.25, 1)),
            ("L", g.p(("M", 13.75, 6.75), ("H", 19.25), ("V", 10.25))), chevron(g, 19.25, 10.25, "d", 2.25),
            ("L", g.p(("M", 10.25, 17.25), ("H", 4.75), ("V", 13.75))), chevron(g, 4.75, 13.75, "u", 2.25)]


@icon("eyedropper", "Eyedropper")
def eyedropper(g):
    return [("S", g.poly([(3.75, 20.25), (3.75, 17.25), (12.5, 8.5), (15.5, 11.5), (6.75, 20.25)], True)),
            ("L", g.poly([(10.5, 6.5), (17.5, 13.5)])),
            ("S", g.poly([(14, 8.5), (17.25, 5.25), (18.75, 5.25), (18.75, 6.75), (15.5, 10)], True)),
            ("L", g.poly([(16, 4), (20, 8)]))]


@icon("copy-params", "Copy params")
def copy_params(g):
    return [("L", g.p(("M", 8.75, 15.25), ("H", 3.75), ("V", 3.75), ("H", 15.25), ("V", 8.75))),
            ("S", g.rect(8.75, 8.75, 20.25, 20.25, 1)),
            ("D", g.p(("M", 11.75, 12.75), ("H", 17.25))), ("D", g.p(("M", 11.75, 16.25), ("H", 15.25)))]


@icon("paste", "Paste")
def paste(g):
    return [("L", g.p(("M", 9.75, 21.25), ("H", 3.75), ("V", 4.75), ("H", 17.25), ("V", 8.25))),
            ("D", g.rect(8.25, 2.75, 12.75, 6.75, 1)),
            ("S", g.rect(11.75, 11.75, 21.25, 21.25, 1)),
            ("D", g.p(("M", 14.75, 15.75), ("H", 18.25)))]


@icon("paste-aligned", "Paste aligned")
def paste_aligned(g):
    return clipboard(g) + [("D", g.rect(8.75, 10.75, 15.25, 17.25))]


@icon("paste-levels", "Paste levels")
def paste_levels(g):
    out = clipboard(g)
    for y in (10.75, 16.75):
        out.append(("D", g.p(("M", 10.75, y), ("H", 16.25))))
        out.append(("K", g.poly([(7.75, y - 1.5), (9.75, y), (7.75, y + 1.5)], True)))
    return out


@icon("paste-params", "Paste params")
def paste_params(g):
    return clipboard(g) + [("D", g.p(("M", 8.75, 9.75), ("H", 15.25))),
                           ("D", g.p(("M", 8.75, 13.75), ("H", 15.25))),
                           ("D", g.p(("M", 8.75, 17.75), ("H", 12.25)))]


@icon("clip-copy", "Clip copy")
def clip_copy(g):
    return clipboard(g) + [("D", g.p(("M", 11.75, 9.25), ("V", 17.75))),
                           ("D", g.poly([(8.75, 14.75), (11.75, 17.75), (14.75, 14.75)]))]

@icon("cut", "Cut")
def cut(g):
    return scissors(g)


@icon("push-pull", "Push pull")
def push_pull(g):
    return [face(g, 12, 16.5, 9), ("L", g.p(("M", 11.75, 15.25), ("V", 3.75))),
            chevron(g, 11.75, 3.75, "u", 3)]


@icon("offset-face", "Offset face")
def offset_face(g):
    return [face(g, 12, 16.5, 9), face(g, 12, 7.5, 9, "L"),
            ("L", g.p(("M", 3.75, 10), ("V", 13.25))), ("L", g.p(("M", 20.25, 10), ("V", 13.25)))]


@icon("split-face", "Split face")
def split_face(g):
    return [face(g, 12, 12, 9.25), ("D", g.poly([(7.375, 9.6875), (16.625, 14.3125)])),
            ("L", g.p(("M", 2.75, 20.25), ("H", 21.25)))]


@icon("push-region", "Push region")
def push_region(g):
    return [face(g, 12, 16.5, 9), face(g, 12, 6.5, 4.25, "L"),
            ("L", g.p(("M", 7.75, 6.5), ("V", 16.5))), ("L", g.p(("M", 16.25, 6.5), ("V", 16.5)))]


@icon("draft", "Draft")
def draft(g):
    return [("S", g.poly([(5.75, 20.25), (9.25, 3.75), (14.75, 3.75), (18.25, 20.25)], True)),
            ("D", g.poly([(5.75, 20.25), (5.75, 3.75)])),
            ("L", g.p(("M", 2.75, 20.25), ("H", 21.25)))]


@icon("gizmo", "Gizmo")
def gizmo(g):
    o = (12, 13)
    return [("L", g.p(("M", 11.75, 13), ("V", 3.25))), chevron(g, 11.75, 3.25, "u", 2.75),
            ("L", g.poly([o, (20.5, 17.25)])), ("L", g.poly([o, (3.5, 17.25)])),
            ("L", g.poly([(17.5, 18.75), (20.5, 17.25), (19, 14.25)])),
            ("L", g.poly([(6.5, 18.75), (3.5, 17.25), (5, 14.25)])),
            ("K", ring(g, 12, 13, 1.75))]


# ---------------------------------------------------------------- selection

def _marquee(g):
    return dash_rect(g, 2.75, 2.75, 18.25, 18.25, 2, 2)


@icon("box-select", "Box select")
def box_select(g):
    c, top = cube(g, 3.25, 18.25, 3.75, 7.5)
    return [("FD", top)] + c + sel_cursor(g)


@icon("select-all", "Select all")
def select_all(g):
    return (_marquee(g) + [("S", g.rect(5.75, 5.75, 10.25, 10.25)), ("S", g.rect(11.75, 5.75, 15.25, 9.25)),
                           ("S", g.rect(5.75, 11.75, 9.25, 15.25))] + sel_cursor(g))


@icon("select-similar", "Select similar")
def select_similar(g):
    return ([("S", g.circ(2.75, 2.75, 10.25, 10.25)), ("L", g.circ(2.75, 12.75, 10.25, 20.25)),
             ("L", g.circ(12.75, 2.75, 20.25, 10.25))] + sel_cursor(g, 13.75, 13.75, 0.45))


@icon("similar-project", "Similar project")
def similar_project(g):
    return ([("S", g.circ(2.75, 5.75, 9.25, 12.25)), ("L", g.circ(2.75, 14.75, 9.25, 21.25)),
             ("L", g.circ(10.75, 5.75, 17.25, 12.25))]
            + badge_local(g, "stack") + sel_cursor(g, 13.75, 13.75, 0.45))


def _same_cat(g):
    return [("S", g.poly([(6.5, 2.75), (10.25, 9.25), (2.75, 9.25)], True)),
            ("S", g.poly([(16.5, 2.75), (20.25, 9.25), (12.75, 9.25)], True)),
            ("L", g.poly([(6.5, 12.75), (10.25, 19.25), (2.75, 19.25)], True))]


@icon("same-cat", "Same cat")
def same_cat(g):
    return _same_cat(g) + sel_cursor(g, 13.75, 13.75, 0.45)


@icon("same-cat-project", "Same cat project")
def same_cat_project(g):
    return [("S", g.poly([(6.5, 5.75), (10.25, 12.25), (2.75, 12.25)], True)),
            ("L", g.poly([(6.5, 14.75), (10.25, 21.25), (2.75, 21.25)], True)),
            ("L", g.poly([(14, 5.75), (17.25, 11.25), (10.75, 11.25)], True))] \
        + badge_local(g, "stack") + sel_cursor(g, 13.75, 13.75, 0.45)


@icon("sel-chain", "Sel chain")
def sel_chain(g):
    return [("L", g.p(("M", 4.25, 17.25), ("V", 4.75), ("H", 12.75), ("V", 10.75), ("H", 19.75))),
            ("K", ring(g, 4.25, 17.25, 1.5)), ("K", ring(g, 12.75, 4.75, 1.5)),
            ("K", ring(g, 19.75, 10.75, 1.5))] + sel_cursor(g, 11.25, 12.75, 0.5)


@icon("sel-connected", "Sel connected")
def sel_connected(g):
    return [("L", g.p(("M", 2.75, 7.75), ("H", 21.25))), ("L", g.p(("M", 7.75, 7.75), ("V", 20.25))),
            ("L", g.p(("M", 16.25, 2.75), ("V", 7.75))),
            ("K", ring(g, 7.75, 7.75, 1.75)), ("K", ring(g, 16.25, 7.75, 1.75))] + sel_cursor(g)


@icon("sel-invert", "Sel invert")
def sel_invert(g):
    return corners(g, 2.75, 2.75, 21.25, 21.25, 4) + [
        ("L", g.circ(6.75, 6.75, 17.25, 17.25)),
        ("K", g.p(("M", 12, 6.75), ("A", None, 0, 1, 12, 17.25), ("Z",)))]


@icon("sel-prev", "Sel prev")
def sel_prev(g):
    return [cursor(g, 4.5, 6.5, 0.85)] + badge_local(g, "back")


@icon("sel-sets", "Sel sets")
def sel_sets(g):
    return (dash_rect(g, 2.75, 2.75, 12.25, 12.25, 1.75, 1.75)
            + [("S", g.rect(6.75, 6.75, 15.25, 15.25, 1))] + sel_cursor(g, 16.25, 16.25, 0.33))


@icon("set-save", "Set save")
def set_save(g):
    return (dash_rect(g, 2.75, 5.75, 17.25, 21.25, 2, 2) + [("S", g.rect(6.75, 9.75, 13.25, 17.25))]
            + badge(g, "in"))


@icon("filter", "Filter")
def filter_(g):
    return [("S", g.poly([(2.75, 3.75), (21.25, 3.75), (14.25, 11.75), (14.25, 18.25), (9.75, 21.25),
                          (9.75, 11.75)], True))]


# ---------------------------------------------------------------- groups

@icon("group", "Group")
def group(g):
    return frame_group(g)


reg("group-add", "Group add", _badged(group_open, "plus"))
def group_one(g):
    return group_open(g)[:2]


reg("group-remove", "Group remove", _badged(group_one, "minus"))
reg("group-delete", "Group delete", _badged(group_open, "x"))
reg("group-edit", "Group edit", _badged(group_open, "pencil"))
reg("group-finish", "Group finish", _badged(group_open, "tick"))
reg("group-cancel", "Group cancel", local_badged(group_open, "cancel"))
reg("group-place", "Group place", _badged(group_open, "in"))
reg("group-select", "Group select", _badged(group_open, "cursor"))


@icon("ungroup", "Ungroup")
def ungroup(g):
    return (dash_rect(g, 2.75, 2.75, 21.25, 21.25, 2.5, 2.5)
            + [("S", g.rect(5.75, 5.75, 10.25, 10.25)), ("S", g.circ(13.75, 13.75, 18.25, 18.25))])


# ---------------------------------------------------------------- visibility

@icon("hide", "Hide")
def hide(g):
    return eye_off(g)


reg("hide-apply", "Hide apply", _badged(hide, "tick"))
reg("hide-cat", "Hide cat", _badged(hide, "clock"))
reg("hide-cat-3d", "Hide cat 3d", _badged(hide, "cube"))


@icon("isolate", "Isolate")
def isolate(g):
    return [("S", g.rect(7.75, 7.75, 16.25, 16.25, 1))] + dash_rect(g, 2.75, 2.75, 21.25, 21.25, 2, 2.6)


reg("isolate-cat-3d", "Isolate cat 3d", _badged(isolate, "cube"))


@icon("reveal", "Reveal")
def reveal(g):
    return [("L", g.p(("M", 2.75, 12), ("Q", 12, 2.5, 21.25, 12))),
            ("S", g.circ(8.75, 8.75, 15.25, 15.25))] + [
        ("L", g.poly([(4.5, 14), (5.5, 15)])), ("L", g.poly([(8.5, 17.5), (9.5, 18)])),
        ("L", g.poly([(14.5, 18), (15.5, 17.5)])), ("L", g.poly([(18.5, 15), (19.5, 14)]))]


reg("reveal-3d", "Reveal 3d", _badged(reveal, "cube"))


@icon("vg-3d", "Vg 3d")
def vg_3d(g):
    return [face(g, 11, 15.5, 8.5), face(g, 11, 9.5, 8.5, "L")] + badge_local(g, "cube")


# ---------------------------------------------------------------- measure, history, zoom

@icon("measure", "Measure")
def measure(g):
    out = [("S", g.rect(2.75, 7.75, 21.25, 16.25, 1))]
    for x, y1 in ((6.75, 11.25), (10.75, 12.75), (14.75, 11.25), (18.25, 12.75)):
        out.append(("D", g.p(("M", x, 7.75), ("V", y1))))
    return out


reg("measure-3d", "Measure 3d", _badged(measure, "cube", (2, 7, 19, 24)))


@icon("redo", "Redo")
def redo(g):
    return [("L", g.poly([(15.75, 3.75), (20.25, 8.25), (15.75, 12.75)])),
            ("L", g.p(("M", 20.25, 8.25), ("H", 9.75), ("A", None, 0, 0, 9.75, 20.25), ("H", 15.75)))]


@icon("repeat", "Repeat")
def repeat(g):
    return refresh(g, 12, 12, 8) + [("K", ring(g, 12, 12, 2.25))]


@icon("zoom-fit", "Zoom fit")
def zoom_fit(g):
    return corners(g, 2.75, 2.75, 21.25, 21.25, 5) + [
        ("S", g.circ(7.75, 7.75, 14.25, 14.25)), ("L", g.poly([(13.5, 13.5), (16.75, 16.75)]))]


@icon("zoom-sel", "Zoom sel")
def zoom_sel(g):
    return magnifier(g, 2.75, 2.75, 16.25, 16.25, 20.75) + [("K", g.rect(7.75, 7.75, 11.25, 11.25))]


reg("zoom-sel-3d", "Zoom sel 3d", _badged(zoom_sel, "cube"))


@icon("maximize", "Maximize")
def maximize(g):
    return [("L", g.rect(2.75, 2.75, 21.25, 21.25, 1)),
            ("L", g.poly([(7.5, 16.5), (16.5, 7.5)])),
            ("L", g.p(("M", 11.75, 6.75), ("H", 17.25), ("V", 12.25))),
            ("L", g.p(("M", 6.75, 11.75), ("V", 17.25), ("H", 12.25)))]


@icon("tile", "Tile")
def tile(g):
    return [("S", g.rect(2.75, 2.75, 10.25, 10.25, 1)), ("L", g.rect(13.75, 2.75, 21.25, 10.25, 1)),
            ("L", g.rect(2.75, 13.75, 10.25, 21.25, 1)), ("L", g.rect(13.75, 13.75, 21.25, 21.25, 1))]


# ---------------------------------------------------------------- views

@icon("3d-view", "3d view")
def view_3d(g):
    c, top = big_cube(g)
    return [("FD", top)] + c


reg("home-3d", "Home 3d", local_badged(view_3d, "home"))
reg("view-save", "View save", _badged(view_3d, "in"))


@icon("section-box", "Section box")
def section_box(g):
    c, top = cube(g, 4.25, 19.25, 4.25, 7.5)
    a, b = g.sx(4.25), g.sx(19.25)
    cx, hw, yt = (a + b) / 2, (b - a) / 2, g.sy(4.25)
    hh = round(7.5 * g.k * 2) / 2
    r = 1.5 * g.k
    pts = ((cx, yt), (a, yt + hw / 2 + hh), (b, yt + hw / 2 + hh), (cx, yt + hw))
    return [("L", c[0][1]), ("L", c[1][1])] + [("K", g_poly_dot(x, y, r)) for x, y in pts]


def g_poly_dot(x, y, r):
    return (f"M{fmt(x-r)} {fmt(y)}A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x+r)} {fmt(y)}"
            f"A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x-r)} {fmt(y)}Z")


@icon("section-caps", "Section caps")
def section_caps(g):
    c, top = big_cube(g)
    q = cube_pts(g)
    a, cx, ys, yc, hh = q["a"], q["cx"], q["ys"], q["yc"], q["hh"]
    hatch = []
    for t in (0.35, 0.7):
        y0 = ys + hh * t
        hatch.append(("H", f"M{fmt(a)} {fmt(y0)}L{fmt(cx)} {fmt(y0+(yc-ys))}"))
    return [("K", top), c[0], ("D", c[1][1])] + hatch


reg("section-here", "Section here", _badged(B.section, "cursor"))


@icon("plan-view", "Plan view")
def plan_view(g):
    return plan(g)


def plan_open(g):
    return [("L", g.p(("M", 9.75, 20.25), ("H", 3.75), ("V", 3.75), ("H", 13.75))),
            ("L", g.p(("M", 20.25, 12.75), ("V", 20.25), ("H", 14.75))),
            ("L", g.p(("M", 11.75, 3.75), ("V", 12.75), ("H", 20.25))),
            ("L", g.p(("M", 3.75, 12.75), ("H", 6.75)))]


reg("plan-here", "Plan here", _badged(plan_open, "cursor"))


@icon("plan-grid", "Plan grid")
def plan_grid(g):
    return [("L", g.rect(2.75, 2.75, 21.25, 21.25, 1)),
            ("L", g.p(("M", 8.75, 2.75), ("V", 21.25))), ("L", g.p(("M", 15.25, 2.75), ("V", 21.25))),
            ("L", g.p(("M", 2.75, 8.75), ("H", 21.25))), ("L", g.p(("M", 2.75, 15.25), ("H", 21.25)))]


@icon("walk", "Walk")
def walk(g):
    return [("S", g.rect(3.75, 8.75, 9.25, 20.25, 2.75)), ("S", g.rect(14.75, 3.75, 20.25, 15.25, 2.75)),
            ("D", g.p(("M", 3.75, 15.75), ("H", 9.25))), ("D", g.p(("M", 14.75, 10.75), ("H", 20.25)))]


@icon("projection", "Projection")
def projection(g):
    return [("L", g.p(("M", 20.25, 4), ("V", 20))),
            ("L", g.poly([(20.25, 4), (4.25, 12), (20.25, 20)])),
            ("D", g.p(("M", 14.75, 7.25), ("V", 16.75))),
            ("K", ring(g, 4.25, 12, 1.75))]


@icon("snaps", "Snaps")
def snaps(g):
    return magnet(g)


@icon("snap-3d", "Snap 3d")
def snap_3d(g):
    c, _ = cube(g, 15.25, 21.25, 14.25, 3)
    return magnet(g.sub(2, 2, 18, 18)) + bdg([("L", c[0][1]), ("L", c[1][1])])


@icon("ortho", "Ortho")
def ortho(g):
    return [("L", g.p(("M", 4.75, 2.75), ("V", 19.25), ("H", 21.25))),
            ("L", g.p(("M", 4.75, 13.75), ("H", 10.25), ("V", 19.25)))]


@icon("parallel", "Parallel")
def parallel(g):
    return [("L", g.poly([(3, 15), (15, 3)])), ("L", g.poly([(9, 21), (21, 9)])),
            ("L", g.poly([(7.5, 11.5), (12.5, 16.5)]))]


@icon("underlay", "Underlay")
def underlay(g):
    return [face(g, 12, 9, 9.25)] + [
        ("L", g.poly([(2.75, 15), (4.75, 16)])), ("L", g.poly([(7.75, 17.5), (9.75, 18.5)])),
        ("L", g.poly([(14.25, 18.5), (16.25, 17.5)])), ("L", g.poly([(19.25, 16), (21.25, 15)]))]


# ---------------------------------------------------------------- display styles
# A box turned off the corner (2:1 edges, unequal sides) so the hidden back edges stand apart
# from the front ones; drawn in pixel space per master: (front x, top y, left w, right w, height).
# "lo": the front edge reads crisp on its left side, "hi" on its right (keep it beside an open face).
BOXES = {"lo": {24: (13.75, 11.75, 11, 7.5, 9.5), 20: (11.75, 10.25, 9, 5.5, 7),
                16: (9.5, 8.5, 7, 5, 5)},
         "hi": {24: (13.25, 11.75, 10.5, 8, 9.5), 20: (11.25, 10.25, 8.5, 6, 7),
                16: (9.5, 8.5, 7, 5, 5)}}


def box_geo(g, side="lo"):
    fx, fy, w1, w2, h = BOXES[side][g.n]
    F, Lp, R = (fx, fy), (fx - w1, fy - w1 / 2), (fx + w2, fy - w2 / 2)
    K = (Lp[0] + R[0] - fx, Lp[1] + R[1] - fy)

    def dn(p):
        return (p[0], p[1] + h)
    return dict(F=F, L=Lp, R=R, K=K, Fd=dn(F), Ld=dn(Lp), Rd=dn(R), Kd=dn(K))


def _style(g, faces=(), hidden=None, front=True, side="lo"):
    q = box_geo(g, side)
    out = [("L", g_poly([q["K"], q["R"], q["Rd"], q["Fd"], q["Ld"], q["L"]]))]
    fmap = {"top": [q["K"], q["R"], q["F"], q["L"]], "left": [q["L"], q["F"], q["Fd"], q["Ld"]],
            "right": [q["F"], q["R"], q["Rd"], q["Fd"]]}
    for f in faces:
        out.append(("K", g_poly(fmap[f])))
    if front:
        out.append(("L", g.raw([q["L"], q["F"], q["R"]])))
        out.append(("L", g.raw([q["F"], q["Fd"]])))
    if hidden:
        for p0, p1 in ((q["Kd"], q["K"]), (q["Kd"], q["Ld"]), (q["Kd"], q["Rd"])):
            spans = [(0, 1)] if hidden == "solid" else [(0, 0.3), (0.6, 0.8)]
            for t0, t1 in spans:
                a = (p0[0] + (p1[0] - p0[0]) * t0, p0[1] + (p1[1] - p0[1]) * t0)
                b = (p0[0] + (p1[0] - p0[0]) * t1, p0[1] + (p1[1] - p0[1]) * t1)
                out.append(("L", g.raw([a, b])))
    return out


@icon("style-wire", "Style wire")
def style_wire(g):
    return _style(g, hidden="solid")


@icon("style-hidden", "Style hidden")
def style_hidden(g):
    return _style(g)


@icon("style-plain", "Style plain")
def style_plain(g):
    return _style(g, faces=("left", "right"), front=False)


@icon("style-shaded", "Style shaded")
def style_shaded(g):
    return _style(g, faces=("left",), side="hi")


@icon("style-consistent", "Style consistent")
def style_consistent(g):
    return _style(g, faces=("top", "right"))


@icon("style-realistic", "Style realistic")
def style_realistic(g):
    q = box_geo(g)
    out = _style(g)
    h = BOXES["lo"][g.n][4]
    for t in (0.5,):
        dy = h * t
        out.append(("L", g.raw([(q["L"][0], q["L"][1] + dy), (q["F"][0], q["F"][1] + dy),
                                (q["R"][0], q["R"][1] + dy)])))
    return out


@icon("style-xray", "Style xray")
def style_xray(g):
    return _style(g, hidden="solid", front=False)


@icon("shading", "Shading")
def shading(g):
    return [("S", g.circ(2.75, 2.75, 21.25, 21.25)),
            ("K", g.p(("M", 18.54, 5.46), ("A", 9.25, 0, 1, 5.46, 18.54), ("Z",)))]


@icon("thin-lines", "Thin lines")
def thin_lines(g):
    return [("K", g.rect(2.75, 4.75, 21.25, 7.25)),
            ("L", g.p(("M", 2.75, 12.75), ("H", 21.25))), ("L", g.p(("M", 2.75, 18.75), ("H", 21.25)))]


@icon("detail-level", "Detail level")
def detail_level(g):
    return [("S", g.rect(3.75, 15.75, 7.25, 20.25)), ("S", g.rect(10.25, 10.75, 13.75, 20.25)),
            ("S", g.rect(16.75, 4.75, 20.25, 20.25))]


# ---------------------------------------------------------------- performance modes

@icon("gfx-auto", "Gfx auto")
def gfx_auto(g):
    return gauge(g, 90)[:2] + [("K", ring(g, 12, 16.25, 1.25))] + badge(g, "refresh")


@icon("gfx-balanced", "Gfx balanced")
def gfx_balanced(g):
    return gauge(g, 90)


@icon("gfx-quality", "Gfx quality")
def gfx_quality(g):
    return gauge(g, 150)


@icon("gfx-speed", "Gfx speed")
def gfx_speed(g):
    return gauge(g, 30)
