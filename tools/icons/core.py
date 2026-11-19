"""Icon geometry core: one drawing on the 24 grid -> snapped optical masters 24/20/16 in three
directions (outline, duotone, filled). Rules: docs/design/ICONS_V4.md.

Coordinates (24 grid): .75 = low-side edge (left/top), .25 = high-side edge (right/bottom); the
outer stroke edge then lands on a pixel boundary at 24 and 20, and both snap to .5 at 16 (1 px
stroke). Other fractions are free: diagonals, centres, arc ends."""
import math

ACCENT = "#2457D6"
W = {24: 1.5, 20: 1.5, 16: 1.0}
SIZES = (24, 20, 16)
MODES = ("outline", "duotone", "filled")


class V:
    """Explicit per-size value; 20/16 entries are in target pixel space (identity grid only)."""

    def __init__(self, a24, a20=None, a16=None):
        self.a = {24: a24, 20: a20, 16: a16}


def fmt(v):
    s = ("%.3f" % v).rstrip("0").rstrip(".")
    return "0" if s in ("-0", "") else s


def _edge(v):
    return {75: 0.75, 25: 0.25}.get(round((v % 1) * 100))


class G:
    """A master of `size` px. `ax`/`ay` are (scale, offset) applied on the 24 grid before the size
    scale, so a sub-grid (badge base, small motif) snaps by the same rule as the full glyph."""

    def __init__(self, size, ax=(1.0, 0.0), ay=(1.0, 0.0)):
        self.n, self.k, self.w = size, size / 24, W[size]
        self.ax, self.ay = ax, ay
        self.ident = ax == (1.0, 0.0) and ay == (1.0, 0.0)

    def sub(self, x0, y0, x1, y1):
        """Child grid: the live area 2..22 lands on (x0, y0)-(x1, y1); strokes keep their width."""
        sx, sy = (x1 - x0) / 20, (y1 - y0) / 20
        ax = (self.ax[0] * sx, self.ax[0] * (x0 - 2 * sx) + self.ax[1])
        ay = (self.ay[0] * sy, self.ay[0] * (y0 - 2 * sy) + self.ay[1])
        return G(self.n, ax, ay)

    def _snap(self, v, a):
        if isinstance(v, V):
            if self.ident and v.a[self.n] is not None:
                return v.a[self.n]
            v = v.a[24]
        if self.n == 24 and self.ident:
            return v
        u = (a[0] * v + a[1]) * self.k
        off = _edge(v)
        if off is None:
            return round(u, 3)
        if self.w == 1.0:
            return round(u - 0.5) + 0.5
        lo = (self.w / 2) % 1  # outer stroke edge on a pixel boundary
        off = lo if off == 0.75 else 1 - lo
        return round(u - off) + off

    def sx(self, v):
        return self._snap(v, self.ax)

    def sy(self, v):
        return self._snap(v, self.ay)

    def s(self, v):
        return self.sx(v)

    def r(self, v):
        if isinstance(v, V):
            if self.ident and v.a[self.n] is not None:
                return v.a[self.n]
            v = v.a[24]
        return v * self.k * self.ax[0]

    def p(self, *t):
        """Path from tokens (M L H V A Q Z); coordinates snapped. ('A', r|None, large, sweep, x, y):
        r None = quarter arc when both deltas are non-zero, else a half circle on the chord."""
        out, cx, cy = [], 0, 0
        for tok in t:
            c = tok[0]
            if c in "ML":
                cx, cy = self.sx(tok[1]), self.sy(tok[2])
                out.append(f"{c}{fmt(cx)} {fmt(cy)}")
            elif c == "H":
                cx = self.sx(tok[1])
                out.append(f"H{fmt(cx)}")
            elif c == "V":
                cy = self.sy(tok[1])
                out.append(f"V{fmt(cy)}")
            elif c == "A":
                x, y = self.sx(tok[4]), self.sy(tok[5])
                r = tok[1]
                if r is None:
                    dx, dy = abs(x - cx), abs(y - cy)
                    r = max(dx, dy) if min(dx, dy) > 1e-6 else math.hypot(dx, dy) / 2
                else:
                    r = self.r(r)
                out.append(f"A{fmt(r)} {fmt(r)} 0 {tok[2]} {tok[3]} {fmt(x)} {fmt(y)}")
                cx, cy = x, y
            elif c == "Q":
                out.append(f"Q{fmt(self.sx(tok[1]))} {fmt(self.sy(tok[2]))} "
                           f"{fmt(self.sx(tok[3]))} {fmt(self.sy(tok[4]))}")
                cx, cy = self.sx(tok[3]), self.sy(tok[4])
            elif c == "Z":
                out.append("Z")
        return "".join(out)

    def poly(self, pts, close=False):
        t = [("M",) + tuple(pts[0])] + [("L",) + tuple(q) for q in pts[1:]]
        return self.p(*(t + ([("Z",)] if close else [])))

    def rect(self, x0, y0, x1, y1, r=0):
        x0, y0, x1, y1 = self.sx(x0), self.sy(y0), self.sx(x1), self.sy(y1)
        r = self.r(r)
        if not r:
            return f"M{fmt(x0)} {fmt(y0)}H{fmt(x1)}V{fmt(y1)}H{fmt(x0)}Z"
        a = f"A{fmt(r)} {fmt(r)} 0 0 1 "
        return (f"M{fmt(x0+r)} {fmt(y0)}H{fmt(x1-r)}{a}{fmt(x1)} {fmt(y0+r)}V{fmt(y1-r)}"
                f"{a}{fmt(x1-r)} {fmt(y1)}H{fmt(x0+r)}{a}{fmt(x0)} {fmt(y1-r)}V{fmt(y0+r)}"
                f"{a}{fmt(x0+r)} {fmt(y0)}Z")

    def circ(self, x0, y0, x1, y1):
        x0, y0, x1, y1 = self.sx(x0), self.sy(y0), self.sx(x1), self.sy(y1)
        cy, r = (y0 + y1) / 2, (x1 - x0) / 2
        return (f"M{fmt(x0)} {fmt(cy)}A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x1)} {fmt(cy)}"
                f"A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x0)} {fmt(cy)}Z")

    def dot(self, x, y):
        """Filled point (role K/KX): a small circle, never a thick zero-length stroke."""
        x, y, r = self.sx(x), self.sy(y), 0.3 * self.w
        return (f"M{fmt(x - r)} {fmt(y)}A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x + r)} {fmt(y)}"
                f"A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x - r)} {fmt(y)}Z")

    def spark(self, cx, cy, R, k=0.3):
        cx, cy, R = self.sx(cx), self.sy(cy), self.r(R)
        q = R * k
        pts = [(cx, cy - R), (cx + R, cy), (cx, cy + R), (cx - R, cy)]
        ctl = [(cx + q, cy - q), (cx + q, cy + q), (cx - q, cy + q), (cx - q, cy - q)]
        d = f"M{fmt(pts[0][0])} {fmt(pts[0][1])}"
        for i in range(4):
            c, e = ctl[i], pts[(i + 1) % 4]
            d += f"Q{fmt(c[0])} {fmt(c[1])} {fmt(e[0])} {fmt(e[1])}"
        return d + "Z"

    def raw(self, pts):
        """Polyline through already-snapped pixel coordinates."""
        return "M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in pts)


