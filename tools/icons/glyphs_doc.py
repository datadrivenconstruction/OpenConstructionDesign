"""Annotation, sheets, files and exchange, project data and app chrome glyphs."""
import math

from core import (V, arrow, badge, badged, chevron, cursor, folder, fmt, g_poly, gear, icon, lock,
                  model, page, pencil, ring, table)
import core
import glyphs_base as B


def bk(els):
    """Elements to the badge layer."""
    return [("b" + r, d) for r, d in els]


def lines(g, x0, x1, ys, role="D"):
    return [(role, g.p(("M", x0, y), ("H", x1))) for y in ys]


def tri_badge(g):
    """Delta (change) mark in the badge zone."""
    return bk([("L", g.poly([(18.5, 2.75), (21.25, 8.25), (15.75, 8.25)], True))])


NX, NY = 12.75, 11.75  # notch corner: base strokes stay clear of the badge halo


def notch(g, x0, y0, x1, y1, nx=NX, ny=NY, r=1):
    """Rectangle with its top-right corner cut away for a badge."""
    return g.p(("M", x0 + r, y0), ("H", nx), ("V", ny), ("H", x1), ("V", y1 - r),
               ("A", r, 0, 1, x1 - r, y1), ("H", x0 + r), ("A", r, 0, 1, x0, y1 - r),
               ("V", y0 + r), ("A", r, 0, 1, x0 + r, y0), ("Z",))


def cube_badge(g):
    return bk([("L", g.poly([(18.5, 2.75), (21.25, 4.125), (21.25, 6.875), (18.5, 8.25),
                             (15.75, 6.875), (15.75, 4.125)], True)),
               ("L", g.poly([(15.75, 4.125), (18.5, 5.5), (21.25, 4.125)]))])


def gear_badge(g):
    return bk([gear(g, 18.5, 5.5, 2.75, 1.9, 0, 6)[0]])


def ngrid(g):
    """Spreadsheet grid with the top-right cell left for a badge."""
    return [("S", notch(g, 2.75, 6.75, 17.25, 20.25)),
            ("D", g.p(("M", 2.75, 11.75), ("H", 12.75))), ("D", g.p(("M", 2.75, 16.25), ("H", 17.25))),
            ("D", g.p(("M", 7.75, 6.75), ("V", 20.25))), ("D", g.p(("M", 12.75, 11.75), ("V", 20.25)))]


def frame(g, top=None):
    """App window frame, optionally with a top band."""
    out = [("S", g.rect(2.75, 3.75, 21.25, 20.25, 1))]
    if top is not None:
        out.append(("D", g.p(("M", 2.75, top), ("H", 21.25))))
    return out


def bubble(g, x0, y0, x1, y1, tx, r=2):
    """Speech bubble with a 45-degree tail from the bottom edge at tx (tail to the lower left)."""
    return g.p(("M", x0 + r, y0), ("H", x1 - r), ("A", r, 0, 1, x1, y0 + r), ("V", y1 - r),
               ("A", r, 0, 1, x1 - r, y1), ("H", tx + 3), ("L", tx, y1 + 3), ("V", y1),
               ("H", x0 + r), ("A", r, 0, 1, x0, y1 - r), ("V", y0 + r), ("A", r, 0, 1, x0 + r, y0),
               ("Z",))


def capsule(g, x0, y0, x1, y1, r):
    """Stadium around the segment (x0, y0)-(x1, y1)."""
    a = math.atan2(y1 - y0, x1 - x0)
    nx, ny = -math.sin(a) * r, math.cos(a) * r
    p = [(x0 + nx, y0 + ny), (x1 + nx, y1 + ny), (x1 - nx, y1 - ny), (x0 - nx, y0 - ny)]
    q = [(g.sx(x), g.sy(y)) for x, y in p]
    rr = g.r(r)
    return (f"M{fmt(q[0][0])} {fmt(q[0][1])}L{fmt(q[1][0])} {fmt(q[1][1])}"
            f"A{fmt(rr)} {fmt(rr)} 0 0 0 {fmt(q[2][0])} {fmt(q[2][1])}"
            f"L{fmt(q[3][0])} {fmt(q[3][1])}A{fmt(rr)} {fmt(rr)} 0 0 0 {fmt(q[0][0])} {fmt(q[0][1])}Z")


def solid_head(g, tx, ty, dx, dy, ln=3.0, hw=1.5):
    """Solid annotation head with its tip at (tx, ty) pointing along (dx, dy)."""
    n = math.hypot(dx, dy)
    ux, uy = dx / n, dy / n
    bx, by = tx - ux * ln, ty - uy * ln
    return ("K", g.poly([(tx, ty), (bx - uy * hw, by + ux * hw), (bx + uy * hw, by - ux * hw)], True))


# ---------------------------------------------------------------- annotation

@icon("text", "Text")
def text(g):
    return [("L", g.p(("M", 4.75, 4.75), ("H", 19.25))),
            ("L", g.p(("M", 11.75, 4.75), ("V", 19.25))),
            ("L", g.p(("M", 8.75, 19.25), ("H", 14.75)))]


@icon("leader", "Leader")
def leader(g):
    return [("L", g.p(("M", 12.75, 4.75), ("H", 21.25))),
            ("L", g.p(("M", 12.75, 8.25), ("H", 18.25))),
            ("L", g.poly([(6.75, 17.25), (12.25, 11.75), (21.25, 11.75)])),
            solid_head(g, 3.75, 20.25, -1, 1, 3.5, 1.75)]


@icon("detail-line", "Detail line")
def detail_line(g):
    return [("S", g.rect(2.75, 17.75, 6.25, 21.25)), ("S", g.rect(17.75, 2.75, 21.25, 6.25)),
            ("L", g.poly([(6.25, 17.75), (17.75, 6.25)]))]


@icon("spot-elev", "Spot elev")
def spot_elev(g):
    return [("L", g.p(("M", 2.75, 20.25), ("H", 13.25))),
            ("K", g.poly([(4.75, 17.25), (10.75, 17.25), (7.75, 20.25)], True)),
            ("L", g.poly([(10.75, 17.25), (16.25, 11.75)])),
            ("S", g.rect(14.75, 3.75, 21.25, 11.75, 1))]


