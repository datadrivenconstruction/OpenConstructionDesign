// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Session preview in model coordinates: a session lists abstract primitives, every
//! view turns them into its own picture through its adapter — plan footprints, 3D volumes,
//! section cuts. No session draws; `overlay::paint_preview` maps the result to the canvas.
use super::adapter::ViewAdapter;
use super::pointer::{ViewKind, WorkPlane};
use ddc_geom::{v2, v3, Tol, V2, V3};

/// Chord sag of an arc band, m.
const SAG: f64 = 0.01;

/// Colour role of a primitive; canvas tokens `preview` and `error` (DESIGN_SYSTEM.md).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Style {
    Line,
    Fill,
    Invalid,
}

impl Style {
    pub fn token(self) -> &'static str {
        match self {
            Style::Line | Style::Fill => "preview",
            Style::Invalid => "error",
        }
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Glyph {
    /// A collected point.
    Point,
    /// The snapped cursor.
    Snap,
    /// Direction or side marker.
    Arrow,
}

#[derive(Clone, Debug, PartialEq)]
pub enum Prim {
    Polyline { pts: Vec<V3>, closed: bool, style: Style },
    /// Wall-like band along the axis a→b (bulge = tan(sweep/4)), `justify` — offset of the band
    /// centre from the axis to the left, in widths (0 centred, 0.5 all on the left).
    Band { axis: [V3; 2], bulge: f64, width: f64, height: f64, justify: f64 },
    /// Extruded plan ring between elevations.
    Prism { ring: Vec<V2>, z0: f64, z1: f64 },
    Point { p: V3, glyph: Glyph },
    TempDim { a: V3, b: V3, value: f64, editable: bool },
    /// i18n key shown at the point.
    Label { p: V3, key: &'static str },
    /// What would be refused as it stands: drawn in the error colour in every view.
    Invalid(Box<Prim>),
}

/// Rubber-band of a session; every view draws the same primitives.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Preview(pub Vec<Prim>);

/// Picture element in screen pixels, made by a view from a primitive.
#[derive(Clone, Debug, PartialEq)]
pub enum Draw {
    Path { pts: Vec<[f32; 2]>, closed: bool, fill: bool, style: Style },
    /// Triangles of a volume (3D), translucent.
    Tris { pts: Vec<[f32; 2]>, tris: Vec<[u32; 3]>, style: Style },
    Glyph { at: [f32; 2], glyph: Glyph, style: Style },
    /// Temporary dimension: the line and its value in metres.
    Dim { a: [f32; 2], b: [f32; 2], value: f64, editable: bool, style: Style },
    Label { at: [f32; 2], key: &'static str, style: Style },
}

impl Prim {
    /// The same primitive marked invalid.
    pub fn invalid(self) -> Prim {
        match self {
            p @ Prim::Invalid(_) => p,
            p => Prim::Invalid(Box::new(p)),
        }
    }
}

/// Plan ring and elevations of a volume primitive (band or prism).
pub fn footprint(p: &Prim) -> Option<(Vec<V2>, f64, f64)> {
    match p {
        Prim::Band { axis, bulge, width, height, justify } => {
            let (a, b) = (v2(axis[0].x, axis[0].y), v2(axis[1].x, axis[1].y));
            let z0 = axis[0].z.min(axis[1].z);
            let (o_r, o_l) = (width * (justify - 0.5), width * (justify + 0.5));
            let ring = band_ring(a, b, *bulge, o_r, o_l)?;
            Some((ring, z0, z0 + height))
        }
        Prim::Prism { ring, z0, z1 } => (ring.len() >= 3).then(|| (ring.clone(), *z0, *z1)),
        Prim::Invalid(p) => footprint(p),
        _ => None,
    }
}

/// Counter-clockwise ring of a band between left offsets `o_r` < `o_l` of the axis.
fn band_ring(a: V2, b: V2, bulge: f64, o_r: f64, o_l: f64) -> Option<Vec<V2>> {
    let d = b - a;
    let l = d.len();
    if !(l > 1e-9 && o_l > o_r) {
        return None;
    }
    if bulge.abs() < 1e-9 {
        let n = v2(-d.y / l, d.x / l);
        return Some(vec![a + n * o_r, b + n * o_r, b + n * o_l, a + n * o_l]);
    }
    let ax = ddc_geom::arc::ArcAxis::new(a, b, bulge, Tol::default()).ok()?;
    let len = ax.length();
    // chords of the outer face within the sag
    let r = ax.radius_at(o_r).abs().max(ax.radius_at(o_l).abs()).max(1e-6);
    let step = 2.0 * (2.0 * r * SAG).max(1e-12).sqrt();
    let k = ((len * r / ax.radius.max(1e-9)) / step).ceil().clamp(2.0, 256.0) as usize;
    let at = |s: f64, o: f64| {
        let ang = ax.angle_at(s);
        ax.center + v2(ang.cos(), ang.sin()) * ax.radius_at(o)
    };
    let mut ring: Vec<V2> = (0..=k).map(|i| at(len * i as f64 / k as f64, o_r)).collect();
    ring.extend((0..=k).rev().map(|i| at(len * i as f64 / k as f64, o_l)));
    if ddc_geom::poly::area(&ring) < 0.0 {
        ring.reverse();
    }
    Some(ring)
}

/// Closed mesh of a volume primitive (the prism of `ddc_geom`, as for element bodies).
pub fn mesh(p: &Prim) -> Option<ddc_geom::mesh::Mesh> {
    let (ring, z0, z1) = footprint(p)?;
    let m = ddc_geom::footing::prism(&ring, z0, z1);
    (!m.triangles.is_empty()).then_some(m)
}

/// Cut of a volume primitive by a vertical plane: rectangles (s0, s1, z0, z1) in the plane's
/// (u, v) coordinates, s along `u`.
pub fn section_cut(p: &Prim, cut: &WorkPlane) -> Vec<[f64; 4]> {
    let Some((ring, z0, z1)) = footprint(p) else { return vec![] };
    let o = v2(cut.origin.x, cut.origin.y);
    let (u, n) = (v2(cut.u.x, cut.u.y), v2(cut.n.x, cut.n.y));
    let mut ts: Vec<f64> = vec![];
    for i in 0..ring.len() {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        let (fp, fq) = ((p - o).dot(n), (q - o).dot(n));
        // half-open rule: a vertex on the line counts once
        if (fp > 0.0) != (fq > 0.0) {
            let t = fp / (fp - fq);
            ts.push((p + (q - p) * t - o).dot(u));
        }
    }
    ts.sort_by(f64::total_cmp);
    let zb = cut.origin.z;
    ts.chunks_exact(2).filter(|c| c[1] - c[0] > 1e-12).map(|c| [c[0], c[1], z0 - zb, z1 - zb]).collect()
}

fn style_of(invalid: bool, base: Style) -> Style {
    if invalid {
        Style::Invalid
    } else {
        base
    }
}

/// The picture of `prims` in the view of `ad`.
pub fn draws(prims: &[Prim], ad: &dyn ViewAdapter) -> Vec<Draw> {
    let mut out = vec![];
    for p in prims {
        draw_one(p, ad, false, &mut out);
    }
    out
}

fn draw_one(p: &Prim, ad: &dyn ViewAdapter, invalid: bool, out: &mut Vec<Draw>) {
    let pr = |q: V3| ad.project(q);
    match p {
        Prim::Invalid(inner) => draw_one(inner, ad, true, out),
        Prim::Polyline { pts, closed, style } => {
            let s: Option<Vec<[f32; 2]>> = pts.iter().map(|&q| pr(q)).collect();
            if let Some(s) = s {
                out.push(Draw::Path { pts: s, closed: *closed, fill: false, style: style_of(invalid, *style) });
            }
        }
        Prim::Band { .. } | Prim::Prism { .. } => {
            let st = style_of(invalid, Style::Fill);
            match ad.kind() {
                ViewKind::Plan { .. } => {
                    if let Some((ring, z0, _)) = footprint(p) {
                        let s: Option<Vec<[f32; 2]>> = ring.iter().map(|q| pr(v3(q.x, q.y, z0))).collect();
                        if let Some(s) = s {
                            out.push(Draw::Path { pts: s, closed: true, fill: true, style: st });
                        }
                    }
                    if let Prim::Band { axis, .. } = p {
                        if let (Some(a), Some(b)) = (pr(axis[0]), pr(axis[1])) {
                            out.push(Draw::Path { pts: vec![a, b], closed: false, fill: false, style: style_of(invalid, Style::Line) });
                        }
                    }
                }
                ViewKind::View3d => {
                    if let Some(m) = mesh(p) {
                        let s: Option<Vec<[f32; 2]>> = m.positions.iter().map(|&q| pr(q)).collect();
                        if let Some(s) = s {
                            out.push(Draw::Tris { pts: s, tris: m.triangles, style: st });
                        }
                    }
                }
                ViewKind::Section { .. } => {
                    if let Some(cut) = ad.cut() {
                        for [s0, s1, z0, z1] in section_cut(p, &cut) {
                            let c = [v2(s0, z0), v2(s1, z0), v2(s1, z1), v2(s0, z1)];
                            let s: Option<Vec<[f32; 2]>> = c.iter().map(|&q| pr(cut.from_uv(q))).collect();
                            if let Some(s) = s {
                                out.push(Draw::Path { pts: s, closed: true, fill: true, style: st });
                            }
                        }
                    }
                }
            }
        }
        Prim::Point { p: q, glyph } => {
            if let Some(at) = pr(*q) {
                out.push(Draw::Glyph { at, glyph: *glyph, style: style_of(invalid, Style::Line) });
            }
        }
        Prim::TempDim { a, b, value, editable } => {
            if let (Some(sa), Some(sb)) = (pr(*a), pr(*b)) {
                out.push(Draw::Dim { a: sa, b: sb, value: *value, editable: *editable, style: style_of(invalid, Style::Line) });
            }
        }
        Prim::Label { p: q, key } => {
            if let Some(at) = pr(*q) {
                out.push(Draw::Label { at, key: *key, style: style_of(invalid, Style::Line) });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::interact::adapter::{PlanAdapter, SectionAdapter, View3dAdapter};

    fn wall6() -> Prim {
        Prim::Band { axis: [v3(0.0, 0.0, 0.0), v3(6.0, 0.0, 0.0)], bulge: 0.0, width: 0.2, height: 3.0, justify: 0.0 }
    }

    fn rubber() -> Vec<Prim> {
        vec![wall6(), Prim::TempDim { a: v3(0.0, 0.0, 0.0), b: v3(6.0, 0.0, 0.0), value: 6.0, editable: true }]
    }

    fn plan() -> PlanAdapter {
        PlanAdapter { center: v2(3.0, 0.0), scale: 100.0, canvas: [400.0, 300.0], level: 0, z: 0.0 }
    }

    fn view3d() -> View3dAdapter {
        let cam = ddc_render::OrbitCamera {
            target: ddc_render::glam::Vec3::new(3.0, 0.0, 1.5),
            distance: 20.0,
            ..Default::default()
        };
        View3dAdapter::new(&cam, [0.0, 0.0], [800.0, 600.0], 0.0, WorkPlane::level(0, 0.0))
    }

    /// Section across the wall at x = 3, looking along -x.
    fn across() -> SectionAdapter {
        SectionAdapter::new(5, v2(3.0, -1.0), v2(3.0, 1.0), v2(1.0, 1.5), 100.0, [400.0, 300.0]).unwrap()
    }

    fn bbox(pts: &[V3]) -> (V3, V3) {
        let mut lo = v3(f64::MAX, f64::MAX, f64::MAX);
        let mut hi = v3(f64::MIN, f64::MIN, f64::MIN);
        for p in pts {
            lo = v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z));
            hi = v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z));
        }
        (lo, hi)
    }