# Roles: L line | S surface (stroke + fill) | F fill-only surface | SF stroke, filled only in C
#        FD fill in duotone only | D detail (knocked out when filled) | H hatch (dropped when
#        filled) | K solid ink mark | KX solid mark knocked out when filled | A AI spark (ink;
#        accent in duotone) | AX same, knocked out when filled.
# A leading "b" puts the element in the badge layer: drawn last, with a knocked-out halo around
# it in the base below.
SURFACE_ROLES = ("S", "F", "SF", "FD")
ICONS = []
_NAMES = set()


def icon(name, ru, family=None):
    def deco(fn):
        assert name not in _NAMES, f"duplicate glyph {name}"
        _NAMES.add(name)
        ICONS.append((name, ru, fn))
        return fn
    return deco


def alias_glyph(name, ru, fn):
    """Register a glyph drawn by an existing function (a twin under a new id)."""
    icon(name, ru)(fn)


def _layers(els, mode, mid, size):
    fills, strokes, marks, kos = [], [], [], []
    for role, d in els:
        if role in ("S", "F") or (role == "SF" and mode == "filled") or \
                (role == "FD" and mode == "duotone"):
            fills.append(d)
        if role in ("S", "L", "SF"):
            strokes.append(d)
        if role in ("D", "H"):
            if mode == "filled":
                if role == "D":
                    kos.append(f'<path d="{d}" stroke="#000"/>')
            else:
                strokes.append(d)
        if role in ("K", "KX", "A", "AX"):
            if mode == "filled" and role.endswith("X"):
                st = "#000" if role[0] == "K" else "none"
                kos.append(f'<path d="{d}" fill="#000" stroke="{st}"/>')
            else:
                col = ACCENT if (mode == "duotone" and role.startswith("A")) else "currentColor"
                st = "" if role[0] == "K" else ' stroke="none"'
                marks.append(f'<path d="{d}" fill="{col}"{st}/>')
    body = []
    if mode == "duotone" and fills:
        body.append(f'<path d="{" ".join(fills)}" fill="{ACCENT}" fill-opacity=".2" stroke="none"/>')
    if mode == "filled" and fills:
        fp = f'<path d="{" ".join(fills)}" fill="currentColor" stroke="none"/>'
        if kos:
            body.append(f'<mask id="{mid}" maskUnits="userSpaceOnUse" x="0" y="0" width="{size}" '
                        f'height="{size}"><rect width="{size}" height="{size}" fill="#fff"/>'
                        f'{"".join(kos)}</mask>')
            body.append(f'<g mask="url(#{mid})">{fp}</g>')
        else:
            body.append(fp)
    if strokes:
        body.append(f'<path d="{"".join(strokes)}"/>')
    body += marks
    return body