@icon("spot-coord", "Spot coord")
def spot_coord(g):
    return [("S", g.circ(3.75, 14.75, 9.25, 20.25)), ("K", g.dot(6.5, 17.5)),
            ("L", g.poly([(8.5, 15.5), (12.25, 11.75)])),
            ("S", g.rect(11.75, 3.75, 21.25, 11.75, 1)),
            ("D", g.p(("M", 14.75, 7.75), ("H", 18.25)))]


@icon("dim-angular", "Dim angular")
def dim_angular(g):
    return [("L", g.poly([(21.25, 20.25), (3.75, 20.25), (16.25, 7.75)])),
            ("L", g.p(("M", 15.75, 20.25), ("A", 12, 0, 0, 12.24, 11.76))),
            solid_head(g, 15.75, 19.75, 0, 1, 2.5, 1.5)]


@icon("dim-radial", "Dim radial")
def dim_radial(g):
    return [("L", g.circ(2.75, 2.75, 21.25, 21.25)), ("K", g.dot(12, 12)),
            ("L", g.poly([(12, 12), (16.4, 7.6)])),
            solid_head(g, 18.54, 5.46, 1, -1, 3.25, 1.75)]


@icon("dim-chain", "Dim chain")
def dim_chain(g):
    y = V(8.75, 7.25, 5.5)
    out = [("S", g.rect(2.75, 15.75, 6.25, 20.25)), ("S", g.rect(10.75, 15.75, 13.25, 20.25)),
           ("S", g.rect(17.75, 15.75, 21.25, 20.25)),
           ("L", g.p(("M", 2.75, y), ("H", 21.25)))]
    yy = g.sy(y)
    for x in (4.75, 11.75, 19.75):
        out.append(("L", g.p(("M", x, V(4.75, 3.75, 2.5)), ("V", 15.75))))
        xx, tt = g.sx(x), 1.5 * g.k
        out.append(("L", f"M{fmt(xx-tt)} {fmt(yy+tt)}L{fmt(xx+tt)} {fmt(yy-tt)}"))
    return out


@icon("dim-auto", "Dim auto")
def dim_auto(g):
    y = V(7.75, 6.25, 4.5)
    out = [("S", g.rect(2.75, 14.75, 21.25, 20.25)),
           ("D", g.p(("M", 9.75, 14.75), ("V", 20.25))), ("D", g.p(("M", 14.25, 14.75), ("V", 20.25))),
           ("L", g.p(("M", 2.75, y), ("H", 21.25)))]
    yy = g.sy(y)
    for x in (3.75, 9.75, 14.25, 20.25):
        out.append(("L", g.p(("M", x, V(3.75, 2.75, 2.5)), ("V", 14.75))))
        xx, tt = g.sx(x), 1.25 * g.k
        out.append(("L", f"M{fmt(xx-tt)} {fmt(yy+tt)}L{fmt(xx+tt)} {fmt(yy-tt)}"))
    return out


@icon("distribute", "Distribute")
def distribute(g):
    out = [("S", g.rect(2.75, 5.75, 6.25, 12.25)), ("S", g.rect(9.75, 5.75, 13.25, 12.25)),
           ("S", g.rect(16.75, 5.75, 20.25, 12.25)),
           ("L", g.p(("M", 4.75, 18.25), ("H", 18.75)))]
    for x in (4.75, 11.75, 18.75):
        out.append(("L", g.p(("M", x, 14.75), ("V", 21.25))))
    return out


@icon("tag-all", "Tag all")
def tag_all(g):
    return [("L", g.p(("M", 10.75, 2.75), ("H", 20.25), ("A", 1, 0, 1, 21.25, 3.75), ("V", 9.25))),
            ("S", g.rect(6.75, 6.75, 17.25, 14.25, 1)),
            ("D", g.p(("M", 9.75, V(10.75, 9.75, 7.5)), ("H", 14.25))),
            ("L", g.poly([(9.25, 14.25), (5.25, 18.25)])),
            ("K", g.circ(2.75, 17.75, 5.75, 20.75))]


@icon("detail-comp", "Detail comp")
def detail_comp(g):
    return [("S", g.rect(2.75, 6.75, 21.25, 17.25)),
            ("H", g.poly([(3.75, 15.25), (10.25, 8.75), (16.75, 15.25), (20.25, 11.75)]))]


@icon("region", "Region")
def region(g):
    return [("S", g.poly([(3.75, 10.25), (10.25, 3.75), (20.25, 3.75), (20.25, 20.25), (3.75, 20.25)],
                         True)),
            ("H", g.poly([(7, 7), (20.25, 20.25)])), ("H", g.poly([(11.75, 3.75), (20.25, 12.25)])),
            ("H", g.poly([(3.75, 11.75), (12.25, 20.25)]))]


def cloud_path(g, x0=4.75, y0=6.75, x1=19.25, y1=17.25, nx=4, ny=3):
    sx, sy = (x1 - x0) / nx, (y1 - y0) / ny
    t = [("M", x0, y0)]
    for i in range(1, nx + 1):
        t.append(("A", sx / 2, 0, 1, x0 + sx * i, y0))
    for i in range(1, ny + 1):
        t.append(("A", sy / 2, 0, 1, x1, y0 + sy * i))
    for i in range(1, nx + 1):
        t.append(("A", sx / 2, 0, 1, x1 - sx * i, y1))
    for i in range(1, ny + 1):
        t.append(("A", sy / 2, 0, 1, x0, y1 - sy * i))
    t.append(("Z",))
    return g.p(*t)


@icon("cloud", "Cloud")
def cloud(g):
    return [("S", cloud_path(g))]


@icon("revision-issue", "Revision issue")
def revision_issue(g):
    return [("S", cloud_path(g, 5.25, 10.75, 15.75, 18.25, 3, 2))] + badge(g, "tick")


def legend_g(g):
    out = []
    for y0, y1, ly in ((3.75, 7.25, 5.75), (10.25, 13.75, 11.75), (16.75, 20.25, 18.25)):
        out.append(("S", g.rect(3.75, y0, 7.25, y1)))
        out.append(("L", g.p(("M", 10.75, ly), ("H", 20.25))))
    return out


@icon("legend", "Legend")
def legend(g):
    return legend_g(g)