    fn close(a: f64, b: f64) -> bool {
        (a - b).abs() < 1e-9
    }

    #[test]
    fn wall_band_in_three_views() {
        // plan: the strip of the wall with its axis and the temporary dimension
        let (ring, z0, z1) = footprint(&wall6()).unwrap();
        assert_eq!(ring, vec![v2(0.0, -0.1), v2(6.0, -0.1), v2(6.0, 0.1), v2(0.0, 0.1)]);
        assert_eq!((z0, z1), (0.0, 3.0));
        let d = draws(&rubber(), &plan());
        assert!(matches!(&d[0], Draw::Path { pts, closed: true, fill: true, style: Style::Fill } if pts.len() == 4));
        assert!(matches!(&d[1], Draw::Path { pts, closed: false, style: Style::Line, .. } if pts.len() == 2));
        assert!(matches!(&d[2], Draw::Dim { value, editable: true, .. } if close(*value, 6.0)));
        if let Draw::Path { pts, .. } = &d[0] {
            // 6 m × 0.2 m at 100 px/m
            assert!((pts[1][0] - pts[0][0] - 600.0).abs() < 1e-3 && (pts[0][1] - pts[3][1] - 20.0).abs() < 1e-3);
        }
        // 3D: a closed volume 6 × 0.2 × 3
        let m = mesh(&wall6()).unwrap();
        let (lo, hi) = bbox(&m.positions);
        assert!(close(hi.x - lo.x, 6.0) && close(hi.y - lo.y, 0.2) && close(hi.z - lo.z, 3.0));
        assert!(close(m.volume(), 3.6), "volume {}", m.volume());
        let d = draws(&rubber(), &view3d());
        assert!(matches!(&d[0], Draw::Tris { tris, style: Style::Fill, .. } if tris.len() == m.triangles.len()));
        assert!(matches!(&d[1], Draw::Dim { .. }));
        // section across: the cut rectangle 0.2 × 3
        let cut = across().plane;
        let r = section_cut(&wall6(), &cut);
        assert_eq!(r.len(), 1);
        assert!(close(r[0][0], 0.9) && close(r[0][1], 1.1) && close(r[0][2], 0.0) && close(r[0][3], 3.0), "{r:?}");
        let d = draws(&rubber(), &across());
        assert!(matches!(&d[0], Draw::Path { pts, closed: true, fill: true, .. } if pts.len() == 4));
        if let Draw::Path { pts, .. } = &d[0] {
            assert!(((pts[1][0] - pts[0][0]).abs() - 20.0).abs() < 1e-3 && (pts[0][1] - pts[2][1] - 300.0).abs() < 1e-3);
        }
        // invalid: the error style everywhere
        let bad: Vec<Prim> = rubber().into_iter().map(Prim::invalid).collect();
        for ad in [&plan() as &dyn ViewAdapter, &view3d(), &across()] {
            let d = draws(&bad, ad);
            assert!(!d.is_empty());
            for x in &d {
                let st = match x {
                    Draw::Path { style, .. } | Draw::Tris { style, .. } | Draw::Glyph { style, .. } | Draw::Dim { style, .. } | Draw::Label { style, .. } => *style,
                };
                assert_eq!(st, Style::Invalid, "{:?}", ad.kind());
            }
        }
    }