HALO = 1.0  # px of clear space between a badge and the base it sits on


def render(name, fn, size, mode):
    g = G(size)
    els = fn(g)
    base = [(r, d) for r, d in els if not r.startswith("b")]
    badge = [(r[1:], d) for r, d in els if r.startswith("b")]
    body = _layers(base, mode, f"k-{name}-{size}", size)
    if badge:
        w = W[size] + 2 * HALO
        cut = []
        for role, d in badge:
            fill = "#000" if role in ("S", "F", "SF", "K", "KX", "A", "AX") else "none"
            cut.append(f'<path d="{d}" fill="{fill}" stroke="#000" stroke-width="{fmt(w)}"/>')
        hid = f"h-{name}-{size}"
        body = ([f'<mask id="{hid}" maskUnits="userSpaceOnUse" x="0" y="0" width="{size}" '
                 f'height="{size}"><rect width="{size}" height="{size}" fill="#fff"/>'
                 f'{"".join(cut)}</mask>', f'<g mask="url(#{hid})">'] + body + ["</g>"]
                + _layers(badge, mode, f"kb-{name}-{size}", size))
    return body, fmt(W[size])


def svg(name, fn, size, mode):
    body, w = render(name, fn, size, mode)
    return (f'<svg xmlns="http://www.w3.org/2000/svg" width="{size}" height="{size}" '
            f'viewBox="0 0 {size} {size}" fill="none" stroke="currentColor" stroke-width="{w}" '
            f'stroke-linecap="round" stroke-linejoin="round">' + "".join(body) + "</svg>\n")


def has_surface(fn):
    return any(r.lstrip("b") in SURFACE_ROLES or r.lstrip("b") in ("D", "KX", "AX")
               for r, _ in fn(G(24)))


# ---------------------------------------------------------------- shared motifs (24 grid)

def L(g, *pts):
    return ("L", g.poly(pts))


def cube(g, x0=2.75, x1=14.75, ytop=8.75, h=6.5):
    """2:1 dimetric model cube; returns (elements, top face)."""
    a, b = g.sx(x0), g.sx(x1)
    cx, hw = (a + b) / 2, (b - a) / 2
    yt = g.sy(ytop)
    hh = round(h * g.k * g.ay[0] * 2) / 2
    ys, yc = yt + hw / 2, yt + hw
    hex_ = g_poly([(cx, yt), (b, ys), (b, ys + hh), (cx, yc + hh), (a, ys + hh), (a, ys)])
    top = g_poly([(cx, yt), (b, ys), (cx, yc), (a, ys)])
    y_ = (f"M{fmt(a)} {fmt(ys)}L{fmt(cx)} {fmt(yc)}L{fmt(b)} {fmt(ys)}"
          f"M{fmt(cx)} {fmt(yc)}V{fmt(yc+hh)}")
    return [("SF", hex_), ("D", y_)], top