@icon("legend-place", "Legend place")
def legend_place(g):
    return [("S", g.rect(2.75, 4.75, 21.25, 19.25, 1)),
            ("S", g.rect(5.75, 7.75, 8.75, 10.75)), ("D", g.p(("M", 11.25, 9.25), ("H", 18.25))),
            ("S", g.rect(5.75, 13.25, 8.75, 16.25)), ("D", g.p(("M", 11.25, 14.75), ("H", 18.25)))]


@icon("numbering", "Numbering")
def numbering(g):
    return [("L", g.poly([(9.75, 3.75), (7.75, 20.25)])), ("L", g.poly([(16.25, 3.75), (14.25, 20.25)])),
            ("L", g.p(("M", 3.75, 8.75), ("H", 20.25))), ("L", g.p(("M", 3.75, 15.25), ("H", 20.25)))]


# ---------------------------------------------------------------- sheets and views

def sheet_at(g, x0, y0, x1, y1, tb=True):
    out = [("S", g.rect(x0, y0, x1, y1, 1))]
    if tb:
        out.append(("D", g.p(("M", x1 - 6.5, y1), ("V", y1 - 4.5), ("H", x1))))
    return out


@icon("sheet-duplicate", "Sheet duplicate")
def sheet_duplicate(g):
    return ([("L", g.p(("M", 6.75, 8.75), ("V", 4.75), ("H", 21.25), ("V", 15.25), ("H", 17.25)))]
            + sheet_at(g, 2.75, 8.75, 17.25, 20.25, False)
            + [("D", g.rect(5.75, 11.75, 11.25, 17.25))])


@icon("sheet-duplicate-title", "Sheet duplicate title")
def sheet_duplicate_title(g):
    return ([("L", g.p(("M", 6.75, 8.75), ("V", 4.75), ("H", 21.25), ("V", 15.25), ("H", 17.25)))]
            + sheet_at(g, 2.75, 8.75, 17.25, 20.25, False)
            + [("D", g.p(("M", 2.75, 16.25), ("H", 17.25))), ("D", g.p(("M", 9.75, 16.25), ("V", 20.25)))])


@icon("sheet-list", "Sheet list")
def sheet_list(g):
    out = sheet_at(g, 2.75, 3.75, 21.25, 20.25, False)
    for y in (8.25, 12.25, 16.25):
        out.append(("K", g.dot(6.5, y)))
        out.append(("D", g.p(("M", 9.75, y), ("H", 17.75))))
    return out


@icon("sheet-set", "Sheet set")
def sheet_set(g):
    return ([("L", g.p(("M", 10.75, 6.75), ("V", 2.75), ("H", 21.25), ("V", 13.25), ("H", 17.25))),
             ("L", g.p(("M", 6.75, 10.75), ("V", 6.75), ("H", 17.25), ("V", 17.25), ("H", 13.25)))]
            + sheet_at(g, 2.75, 10.75, 13.25, 21.25, False))


@icon("sheets-levels", "Sheets levels")
def sheets_levels(g):
    return (sheet_at(g, 2.75, 8.75, 15.25, 20.25)
            + bk([("K", g.poly([(16.25, 2.75), (20.75, 2.75), (18.5, 5.75)], True)),
                  ("L", g.p(("M", 15.75, 8.25), ("H", 21.25)))]))


@icon("drawing-set", "Drawing set")
def drawing_set(g):
    return ([("L", g.p(("M", 8.75, 9.75), ("V", 2.75), ("H", 17.25), ("L", 21.25, 6.75), ("V", 15.25),
                       ("H", 16.25))),
             ("D", g.p(("M", 17.25, 2.75), ("V", 6.75), ("H", 21.25)))]
            + sheet_at(g, 2.75, 9.75, 16.25, 21.25))


@icon("place-view", "Place view")
def place_view(g):
    return (sheet_at(g, 2.75, 7.75, 17.25, 21.25)
            + [("S", g.rect(5.75, 10.75, 10.25, 15.25))] + badge(g, "in"))


def drafting(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 20.25, 1))] + pencil(g, 7, 17, 17.5, 6.5, 2)


@icon("drafting-new", "Drafting new")
def drafting_new(g):
    return ([("S", notch(g, 2.75, 3.75, 21.25, 20.25))] + pencil(g, 6, 18, 12.5, 11.5, 2)
            + badge(g, "plus"))


@icon("drafting-open", "Drafting open")
def drafting_open(g):
    return ([("L", g.p(("M", 6.75, 6.75), ("V", 2.75), ("H", 21.25), ("V", 17.25), ("H", 17.25)))]
            + [("S", g.rect(2.75, 6.75, 17.25, 21.25, 1))] + pencil(g, 6.5, 18, 13.5, 11, 1.75))


@icon("to-detail", "To detail")
def to_detail(g):
    return [("S", g.rect(11.75, 11.75, 21.25, 20.25)),
            ("H", g.poly([(12.75, 18.25), (16.25, 14.75), (19.75, 18.25)])),
            ("L", g.poly([(2.75, 11.25), (2.75, 2.75), (11.25, 2.75)])),
            ("L", g.poly([(5.75, 5.75), (9.25, 9.25)])),
            ("L", g.p(("M", 9.25, 5.75), ("V", 9.25), ("H", 5.75)))]


@icon("table", "Table")
def table_(g):
    return table(g, 2.75, 3.75, 21.25, 20.25, rows=(8.75, 14.25), cols=(9.75,))


@icon("table-open", "Table open")
def table_open(g):
    return [("S", notch(g, 2.75, 3.75, 21.25, 20.25, ny=13.75)),
            ("D", g.p(("M", 2.75, 7.75), ("H", 12.75))), ("D", g.p(("M", 2.75, 16.25), ("H", 21.25))),
            ("D", g.p(("M", 7.75, 3.75), ("V", 20.25)))] + badge(g, "out")


# ---------------------------------------------------------------- files and exchange

def doc(g):
    return page(g, 4.75, 2.75, 19.25, 21.25, 4.5)


def cad_page(g):
    return doc(g) + [("D", g.circ(7.75, 12.75, 13.25, 18.25)),
                     ("D", g.poly([(9.5, 9.5), (16.25, 16.25)]))]


def pdf_page(g):
    return doc(g) + [("D", g.poly([(7.75, 18.25), (10.75, 14.25), (13.25, 16.75), (14.75, 15.25),
                                   (16.25, 18.25)])),
                     ("KX", g.dot(9.75, 9.75))]