    #[test]
    fn section_along_the_wall_misses_or_cuts_by_side() {
        // a cut parallel to the band outside it gives nothing, through it the whole length
        let out = SectionAdapter::new(1, v2(-1.0, 1.0), v2(7.0, 1.0), v2(0.0, 0.0), 50.0, [0.0, 0.0]).unwrap();
        assert!(section_cut(&wall6(), &out.plane).is_empty());
        let through = SectionAdapter::new(1, v2(-1.0, 0.0), v2(7.0, 0.0), v2(0.0, 0.0), 50.0, [0.0, 0.0]).unwrap();
        let r = section_cut(&wall6(), &through.plane);
        assert_eq!(r.len(), 1);
        assert!(close(r[0][0], 1.0) && close(r[0][1], 7.0));
    }

    #[test]
    fn arc_band_keeps_its_area() {
        // half circle of radius 3 (bulge 1), 0.2 thick: area = π·R·w
        let p = Prim::Band { axis: [v3(0.0, 0.0, 0.0), v3(6.0, 0.0, 0.0)], bulge: 1.0, width: 0.2, height: 3.0, justify: 0.0 };
        let (ring, ..) = footprint(&p).unwrap();
        let a = ddc_geom::poly::area(&ring);
        let want = std::f64::consts::PI * 3.0 * 0.2;
        assert!((a - want).abs() / want < 1e-2, "{a} vs {want}");
    }

    #[test]
    fn wall_preview_build_under_0_2ms() {
        let bound = if cfg!(debug_assertions) { 2e-3 } else { 0.2e-3 };
        let (pl, v3d, sec) = (plan(), view3d(), across());
        let mut best = f64::MAX;
        for _ in 0..20 {
            let t = web_time::Instant::now();
            let n = draws(&rubber(), &pl).len() + draws(&rubber(), &v3d).len() + draws(&rubber(), &sec).len();
            std::hint::black_box(n);
            best = best.min(t.elapsed().as_secs_f64());
        }
        assert!(best <= bound, "{:.3} ms", best * 1e3);
    }
}