def model(g, x0=2.75, x1=14.75, ytop=8.75, h=6.5):
    c, top = cube(g, x0, x1, ytop, h)
    return [("FD", top)] + c


def g_poly(pts):
    return "M" + "L".join(f"{fmt(x)} {fmt(y)}" for x, y in pts) + "Z"


def arrow(g, x0, y0, x1, y1, h=3.0, role="L"):
    """Shaft (x0,y0)->(x1,y1) with an open 90-degree head of leg `h` at the end."""
    ang = math.atan2(y1 - y0, x1 - x0)
    pts = []
    for s in (1, -1):
        a = ang + math.pi + s * math.pi / 4
        pts.append((x1 + h * math.cos(a), y1 + h * math.sin(a)))
    pts = [tuple(round(c * 4) / 4 for c in p) for p in pts]
    return [(role, g.poly([(x0, y0), (x1, y1)])), (role, g.poly([pts[0], (x1, y1), pts[1]]))]


def chevron(g, x, y, d, h=3.0, role="L"):
    """Open head at (x, y) pointing d in 'l r u d'."""
    if d == "r":
        pts = [(x - h, y - h), (x, y), (x - h, y + h)]
    elif d == "l":
        pts = [(x + h, y - h), (x, y), (x + h, y + h)]
    elif d == "u":
        pts = [(x - h, y + h), (x, y), (x + h, y + h)]
    else:
        pts = [(x - h, y - h), (x, y), (x + h, y - h)]
    return (role, g.poly(pts))


def cursor(g, x=4.5, y=4.5, s=1.0, role="S"):
    """Selection arrow (the select glyph) with its tip at (x, y), scaled by s."""
    base = [(0, 0), (15, 5.1), (9.65, 7.15), (15.05, 12.55), (12.55, 15.05), (7.15, 9.65),
            (5.1, 15)]
    return (role, g.poly([(x + px * s, y + py * s) for px, py in base], True))


def page(g, x0=4.75, y0=2.75, x1=19.25, y1=21.25, f=4.5):
    """Document with a folded top-right corner."""
    return [("S", g.poly([(x0, y0), (x1 - f, y0), (x1, y0 + f), (x1, y1), (x0, y1)], True)),
            ("D", g.p(("M", x1 - f, y0), ("V", y0 + f), ("H", x1)))]


def table(g, x0, y0, x1, y1, rows=(), cols=(), r=1):
    out = [("S", g.rect(x0, y0, x1, y1, r))]
    for y in rows:
        out.append(("D", g.p(("M", x0, y), ("H", x1))))
    for x in cols:
        out.append(("D", g.p(("M", x, y0), ("V", y1))))
    return out


def eye(g, cx=12, cy=12, w=9.25, h=6.5, pupil=2.75):
    """Almond eye centred at (cx, cy): two arcs and a pupil."""
    x0, x1 = cx - w, cx + w
    rr = (w * w + h * h) / (2 * h)
    lid = g.p(("M", x0, cy), ("A", rr, 0, 1, x1, cy), ("A", rr, 0, 1, x0, cy), ("Z",))
    return [("S", lid), ("D", g.circ(cx - pupil, cy - pupil, cx + pupil, cy + pupil))]


def lock(g, x0=5.75, y0=10.75, x1=18.25, y1=20.25, sh=3.5):
    cx = (x0 + x1) / 2
    return [("S", g.rect(x0, y0, x1, y1, 1)),
            ("L", g.p(("M", cx - sh, y0), ("V", y0 - 2.5), ("A", sh, 0, 1, cx + sh, y0 - 2.5),
                      ("V", y0))),
            ("D", g.p(("M", cx, y0 + 3.5), ("V", y1 - 3.5)))]


def magnifier(g, x0=3.75, y0=3.75, x1=16.25, y1=16.25, tail=20.75):
    return [("S", g.circ(x0, y0, x1, y1)),
            ("L", g.poly([(x1 - (x1 - x0) * 0.146, y1 - (y1 - y0) * 0.146), (tail, tail)]))]