def grid_sheet(g):
    return table(g, 3.75, 3.75, 20.25, 20.25, rows=(9.25, 14.75), cols=(9.25, 14.75))


@icon("file-new", "File new")
def file_new(g):
    return badged(doc, "plus")(g)


@icon("pdf-out", "Pdf out")
def pdf_out(g):
    return badged(pdf_page, "out")(g)


@icon("import-pdf", "Import pdf")
def import_pdf(g):
    return badged(pdf_page, "in")(g)


@icon("dxf-out", "Dxf out")
def dxf_out(g):
    return badged(cad_page, "out")(g)


@icon("import-cad", "Import cad")
def import_cad(g):
    return badged(cad_page, "in")(g)


@icon("reimport", "Reimport")
def reimport(g):
    return badged(cad_page, "refresh")(g)


@icon("source", "Source")
def source(g):
    return doc(g) + [("L", g.poly([(10.25, 11.25), (7.75, 13.75), (10.25, 16.25)])),
                     ("L", g.poly([(13.75, 11.25), (16.25, 13.75), (13.75, 16.25)]))]


@icon("xlsx-out", "Xlsx out")
def xlsx_out(g):
    return ngrid(g) + badge(g, "out")


@icon("xlsx-in", "Xlsx in")
def xlsx_in(g):
    return ngrid(g) + badge(g, "in")


@icon("xlsx-sync", "Xlsx sync")
def xlsx_sync(g):
    return ngrid(g) + badge(g, "refresh")


@icon("xlsx-delta", "Xlsx delta")
def xlsx_delta(g):
    return ngrid(g) + tri_badge(g)


@icon("gltf-out", "Gltf out")
def gltf_out(g):
    c, _top = core.cube(g)
    hex_ = c[0][1]
    a, b = g.sx(2.75), g.sx(14.75)
    cx, hw = (a + b) / 2, (b - a) / 2
    yt = g.sy(8.75)
    hh = round(6.5 * g.k * 2) / 2
    ys = yt + hw / 2
    mid = (cx, yt + hw / 2 + hh / 2)
    spokes = "".join(f"M{fmt(mid[0])} {fmt(mid[1])}L{fmt(x)} {fmt(y)}"
                     for x, y in ((cx, yt), (b, ys + hh), (a, ys + hh)))
    return [("SF", hex_), ("D", spokes),
            ("L", g.p(("M", 16.75, 15.25), ("H", 21.25))),
            ("L", g.poly([(18.75, 12.75), (21.25, 15.25), (18.75, 17.75)]))]


def pips(g, n):
    """Schema generation mark: n solid pips along the top, right aligned (2x3 1, 4 2, 4x3 3)."""
    return [("K", g.rect(x, 2.75, x + 3.5, 6.25)) for x in (17.75, 12.75, 7.75)[:n]]


@icon("ifc-out-4", "Ifc out 4")
def ifc_out_4(g):
    return B.ifc_out(g) + pips(g, 2)


@icon("ifc-out-4x3", "Ifc out 4x3")
def ifc_out_4x3(g):
    return B.ifc_out(g) + pips(g, 3)


@icon("ifc-out-2x3", "Ifc out 2x3")
def ifc_out_2x3(g):
    return B.ifc_out(g) + pips(g, 1)


@icon("ifc-delta", "Ifc delta")
def ifc_delta(g):
    return model(g) + tri_badge(g)


@icon("deliver", "Deliver")
def deliver(g):
    return [("S", g.poly([(21.25, 2.75), (2.75, 11.75), (9.75, 14.25), (12.25, 21.25)], True)),
            ("D", g.poly([(9.75, 14.25), (21.25, 2.75)]))]


@icon("print", "Print")
def print_(g):
    return [("L", g.p(("M", 6.75, 8.75), ("V", 3.75), ("H", 17.25), ("V", 8.75))),
            ("S", g.rect(2.75, 8.75, 21.25, 16.25, 1)),
            ("S", g.rect(6.75, 13.75, 17.25, 20.25)),
            ("KX", g.dot(17.75, 11.75))]


@icon("import", "Import")
def import_(g):
    return [("L", g.p(("M", 2.75, 14.75), ("V", 20.25), ("H", 21.25), ("V", 14.75))),
            ("L", g.p(("M", 11.75, 3.75), ("V", 14.75))), chevron(g, 11.75, 14.75, "d", 4)]


def floppy(g):
    return [("S", g.poly([(3.75, 3.75), (16.75, 3.75), (20.25, 7.25), (20.25, 20.25), (3.75, 20.25)],
                         True)),
            ("D", g.p(("M", 7.75, 3.75), ("V", V(8.25, 7.25, 5.5)), ("H", 14.25), ("V", 3.75))),
            ("D", g.circ(8.75, 11.75, 15.25, 18.25))]


@icon("save", "Save")
def save(g):
    return floppy(g)


@icon("save-as", "Save as")
def save_as(g):
    return badged(floppy, "pencil")(g)


@icon("autosave", "Autosave")
def autosave(g):
    return badged(floppy, "clock")(g)


def open_folder(g):
    return folder(g) + [("D", g.p(("M", 2.75, 10.75), ("H", 21.25)))]


@icon("open", "Open")
def open_(g):
    return open_folder(g)


@icon("open-big", "Open big")
def open_big(g):
    return [("S", g.p(("M", 2.75, 5.75), ("A", 1, 0, 1, 3.75, 4.75), ("H", 8.25), ("L", 10.25, 6.75),
                      ("H", 12.75), ("V", 13.75), ("H", 21.25), ("V", 18.25), ("A", 1, 0, 1, 20.25, 19.25),
                      ("H", 3.75), ("A", 1, 0, 1, 2.75, 18.25), ("Z",))),
            ("D", g.p(("M", 2.75, 9.75), ("H", 12.75)))] + cube_badge(g)


@icon("sample", "Sample")
def sample(g):
    return [("S", g.p(("M", 9.75, 3.75), ("V", 8.75), ("L", 4, 20.25), ("H", 20), ("L", 14.25, 8.75),
                      ("V", 3.75), ("Z",))),
            ("L", g.p(("M", 7.75, 3.75), ("H", 16.25))),
            ("D", g.p(("M", 6.5, 15.25), ("H", 17.5)))]


@icon("history", "History")
def history(g):
    return [("L", g.p(("M", 3.75, 12), ("A", 8.25, 1, 1, 6.17, 17.83))),
            ("L", g.poly([(9.25, 17.75), (6.25, 17.75), (6.25, 20.75)])),
            ("L", g.p(("M", 11.75, 7.75), ("V", 12.25), ("H", 15.25)))]


def two_pane(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 20.25, 1)), ("D", g.p(("M", 11.75, 3.75), ("V", 20.25))),
            ("D", g.p(("M", 5.75, 8.75), ("H", 8.75))), ("D", g.p(("M", 5.75, 12.75), ("H", 8.75))),
            ("D", g.p(("M", 14.75, 8.75), ("H", 18.25))),
            ("K", g.rect(14.75, 12.25, 18.25, 13.75))]


@icon("diff", "Diff")
def diff(g):
    return two_pane(g)


@icon("compare-version", "Compare version")
def compare_version(g):
    return [("S", notch(g, 2.75, 3.75, 21.25, 20.25, 11.75, 13.75)),
            ("D", g.p(("M", 11.75, 13.75), ("V", 20.25))),
            ("D", g.p(("M", 5.75, 8.75), ("H", 8.75))), ("D", g.p(("M", 5.75, 12.75), ("H", 8.75))),
            ("K", g.rect(14.75, 14.75, 18.25, 16.25))] + badge(g, "clock")


def status_box(g):
    return [("L", g.rect(2.75, 2.75, 21.25, 21.25, 2))]


@icon("diff-added", "Diff added")
def diff_added(g):
    return status_box(g) + [("L", g.p(("M", 6.75, 11.75), ("H", 17.25))),
                            ("L", g.p(("M", 11.75, 6.75), ("V", 17.25)))]


@icon("diff-changed", "Diff changed")
def diff_changed(g):
    return status_box(g) + [("K", ring(g, 12, 12, 3.25))]


@icon("diff-deleted", "Diff deleted")
def diff_deleted(g):
    return status_box(g) + [("L", g.p(("M", 6.75, 11.75), ("H", 17.25)))]


@icon("diff-refused", "Diff refused")
def diff_refused(g):
    return [("S", g.circ(3.75, 3.75, 20.25, 20.25)), ("D", g.poly([(6.2, 6.2), (17.8, 17.8)]))]


def merge(g):
    return [("L", g.poly([(5.75, 3.75), (11.75, 15.75)])), ("L", g.poly([(17.75, 3.75), (11.75, 15.75)])),
            ("L", g.p(("M", 11.75, 15.75), ("V", 20.25))), chevron(g, 11.75, 20.25, "d", 3)]


@icon("merge-file", "Merge file")
def merge_file(g):
    return merge(g)


@icon("merge-conflict", "Merge conflict")
def merge_conflict(g):
    return badged(merge, "x")(g)


def chain(g):
    return [("S", capsule(g, 5.5, 18.5, 9.5, 14.5, 2.75)), ("S", capsule(g, 14.5, 9.5, 18.5, 5.5, 2.75)),
            ("L", g.poly([(9.5, 14.5), (14.5, 9.5)]))]


@icon("link", "Link")
def link(g):
    return chain(g)


@icon("link-add", "Link add")
def link_add(g):
    return badged(chain, "plus")(g)


@icon("link-reload", "Link reload")
def link_reload(g):
    return badged(chain, "refresh")(g)


@icon("links", "Links")
def links(g):
    return model(g, 2.75, 16.75, 7.75, 7.5) + badge(g, "link")


@icon("globe", "Globe")
def globe(g):
    return [("S", g.circ(2.75, 2.75, 21.25, 21.25)),
            ("D", g.p(("M", 12, 2.75), ("A", 11.5, 0, 0, 12, 21.25), ("A", 11.5, 0, 0, 12, 2.75))),
            ("D", g.p(("M", 2.75, 11.75), ("H", 21.25)))]


def circle_mark(g):
    return [("S", g.circ(2.75, 2.75, 21.25, 21.25))]


@icon("help", "Help")
def help_(g):
    return circle_mark(g) + [("D", g.p(("M", 9.25, 9.75), ("A", 2.75, 0, 1, 14.75, 9.75),
                                       ("Q", 14.75, 11.75, 12, 12.75), ("V", 14.25))),
                             ("KX", g.dot(12, 17.25))]


@icon("level-info", "Level info")
def level_info(g):
    return circle_mark(g) + [("D", g.p(("M", 11.75, 10.75), ("V", 16.75))), ("KX", g.dot(11.75, 7.25))]


@icon("level-error", "Level error")
def level_error(g):
    return circle_mark(g) + [("D", g.poly([(8.5, 8.5), (15.5, 15.5)])),
                             ("D", g.poly([(15.5, 8.5), (8.5, 15.5)]))]


@icon("level-success", "Level success")
def level_success(g):
    return circle_mark(g) + [("D", g.poly([(7.75, 12.25), (10.75, 15.25), (16.25, 9.75)]))]


@icon("warning", "Warning")
def warning(g):
    return [("S", g.p(("M", 12, 2.75), ("L", 21.25, 20.25), ("H", 2.75), ("Z",))),
            ("D", g.p(("M", 11.75, 9.75), ("V", 14.25))), ("KX", g.dot(11.75, 17.25))]


def clipboard(g):
    return [("S", g.rect(3.75, 4.75, 20.25, 21.25, 1)),
            ("L", g.rect(8.75, 2.75, 15.25, 6.25, 1)),
            ("D", g.poly([(7.75, 11.75), (9.25, 13.25), (11.75, 10.75)])),
            ("D", g.p(("M", 13.75, 11.75), ("H", 16.25))),
            ("D", g.poly([(7.75, 16.75), (9.25, 18.25), (11.75, 15.75)])),
            ("D", g.p(("M", 13.75, 16.75), ("H", 16.25)))]


@icon("issues", "Issues")
def issues(g):
    return clipboard(g)


core.alias_glyph("ai-chat", "Ai chat", B.spark)


icon("ai-review", "Ai review")(badged(lambda g: core.magnifier(g), "spark"))