def gear(g, cx=12, cy=12, R=8.5, r=6.25, hole=2.75, n=8):
    """Gear with n square-ish teeth; the hole is a detail."""
    pts = []
    for i in range(n * 2):
        a0 = math.pi * 2 * i / (n * 2) - math.pi / 2 - math.pi / (n * 2)
        a1 = a0 + math.pi * 2 / (n * 2)
        rad = R if i % 2 == 0 else r
        for a in (a0 + 0.12, a1 - 0.12) if i % 2 == 0 else (a0, a1):
            pts.append((cx + rad * math.cos(a), cy + rad * math.sin(a)))
    sx = [(round(g.sx(x), 3), round(g.sy(y), 3)) for x, y in pts]
    return [("S", g_poly(sx)), ("D", g.circ(cx - hole, cy - hole, cx + hole, cy + hole))]


def pencil(g, x0, y0, x1, y1, t=2.25):
    """Pencil along the 45-degree diagonal from tip (x0, y0) to the end (x1, y1)."""
    ang = math.atan2(y1 - y0, x1 - x0)
    nx, ny = -math.sin(ang) * t, math.cos(ang) * t
    k = 3.0
    tx, ty = x0 + math.cos(ang) * k, y0 + math.sin(ang) * k
    pts = [(x0, y0), (tx + nx, ty + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (tx - nx, ty - ny)]
    return [("S", g.poly(pts, True)), ("D", g.poly([(tx + nx, ty + ny), (tx - nx, ty - ny)]))]


def clock(g, x0=3.75, y0=3.75, x1=20.25, y1=20.25):
    cx, cy = (x0 + x1) / 2, (y0 + y1) / 2
    return [("S", g.circ(x0, y0, x1, y1)),
            ("D", g.p(("M", 11.75, 7.75), ("V", 12.75), ("H", 15.25)))]


def folder(g, x0=2.75, y0=4.75, x1=21.25, y1=19.25):
    return [("S", g.p(("M", x0, y0 + 1), ("A", 1, 0, 1, x0 + 1, y0), ("H", 8.75), ("L", 10.75, 7.25),
                      ("H", x1 - 1), ("A", 1, 0, 1, x1, 8.25), ("V", y1 - 1),
                      ("A", 1, 0, 1, x1 - 1, y1), ("H", x0 + 1), ("A", 1, 0, 1, x0, y1 - 1),
                      ("Z",)))]


def refresh(g, cx=12, cy=12, R=7.5, h=2.5):
    """Two-arrow cycle around (cx, cy)."""
    out = []
    for a0, a1 in ((200, 340), (20, 160)):
        p0 = (cx + R * math.cos(math.radians(a0)), cy + R * math.sin(math.radians(a0)))
        p1 = (cx + R * math.cos(math.radians(a1)), cy + R * math.sin(math.radians(a1)))
        out.append(("L", g.p(("M",) + p0, ("A", R, 0, 1) + p1)))
        t = math.radians(a1 + 90)
        bx, by = p1
        hd = [(bx - h * math.cos(t - math.pi / 4), by - h * math.sin(t - math.pi / 4)), p1,
              (bx - h * math.cos(t + math.pi / 4), by - h * math.sin(t + math.pi / 4))]
        out.append(("L", g.poly(hd)))
    return out


def px_circ(x0, y0, x1, y1):
    """Circle in target pixel space (for explicit 16 px redraws)."""
    cy, r = (y0 + y1) / 2, (x1 - x0) / 2
    return (f"M{fmt(x0)} {fmt(cy)}A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x1)} {fmt(cy)}"
            f"A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x0)} {fmt(cy)}Z")


def ring(g, cx, cy, R):
    """Circle by centre and radius on the free grid (no edge snapping)."""
    x, y, r = g.sx(cx), g.sy(cy), g.r(R)
    return (f"M{fmt(x-r)} {fmt(y)}A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x+r)} {fmt(y)}"
            f"A{fmt(r)} {fmt(r)} 0 1 1 {fmt(x-r)} {fmt(y)}Z")


# ---------------------------------------------------------------- badges (top-right 7 x 7)
# Zone centrelines 15.75..21.25 x 2.75..8.25; the base glyph moves to BASE_BOX.

BASE_BOX = (2, 5, 19, 22)


def badge(g, kind):
    """Modifier mark in the top-right zone; every element goes to the badge layer."""
    cx, cy = 18.5, 5.5
    if kind == "plus":
        els = [("L", g.p(("M", 15.75, 5.75), ("H", 21.25))),
               ("L", g.p(("M", 18.75, 2.75), ("V", 8.25)))]
    elif kind == "minus":
        els = [("L", g.p(("M", 15.75, 5.75), ("H", 21.25)))]
    elif kind == "x":
        els = [("L", g.poly([(16, 3), (21, 8)])), ("L", g.poly([(21, 3), (16, 8)]))]
    elif kind == "tick":
        els = [("L", g.poly([(15.25, 5.75), (17.25, 7.75), (21.25, 3.75)]))]
    elif kind == "out":
        els = [("L", g.poly([(15.75, 8.25), (21.25, 2.75)])),
               ("L", g.p(("M", 16.75, 2.75), ("H", 21.25), ("V", 7.25)))]
    elif kind == "in":
        els = [("L", g.poly([(21.25, 2.75), (15.75, 8.25)])),
               ("L", g.p(("M", 15.75, 3.75), ("V", 8.25), ("H", 20.25)))]
    elif kind == "up":
        els = [("L", g.p(("M", 18.75, 8.25), ("V", 2.75))), chevron(g, 18.75, 2.75, "u", 2.5)]
    elif kind == "down":
        els = [("L", g.p(("M", 18.75, 2.75), ("V", 8.25))), chevron(g, 18.75, 8.25, "d", 2.5)]
    elif kind == "spark":
        els = [("A", g.spark(cx, cy, 3.75))]
    elif kind == "pencil":
        els = pencil(g, 15.5, 8.5, 21, 3, 1.5)
        els = [("S", els[0][1])]
    elif kind == "gear":
        els = [gear(g, cx, cy, 2.75, 2.0, 0, 6)[0]]
    elif kind == "refresh":
        els = refresh(g, cx, cy, 3, 1.75)
    elif kind == "cursor":
        els = [cursor(g, 15.75, 2.75, 0.42, "K")]
    elif kind == "eye":
        els = [("L", g.p(("M", 15.75, 5.5), ("A", 4, 0, 1, 21.25, 5.5), ("A", 4, 0, 1, 15.75, 5.5))),
               ("K", g.dot(18.5, 5.5))]
    elif kind == "lock":
        els = [("S", g.rect(15.75, 5.25, 21.25, 8.25)),
               ("L", g.p(("M", 17, 5.25), ("V", 4.25), ("A", 1.5, 0, 1, 20, 4.25), ("V", 5.25)))]
    elif kind == "dot":
        els = [("K", ring(g, cx, cy, 2.25))]
    elif kind == "cube":
        # 2:1 hexagon; vertical edges on the badge keylines 15.75 / 21.25
        els = [("L", g.poly([(18.5, 2.75), (21.25, 4.125), (21.25, 6.875), (18.5, 8.25),
                             (15.75, 6.875), (15.75, 4.125)], close=True))]
    elif kind == "question":
        els = [("L", g.p(("M", 16.25, 4.25), ("A", 2.25, 0, 1, 20.75, 4.25),
                         ("Q", 20.75, 5.75, 18.5, 6.5))), ("K", g.dot(18.5, 8.75))]
    elif kind == "link":
        els = [("L", g.poly([(16, 8), (21, 3)])), ("L", g.poly([(15.75, 5.75), (15.75, 8.25),
                                                               (18.25, 8.25)])),
               ("L", g.poly([(18.75, 2.75), (21.25, 2.75), (21.25, 5.25)]))]
    elif kind == "clock":
        els = [("L", ring(g, cx, cy, 3.25)), ("L", g.poly([(18.5, 3.75), (18.5, 5.5), (20, 5.5)]))]
    else:
        raise KeyError(kind)
    return [("b" + r, d) for r, d in els]


def badged(base, kind, box=BASE_BOX):
    """Glyph function: `base` shrunk into the bottom-left box plus a `kind` badge."""
    def fn(g):
        return base(g.sub(*box)) + badge(g, kind)
    return fn