@icon("spark-fix", "Spark fix")
def spark_fix(g):
    return [("S", notch(g, 2.75, 4.75, 18.25, 21.25)),
            ("L", g.rect(4.75, 2.75, 8.25, 6.25, 1)),
            ("D", g.poly([(5.75, 10.25), (7.25, 11.75), (9.75, 9.25)])),
            ("D", g.poly([(5.75, 16.25), (7.25, 17.75), (9.75, 15.25)])),
            ("D", g.p(("M", 11.75, 16.75), ("H", 15.25)))] + badge(g, "spark")


def bulb(g):
    return [("S", g.p(("M", 8.75, 15.75), ("A", 6.5, 1, 1, 15.25, 15.75), ("V", 17.25), ("H", 8.75),
                      ("Z",))),
            ("L", g.p(("M", 9.75, 20.25), ("H", 14.25)))]


@icon("tips", "Tips")
def tips(g):
    return bulb(g)


@icon("ctx-tips", "Ctx tips")
def ctx_tips(g):
    return badged(bulb, "cursor")(g)


@icon("tutorial", "Tutorial")
def tutorial(g):
    return [("S", g.poly([(4.75, 3.75), (19.25, 3.75), (15.75, 7.25), (19.25, 10.75), (4.75, 10.75)],
                         True)),
            ("L", g.p(("M", 4.75, 10.75), ("V", 21.25)))]


def book(g):
    return [("S", g.p(("M", 11.75, 6.75), ("Q", 7.75, 4.75, 2.75, 4.75), ("V", 18.25),
                      ("Q", 7.75, 18.25, 11.75, 20.25), ("Q", 15.75, 18.25, 21.25, 18.25),
                      ("V", 4.75), ("Q", 15.75, 4.75, 11.75, 6.75), ("Z",))),
            ("D", g.p(("M", 11.75, 6.75), ("V", 20.25)))]


@icon("reference", "Reference")
def reference(g):
    return book(g)


@icon("whats-new", "Whats new")
def whats_new(g):
    pts = []
    for i in range(16):
        a = math.pi * 2 * i / 16 - math.pi / 2
        r = 9.25 if i % 2 == 0 else 6.75
        pts.append((12 + r * math.cos(a), 12 + r * math.sin(a)))
    return [("S", g.poly(pts, True)), ("KX", g.dot(12, 12))]


@icon("where-is", "Where is")
def where_is(g):
    a1, a2 = math.radians(140), math.radians(40)
    p1 = (12 + 7 * math.cos(a1), 9.5 + 7 * math.sin(a1))
    p2 = (12 + 7 * math.cos(a2), 9.5 + 7 * math.sin(a2))
    return [("S", g.p(("M", 12, 21.25), ("L",) + p1, ("A", 7, 1, 1) + p2, ("Z",))),
            ("D", g.circ(9.25, 6.75, 14.75, 12.25))]


@icon("why", "Why")
def why(g):
    return model(g, 2.75, 16.75, 8.75, 5) + bk([
        ("L", g.p(("M", 16.5, 4.75), ("A", 2, 0, 1, 20.5, 4.75), ("Q", 20.5, 6.25, 18.5, 6.75))),
        ("K", g.dot(18.5, 9.25))])


@icon("keyboard", "Keyboard")
def keyboard(g):
    out = [("S", g.rect(2.75, 5.75, 21.25, 18.25, 1)), ("D", g.p(("M", 8.75, 14.75), ("H", 15.25)))]
    for y in (9.25,):
        for x in (6.5, 10, 14, 17.5):
            out.append(("KX", g.dot(x, y)))
    out.append(("KX", g.dot(6.5, 14.75)))
    out.append(("KX", g.dot(17.5, 14.75)))
    return out


@icon("palette", "Palette")
def palette(g):
    return [("S", g.rect(2.75, 3.75, 21.25, 20.25, 1)),
            ("D", g.circ(5.75, 6.75, 9.25, 10.25)), ("D", g.p(("M", 11.75, 8.75), ("H", 18.25))),
            ("D", g.p(("M", 2.75, 12.75), ("H", 21.25))),
            ("D", g.p(("M", 5.75, 16.75), ("H", 15.25)))]


def bar3(g):
    return [("S", g.rect(2.75, 8.75, 21.25, 15.25, 1)),
            ("D", g.rect(5.75, 10.75, 7.25, 13.25)), ("D", g.rect(10.75, 10.75, 13.25, 13.25)),
            ("D", g.rect(16.75, 10.75, 18.25, 13.25))]


@icon("recent-bar", "Recent bar")
def recent_bar(g):
    return [("S", g.rect(2.75, 11.75, 21.25, 18.25, 1)),
            ("D", g.p(("M", 7.25, 11.75), ("V", 18.25))), ("D", g.p(("M", 11.75, 11.75), ("V", 18.25))),
            ("D", g.p(("M", 16.25, 11.75), ("V", 18.25)))] + bk(
        [("L", ring(g, 6.5, 5.5, 3.25)), ("L", g.poly([(6.5, 3.75), (6.5, 5.5), (8, 5.5)]))])


@icon("context-menu", "Context menu")
def context_menu(g):
    return [("S", g.rect(8.75, 6.75, 21.25, 21.25, 1)),
            ("D", g.p(("M", 11.75, 10.75), ("H", 18.25))), ("D", g.p(("M", 11.75, 14.25), ("H", 18.25))),
            ("D", g.p(("M", 11.75, 17.75), ("H", 16.25))),
            cursor(g, 2.75, 2.75, 0.42, "K")]


@icon("language", "Language")
def language(g):
    return [("L", g.p(("M", 4.75, 2.75), ("H", 12.25), ("A", 2, 0, 1, 14.25, 4.75), ("V", 6.75))),
            ("L", g.p(("M", 6.75, 12.25), ("H", 4.75), ("A", 2, 0, 1, 2.75, 10.25), ("V", 4.75),
                      ("A", 2, 0, 1, 4.75, 2.75))),
            ("S", bubble(g, 6.75, 6.75, 21.25, 17.25, 15.25)),
            ("D", g.p(("M", 10.25, 10.75), ("H", 17.75))), ("D", g.p(("M", 10.25, 13.75), ("H", 15.25)))]


@icon("theme", "Theme")
def theme(g):
    return [("S", g.circ(2.75, 2.75, 21.25, 21.25)),
            ("KX", g.p(("M", 12, 2.75), ("A", 9.25, 0, 1, 12, 21.25), ("Z",)))]


@icon("settings", "Settings")
def settings(g):
    return gear(g)


@icon("units", "Units")
def units(g):
    out = [("S", g.rect(2.75, 7.75, 21.25, 16.25, 1))]
    for x, ln in ((6.75, 12.75), (9.75, 10.75), (12.75, 12.75), (15.75, 10.75), (18.75, 12.75)):
        out.append(("D", g.p(("M", x, 7.75), ("V", ln))))
    return out


# ---------------------------------------------------------------- layouts and chrome

@icon("layout-ribbon", "Layout ribbon")
def layout_ribbon(g):
    return frame(g, 9.25) + [("D", g.p(("M", 8.75, 3.75), ("V", 9.25))),
                             ("D", g.p(("M", 14.75, 3.75), ("V", 9.25)))]


@icon("layout-context", "Layout context")
def layout_context(g):
    return frame(g, 7.75) + [("D", g.rect(11.75, 11.75, 18.25, 17.25, 1))]


@icon("layout-split", "Layout split")
def layout_split(g):
    return frame(g, 7.75) + [("D", g.p(("M", 11.75, 7.75), ("V", 20.25)))]


@icon("layout-focus", "Layout focus")
def layout_focus(g):
    out = []
    for (x, y, sx, sy) in ((2.75, 3.75, 1, 1), (21.25, 3.75, -1, 1), (21.25, 20.25, -1, -1),
                           (2.75, 20.25, 1, -1)):
        out.append(("L", g.poly([(x, y + 4.5 * sy), (x, y), (x + 4.5 * sx, y)])))
    out.append(("S", g.rect(8.75, 9.75, 15.25, 14.25, 1)))
    return out


@icon("layout-table", "Layout table")
def layout_table(g):
    return frame(g, 12.75) + [("D", g.p(("M", 2.75, 16.25), ("H", 21.25))),
                              ("D", g.p(("M", 9.25, 12.75), ("V", 20.25))),
                              ("D", g.p(("M", 15.25, 12.75), ("V", 20.25)))]


@icon("layout-next", "Layout next")
def layout_next(g):
    return frame(g, 7.75) + [("D", g.p(("M", 6.75, 13.75), ("H", 16.25))),
                             ("D", g.poly([(12.75, 10.25), (16.25, 13.75), (12.75, 17.25)]))]


@icon("layout-prev", "Layout prev")
def layout_prev(g):
    return frame(g, 7.75) + [("D", g.p(("M", 7.75, 13.75), ("H", 17.25))),
                             ("D", g.poly([(11.25, 10.25), (7.75, 13.75), (11.25, 17.25)]))]


@icon("layout-settings", "Layout settings")
def layout_settings(g):
    return ([("S", notch(g, 2.75, 3.75, 21.25, 20.25)), ("D", g.p(("M", 2.75, 8.75), ("H", 12.75)))]
            + gear_badge(g))


def tab_strip(g):
    return [("S", g.p(("M", 2.75, 20.25), ("V", 4.75), ("A", 1, 0, 1, 3.75, 3.75), ("H", 9.25),
                      ("A", 1, 0, 1, 10.25, 4.75), ("V", 7.75), ("H", 20.25), ("A", 1, 0, 1, 21.25, 8.75),
                      ("V", 20.25), ("Z",))),
            ("L", g.p(("M", 10.25, 7.75), ("V", 5.75), ("A", 1, 0, 1, 11.25, 4.75), ("H", 15.25),
                      ("A", 1, 0, 1, 16.25, 5.75), ("V", 7.75)))]


@icon("tabs", "Tabs")
def tabs(g):
    return tab_strip(g)


@icon("next-tab", "Next tab")
def next_tab(g):
    return tab_strip(g) + [("D", g.p(("M", 6.75, 14.25), ("H", 16.75))),
                           ("D", g.poly([(13.25, 10.75), (16.75, 14.25), (13.25, 17.75)]))]


@icon("close-tab", "Close tab")
def close_tab(g):
    return tab_strip(g) + [("D", g.poly([(8, 10.5), (16, 18.5)])),
                           ("D", g.poly([(16, 10.5), (8, 18.5)]))]


@icon("close", "Close")
def close(g):
    return [("L", g.poly([(5.5, 5.5), (18.5, 18.5)])), ("L", g.poly([(18.5, 5.5), (5.5, 18.5)]))]


@icon("plus", "Plus")
def plus(g):
    return [("L", g.p(("M", 3.75, 11.75), ("H", 20.25))), ("L", g.p(("M", 11.75, 3.75), ("V", 20.25)))]


@icon("arrow-left", "Arrow left")
def arrow_left(g):
    return arrow(g, 20.25, 11.75, 3.75, 11.75, 6)


@icon("arrow-right", "Arrow right")
def arrow_right(g):
    return arrow(g, 3.75, 11.75, 20.25, 11.75, 6)


@icon("move-up", "Move up")
def move_up(g):
    return [("L", g.p(("M", 4.75, 3.75), ("H", 19.25)))] + arrow(g, 11.75, 20.25, 11.75, 7.75, 5)


@icon("move-down", "Move down")
def move_down(g):
    return [("L", g.p(("M", 4.75, 20.25), ("H", 19.25)))] + arrow(g, 11.75, 3.75, 11.75, 16.25, 5)


@icon("lock", "Lock")
def lock_(g):
    return lock(g)


# ---------------------------------------------------------------- project data

@icon("properties", "Properties")
def properties(g):
    out = [("S", g.rect(3.75, 2.75, 20.25, 21.25, 1))]
    for y in (7.75, 11.75, 15.75):
        out.append(("D", g.p(("M", 6.75, y), ("H", 9.25))))
        out.append(("D", g.p(("M", 11.75, y), ("H", 17.25))))
    return out


def sliders(g):
    out = []
    for y, x in ((5.75, 15.5), (11.75, 8.5), (18.25, 13.5)):
        out.append(("L", g.p(("M", 2.75, y), ("H", 21.25))))
        out.append(("S", ring(g, x, y, 3)))
    return out


@icon("params", "Params")
def params(g):
    return sliders(g)


@icon("project-params", "Project params")
def project_params(g):
    out = [("L", g.p(("M", 2.75, 5.75), ("H", 12.75))), ("S", ring(g, 7.5, 5.75, 3))]
    for y, x in ((12.25, 14.5), (18.75, 8.5)):
        out.append(("L", g.p(("M", 2.75, y), ("H", 21.25))))
        out.append(("S", ring(g, x, y, 3)))
    return out + cube_badge(g)


@icon("gen-rules", "Gen rules")
def gen_rules(g):
    out = [("L", g.p(("M", 2.75, 5.75), ("H", 12.75))), ("S", ring(g, 7.5, 5.75, 3))]
    for y, x in ((12.25, 14.5), (18.75, 8.5)):
        out.append(("L", g.p(("M", 2.75, y), ("H", 21.25))))
        out.append(("S", ring(g, x, y, 3)))
    return out + badge(g, "spark")


@icon("project-info", "Project info")
def project_info(g):
    return [("S", g.poly([(3.75, 20.25), (3.75, 10.75), (12, 2.5), (20.25, 10.75), (20.25, 20.25)],
                         True)),
            ("D", g.p(("M", 11.75, 13.75), ("V", 17.25))), ("KX", g.dot(11.75, 10.25))]


@icon("browser", "Browser")
def browser(g):
    return [("S", g.rect(2.75, 2.75, 8.25, 8.25)),
            ("L", g.p(("M", 5.75, 8.25), ("V", 17.75), ("H", 10.75))),
            ("L", g.p(("M", 5.75, 11.75), ("H", 10.75))),
            ("S", g.rect(10.75, 9.75, 15.25, 13.75)), ("S", g.rect(10.75, 15.75, 15.25, 19.75)),
            ("L", g.p(("M", 17.75, 11.75), ("H", 21.25))), ("L", g.p(("M", 17.75, 17.75), ("H", 21.25)))]


@icon("types", "Types")
def types(g):
    return [("S", g.rect(3.75, 3.75, 10.25, 10.25, 1)), ("S", g.rect(13.75, 3.75, 20.25, 10.25, 1)),
            ("S", g.rect(3.75, 13.75, 10.25, 20.25, 1)), ("K", g.rect(13.75, 13.75, 20.25, 20.25, 1))]


@icon("library", "Library")
def library(g):
    return [("S", g.rect(2.75, 4.75, 6.25, 20.25)), ("S", g.rect(9.75, 7.75, 13.25, 20.25)),
            ("D", g.p(("M", 2.75, 8.75), ("H", 6.25))), ("D", g.p(("M", 9.75, 11.75), ("H", 13.25))),
            ("S", g.poly([(15, 8.5), (18, 7), (21.25, 19.5), (18.25, 21)], True))]


@icon("load-family", "Load family")
def load_family(g):
    return [("S", g.circ(3.75, 3.75, 11.25, 11.25)), ("S", g.rect(3.75, 13.75, 11.25, 21.25)),
            ("S", g.poly([(13.75, 21.25), (21.25, 21.25), (17.5, 13.75)], True))] + badge(g, "in")


@icon("timeline-family", "Timeline family")
def timeline_family(g):
    dots = [("K", g.circ(x - 1.5, 4.25, x + 1.5, 7.25)) for x in (5.25, 12, 18.75)]
    return [("D", g.p(("M", 2.75, 5.75), ("H", 21.25)))] + dots + arrow(g, 11.75, 8.25, 11.75, 11.75, 2.5) + [
        ("S", g.circ(2.75, 14.75, 8.25, 20.25)), ("S", g.rect(9.25, 14.75, 14.75, 20.25)),
        ("S", g.poly([(15.75, 20.25), (21.25, 20.25), (18.5, 14.75)], True))]

# ---------------------------------------------------------------- collapsible chrome (fold1)

@icon("fold-ribbon", "Fold ribbon")
def fold_ribbon(g):
    return frame(g, 8.75) + [chevron(g, 12, 13.25, "u", role="D")]


@icon("fold-options", "Fold options")
def fold_options(g):
    return frame(g, 7.75) + [("D", g.p(("M", 2.75, 11.25), ("H", 21.25))),
                             ("D", g.p(("M", 6.25, 9.75), ("H", 10.75))),
                             ("D", g.p(("M", 13.25, 9.75), ("H", 17.75)))]


@icon("fold-status", "Fold status")
def fold_status(g):
    return frame(g) + [("D", g.p(("M", 2.75, 16.75), ("H", 21.25))),
                       ("D", g.p(("M", 6.25, 18.25), ("H", 10.75)))]


@icon("clean-screen", "Clean screen")
def clean_screen(g):
    out = []
    for (x, y, sx, sy) in ((2.75, 3.75, 1, 1), (21.25, 3.75, -1, 1), (21.25, 20.25, -1, -1),
                           (2.75, 20.25, 1, -1)):
        out.append(("L", g.poly([(x, y + 4.5 * sy), (x, y), (x + 4.5 * sx, y)])))
    out.append(("D", g.p(("M", 9.25, 9.25), ("L", 14.75, 14.75))))
    out.append(("D", g.p(("M", 14.75, 9.25), ("L", 9.25, 14.75))))
    return out


@icon("fold-ai-work", "Fold ai work")
def fold_ai_work(g):
    return frame(g) + [("D", g.p(("M", 8.75, 3.75), ("V", 20.25))),
                       ("D", g.poly([(15.75, 9.25), (13.0, 12), (15.75, 14.75)]))]


@icon("fold-ai-preview", "Fold ai preview")
def fold_ai_preview(g):
    return frame(g) + [("D", g.p(("M", 15.25, 3.75), ("V", 20.25))),
                       ("D", g.poly([(8.25, 9.25), (11.0, 12), (8.25, 14.75)]))]


@icon("setup", "Setup")
def setup(g):
    return [("D", g.p(("M", 2.75, 6.75), ("H", 4.75))),
            ("D", g.p(("M", 8.75, 6.75), ("H", 12.25))),
            ("S", g.circ(4.75, 4.75, 8.75, 8.75)),
            ("D", g.p(("M", 2.75, 12.25), ("H", 12.25))),
            ("D", g.p(("M", 16.25, 12.25), ("H", 21.25))),
            ("S", g.circ(12.25, 10.25, 16.25, 14.25)),
            ("D", g.p(("M", 2.75, 17.75), ("H", 6.75))),
            ("D", g.p(("M", 10.75, 17.75), ("H", 21.25))),
            ("S", g.circ(6.75, 15.75, 10.75, 19.75)),
            ("A", g.spark(18.5, 6, 4))]
