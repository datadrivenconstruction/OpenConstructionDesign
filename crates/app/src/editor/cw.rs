// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Curtain walls and floor slabs in the editor: level loading, 3D meshes (panels as separate
//! elements, glass with a transparent tag), plan picking, the "Curtain wall" tool, property
//! edits. Undo is the model journal (`Model::undo`), redo is the `Cmd` commands.

use super::levels::OTHER_LEVEL;
use super::{Cmd, Editor, Sel};
use ddc_geom::cut::Cutter;
use ddc_geom::{v2, V2};
use ddc_model::{
    CurtainLayout, CurtainQty, CurtainSpec, GridLines, Model, MullionSection, PanelKind, PanelSpec,
    SlabSlope, ViewPrim,
};
use ddc_render::{SceneMesh, TAG_DOOR, TAG_GLASS, TAG_MULLION, TAG_PANEL, TAG_SLAB, TAG_WINDOW};

pub struct CurtainItem {
    pub id: i64,
    pub layout: CurtainLayout,
    pub qty: CurtainQty,
}

impl CurtainItem {
    pub fn spec(&self) -> &CurtainSpec {
        &self.layout.spec
    }
    pub fn length(&self) -> f64 {
        super::cw::axis(self.spec()).map_or(0.0, |a| a.length())
    }
}

pub struct SlabItem {
    pub id: i64,
    /// [outer, holes...].
    pub rings: Vec<Vec<V2>>,
    /// Shaft and opening cutouts from the sketch (not edited by outline grips).
    pub cuts: Vec<Vec<V2>>,
    pub slope: Option<SlabSlope>,
    /// Top relative to the current level (at point `slope.p0`).
    pub top: f64,
    pub thickness: f64,
    /// Plan area.
    pub area: f64,
    pub volume: f64,
}

impl SlabItem {
    pub fn slope_percent(&self) -> f64 {
        self.slope.map_or(0.0, |p| p.gx.hypot(p.gy) * 100.0)
    }
    /// Area of the top face (sloped).
    pub fn surface_area(&self) -> f64 {
        self.area * self.slope.map_or(1.0, |p| p.area_factor())
    }
    /// Top elevation at a plan point relative to the level.
    pub fn top_at(&self, p: V2) -> f64 {
        self.top + self.slope.map_or(0.0, |s| s.rise(p))
    }
}

/// Parameters of new curtain walls (tool panel).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct CurtainDefaults {
    pub u: f64,
    pub v: f64,
    /// Mullion: width in the wall plane × depth.
    pub mullion: (f64, f64),
}

impl Default for CurtainDefaults {
    fn default() -> Self {
        Self {
            u: 1.5,
            v: 1.0,
            mullion: (0.05, 0.15),
        }
    }
}

pub(super) fn axis(s: &CurtainSpec) -> Option<ddc_geom::curtain::Axis> {
    ddc_geom::curtain::Axis::new(v2(s.a.0, s.a.1), v2(s.b.0, s.b.1), s.bulge).ok()
}

fn cw_half(c: &CurtainItem) -> f64 {
    c.spec()
        .interior
        .width()
        .max(c.spec().border.width())
        .max(0.05)
        / 2.0
}

/// Pick distance of one curtain wall, when within `r` of `p`.
fn cw_hit(c: &CurtainItem, p: V2, r: f64) -> Option<(Sel, f64)> {
    let ax = axis(c.spec())?;
    let s = ax.param_of(p).clamp(0.0, ax.length());
    let d = (ax.point_at(s) - p).len() - cw_half(c);
    (d <= r).then_some((Sel::Curtain(c.id), d))
}

pub(super) struct Loaded {
    pub curtains: Vec<CurtainItem>,
    pub slabs: Vec<SlabItem>,
    pub lines: Vec<(i64, V2, V2)>,
    pub elevation: f64,
}

/// Slabs of any level and that level's elevation (the plan draws the level below with it).
pub fn slabs_of(m: &Model, level: i64) -> (Vec<SlabItem>, f64) {
    let l = load(m, level);
    (l.slabs, l.elevation)
}

pub(super) fn load(m: &Model, level: i64) -> Loaded {
    let db = m.sql();
    let elevation: f64 = db
        .query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
            r.get(0)
        })
        .unwrap_or(0.0);
    let ids: Vec<i64> = db
        .prepare("SELECT id FROM curtain_wall WHERE level=?1 ORDER BY id")
        .and_then(|mut st| {
            st.query_map([level], |r| r.get(0))?
                .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default();
    let curtains = ids
        .into_iter()
        .filter_map(|id| {
            let layout = m.curtain_layout(id).ok()?;
            let qty = layout.quantities();
            Some(CurtainItem { id, layout, qty })
        })
        .collect();
    let lines = m
        .curtain_plan(level)
        .unwrap_or_default()
        .into_iter()
        .filter_map(|p| match p {
            ViewPrim::CurtainLine { curtain, a, b } => Some((curtain, v2(a.0, a.1), v2(b.0, b.1))),
            _ => None,
        })
        .collect();
    let rows: Vec<(i64, f64, f64, f64, f64)> = db
        .prepare(
            "SELECT s.id, s.offset, \
             COALESCE((SELECT SUM(thickness) FROM slab_type_layer WHERE stype=s.stype),0), \
             COALESCE((SELECT MAX(area) FROM slab_qty q WHERE q.slab=s.id),0), \
             COALESCE((SELECT SUM(volume) FROM slab_qty q WHERE q.slab=s.id),0) \
             FROM slab s WHERE s.level=?1 ORDER BY s.id",
        )
        .and_then(|mut st| {
            st.query_map([level], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })?
            .collect::<Result<Vec<_>, _>>()
        })
        .unwrap_or_default();
    let slabs = rows
        .into_iter()
        .filter_map(|(id, top, thickness, area, volume)| {
            // a slab cut by a void shows its section at mid-thickness (outer ring first)
            let cut = m
                .cut_section("slab", id, elevation + top - thickness / 2.0)
                .ok()
                .flatten()
                .filter(|r| !r.is_empty());
            let rings = match cut {
                Some(mut rs) => {
                    rs.sort_by(|a, b| ddc_geom::poly::area(b).total_cmp(&ddc_geom::poly::area(a)));
                    rs
                }
                None => m
                    .slab_rings(id)
                    .ok()?
                    .iter()
                    .map(|r| r.iter().map(|&(x, y)| v2(x, y)).collect())
                    .collect(),
            };
            let cuts: Vec<Vec<V2>> = m
                .slab_cut_rings(id)
                .unwrap_or_default()
                .iter()
                .map(|r| r.iter().map(|&(x, y)| v2(x, y)).collect())
                .collect();
            Some(SlabItem {
                id,
                rings,
                cuts,
                slope: m.slab_slope(id).ok().flatten(),
                top,
                thickness,
                area,
                volume,
            })
        })
        .collect();
    Loaded {
        curtains,
        slabs,
        lines,
        elevation,
    }
}

/// Point inside an outline (even-odd).
pub fn inside(p: V2, poly: &[V2]) -> bool {
    let mut c = false;
    for k in 0..poly.len() {
        let (a, b) = (poly[k], poly[(k + 1) % poly.len()]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) / (b.y - a.y) * (b.x - a.x) {
            c = !c;
        }
    }
    c
}

/// Clip a polygon by the half-plane `dot(p, d) >= c` (Sutherland-Hodgman).
fn clip(poly: &[V2], d: V2, c: f64) -> Vec<V2> {
    let f = |p: V2| p.x * d.x + p.y * d.y - c;
    let mut out = vec![];
    for k in 0..poly.len() {
        let (p, q) = (poly[k], poly[(k + 1) % poly.len()]);
        let (fp, fq) = (f(p), f(q));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp >= 0.0) != (fq >= 0.0) {
            out.push(p + (q - p) * (fp / (fp - fq)));
        }
    }
    out
}

/// Slab cutters: a flat slab is one prism; a sloped one is strips across the slope, each at
/// the strip's mid elevation (step no larger than `STEP_ERR`).
pub(super) fn slab_cutters(
    rings: &[Vec<V2>],
    top: f64,
    t: f64,
    slope: Option<SlabSlope>,
) -> Vec<Cutter> {
    const STEP_ERR: f64 = 0.0025;
    let Some((outer, holes)) = rings.split_first() else {
        return vec![];
    };
    let flat = |ring: Vec<V2>, holes: Vec<Vec<V2>>, z1: f64| Cutter {
        ring,
        holes,
        z0: z1 - t,
        z1,
    };
    let Some(pl) = slope.filter(|p| !p.is_flat()) else {
        return vec![flat(outer.clone(), holes.to_vec(), top)];
    };
    let g = v2(pl.gx, pl.gy);
    let gl = g.len();
    let d = g * (1.0 / gl);
    let s: Vec<f64> = outer.iter().map(|p| p.x * d.x + p.y * d.y).collect();
    let (s0, s1) = s
        .iter()
        .fold((f64::MAX, f64::MIN), |(a, b), &v| (a.min(v), b.max(v)));
    let step = (2.0 * STEP_ERR / gl).clamp(0.05, 1.0);
    let n = (((s1 - s0) / step).ceil() as usize).clamp(1, 400);
    let w = (s1 - s0) / n as f64;
    (0..n)
        .filter_map(|k| {
            let (a, b) = (s0 + w * k as f64, s0 + w * (k + 1) as f64);
            let band = |r: &[V2]| clip(&clip(r, d, a), d * -1.0, -b);
            let ring = band(outer);
            if ring.len() < 3 {
                return None;
            }
            let mid = d * ((a + b) / 2.0);
            // the rise is linear along d, so any point of the strip's middle will do
            let z1 = top + pl.rise(mid);
            let hs = holes
                .iter()
                .map(|h| band(h))
                .filter(|h| h.len() >= 3)
                .collect();
            Some(flat(ring, hs, z1))
        })
        .collect()
}

fn to_scene(
    element: u32,
    tag: u32,
    m: &ddc_geom::mesh::Mesh,
    tris: &[usize],
    dz: f64,
) -> SceneMesh {
    let mut map = std::collections::HashMap::new();
    let mut positions = vec![];
    let triangles = tris
        .iter()
        .map(|&t| {
            m.triangles[t].map(|i| {
                *map.entry(i).or_insert_with(|| {
                    let p = m.positions[i as usize];
                    positions.push([p.x as f32, p.y as f32, (p.z - dz) as f32]);
                    positions.len() as u32 - 1
                })
            })
        })
        .collect();
    SceneMesh {
        element,
        tag,
        positions,
        triangles,
    }
}

/// Box `o + s·u + r·v + q·n`, s, r in [0, 1], q in [-1/2, 1/2].
fn block(
    element: u32,
    tag: u32,
    o: [f64; 3],
    u: [f64; 3],
    v: [f64; 3],
    n: [f64; 3],
    dz: f64,
) -> SceneMesh {
    let mut positions = vec![];
    for k in 0..8 {
        let (s, r, q) = (
            (k & 1) as f64,
            ((k >> 1) & 1) as f64,
            ((k >> 2) & 1) as f64 - 0.5,
        );
        let p = |i: usize| o[i] + s * u[i] + r * v[i] + q * n[i];
        positions.push([p(0) as f32, p(1) as f32, (p(2) - dz) as f32]);
    }
    let quads = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let triangles = quads
        .iter()
        .flat_map(|q| [[q[0], q[1], q[2]], [q[0], q[2], q[3]]])
        .collect();
    SceneMesh {
        element,
        tag,
        positions,
        triangles,
    }
}

/// Slope plane of slab `s` for `pct` percent (`Some(None)` flattens it); `None` when out of range.
pub fn slope_for_percent(s: &SlabItem, pct: f64) -> Option<Option<SlabSlope>> {
    if !pct.is_finite() || pct.abs() > 1000.0 {
        return None;
    }
    Some(if pct == 0.0 {
        None
    } else {
        let (p0, dir) = match s.slope.filter(|p| !p.is_flat()) {
            Some(p) => (p.p0, v2(p.gx, p.gy) * (1.0 / p.gx.hypot(p.gy))),
            None => {
                let o = &s.rings[0];
                let k = (0..o.len())
                    .max_by(|&a, &b| {
                        let l = |i: usize| (o[(i + 1) % o.len()] - o[i]).len();
                        // equal lengths: the first edge (contour direction)
                        l(a).total_cmp(&l(b)).then(b.cmp(&a))
                    })
                    .unwrap_or(0);
                let d = o[(k + 1) % o.len()] - o[k];
                (o[k], d * (1.0 / d.len()))
            }
        };
        let g = dir * (pct / 100.0);
        Some(SlabSlope {
            p0,
            gx: g.x,
            gy: g.y,
        })
    })
}

impl Editor {
    /// First scene element after walls and openings: curtain walls, then slabs.
    fn cw_base(&self) -> u32 {
        (self.walls.len() + self.openings.len()) as u32
    }

    /// The mullion element of each curtain wall; panels are the next `cells` elements.
    fn cw_starts(&self) -> (Vec<u32>, u32) {
        let mut e = self.cw_base();
        let mut v = vec![];
        for c in &self.curtains {
            v.push(e);
            e += 1 + c.layout.layout.cells.len() as u32;
        }
        (v, e)
    }

    pub(super) fn cw_range(&self, s: Sel) -> Option<(u32, u32)> {
        let (starts, slab0) = self.cw_starts();
        let ci = |id: i64| self.curtains.iter().position(|c| c.id == id);
        match s {
            Sel::Curtain(id) => ci(id).map(|i| {
                let n = self.curtains[i].layout.layout.cells.len() as u32;
                (starts[i], starts[i] + 1 + n)
            }),
            Sel::Panel(id, k) => ci(id)
                .filter(|&i| (k as usize) < self.curtains[i].layout.layout.cells.len())
                .map(|i| (starts[i] + 1 + k, starts[i] + 2 + k)),
            Sel::Slab(id) => self
                .slabs
                .iter()
                .position(|s| s.id == id)
                .map(|i| (slab0 + i as u32, slab0 + i as u32 + 1)),
            Sel::Body(k, id) => {
                let b0 = slab0 + self.slabs.len() as u32;
                self.bodies
                    .iter()
                    .position(|b| b.kind == k && b.id == id)
                    .map(|i| (b0 + i as u32, b0 + i as u32 + 1))
            }
            _ => None,
        }
    }

    pub(super) fn cw_sel_of(&self, e: u32) -> Option<Sel> {
        let (starts, slab0) = self.cw_starts();
        if e >= slab0 {
            let k = (e - slab0) as usize;
            return match self.slabs.get(k) {
                Some(s) => Some(Sel::Slab(s.id)),
                None => self
                    .bodies
                    .get(k - self.slabs.len())
                    .map(|b| Sel::Body(b.kind, b.id)),
            };
        }
        let i = starts.iter().rposition(|&s| s <= e)?;
        let id = self.curtains[i].id;
        Some(match e - starts[i] {
            0 => Sel::Curtain(id),
            k => Sel::Panel(id, k - 1),
        })
    }

    /// Meshes of curtain walls (mullions as one element, each panel its own) and of the level's slabs.
    pub(super) fn cw_meshes(&self, out: &mut Vec<SceneMesh>, issues: &mut Vec<String>) {
        let (starts, slab0) = self.cw_starts();
        let dz = self.elevation;
        for (c, &e0) in self.curtains.iter().zip(&starts) {
            let l = &c.layout;
            let m = l.mesh();
            let cells = l.layout.cells.len();
            let mut by_panel: Vec<Vec<usize>> = vec![vec![]; cells];
            let mut bars = vec![];
            for (t, &f) in m.face_of.iter().enumerate() {
                match by_panel.get_mut(f as usize) {
                    Some(v) => v.push(t),
                    None => bars.push(t),
                }
            }
            out.push(to_scene(e0, TAG_MULLION, &m, &bars, dz));
            for (k, (tris, cell)) in by_panel.iter().zip(&l.layout.cells).enumerate() {
                let kind = l.panels.get(k).map_or(PanelKind::Glazed, |p| p.kind);
                let e = e0 + 1 + k as u32;
                let tag = match kind {
                    PanelKind::Empty => continue,
                    PanelKind::Glazed | PanelKind::Window => TAG_GLASS,
                    PanelKind::Solid => TAG_PANEL,
                    PanelKind::Door => TAG_DOOR,
                };
                out.push(to_scene(e, tag, &m, tris, dz));
                if matches!(kind, PanelKind::Window | PanelKind::Door) {
                    // the sash frame along the clean cell outline
                    let n = cell.net;
                    let (o, u, v) = (n[0], n[1] - n[0], n[3] - n[0]);
                    let (ul, vl) = (u.len(), v.len());
                    if ul < 0.12 || vl < 0.12 {
                        continue;
                    }
                    let nr = u.cross(v).normalized() * 0.06;
                    let (fu, fv) = (u * (0.05 / ul), v * (0.05 / vl));
                    let a = |p: ddc_geom::V3| [p.x, p.y, p.z];
                    for (oo, uu, vv) in [
                        (o, u, fv),
                        (o + v - fv, u, fv),
                        (o + fv, fu, v - fv * 2.0),
                        (o + u - fu + fv, fu, v - fv * 2.0),
                    ] {
                        out.push(block(e, TAG_WINDOW, a(oo), a(uu), a(vv), a(nr), dz));
                    }
                }
            }
        }
        let texl = crate::mat_tex::layers(self);
        for (i, s) in self.slabs.iter().enumerate() {
            // the exact cut (the `cuts` cache) goes first, otherwise the body with voids of typical bodies
            let cut = self.model.cut_mesh("slab", s.id).ok().flatten();
            match cut.map_or_else(|| crate::solid_tools::slab_mesh(self, s.id, issues), Ok) {
                Ok(m) => {
                    let all: Vec<usize> = (0..m.triangles.len()).collect();
                    let sm = to_scene(slab0 + i as u32, TAG_SLAB, &m, &all, dz);
                    match texl.split("slab", s.id, &sm, TAG_SLAB) {
                        Some(parts) => out.extend(parts),
                        None => out.push(SceneMesh {
                            tag: texl.elem("slab", s.id, TAG_SLAB),
                            ..sm
                        }),
                    }
                }
                Err(e) => issues.push(crate::tr(
                    "curtain_wall.cw.issue.slab",
                    &[("id", &s.id.to_string()), ("e", &e.to_string())],
                )),
            }
        }
        // level roofs (with shaft, opening and dormer cutouts) are for display, not picking; in 3D
        // with `edit3d` providers they draw roofs and bodies (`scene_providers`), otherwise duplicates
        let ids = |sql: &str| -> Vec<i64> {
            let Ok(mut st) = self.model.sql().prepare(sql) else {
                return vec![];
            };
            st.query_map([self.level], |r| r.get(0))
                .map(|it| it.flatten().collect())
                .unwrap_or_default()
        };
        let roofs = if self.scene_providers {
            vec![]
        } else {
            ids("SELECT id FROM roof WHERE level=?1 ORDER BY id")
        };
        for id in roofs {
            match self.model.roof_mesh(id) {
                Ok(m) => {
                    let all: Vec<usize> = (0..m.triangles.len()).collect();
                    let rm = to_scene(OTHER_LEVEL, TAG_SLAB, &m, &all, dz);
                    match texl.split("roof", id, &rm, TAG_SLAB) {
                        Some(parts) => out.extend(parts),
                        None => out.push(SceneMesh {
                            tag: texl.elem("roof", id, TAG_SLAB),
                            ..rm
                        }),
                    }
                }
                Err(e) => issues.push(crate::tr(
                    "host_shape.issue.roof",
                    &[("id", &id.to_string()), ("e", &e.to_string())],
                )),
            }
        }
        // imported level bodies are pickable elements after the slabs
        let b0 = slab0 + self.slabs.len() as u32;
        // ceilings are bodies too (pickable); there is no separate ceiling loop
        for (i, b) in self
            .bodies
            .iter()
            .enumerate()
            .filter(|_| !self.scene_providers)
        {
            use super::bodies::BodyKind as K;
            let tag = match b.kind {
                K::Mesh => TAG_PANEL,
                K::Column | K::Beam | K::Ceiling => TAG_SLAB,
            };
            let e = if super::bodies::is_site(&b.category) {
                super::levels::SITE
            } else {
                b0 + i as u32
            };
            let m = match &b.mesh {
                Some(m) => m.clone(),
                None => match self.model.imported_mesh(b.id) {
                    Ok(x) => std::sync::Arc::new(x.mesh),
                    Err(err) => {
                        issues.push(crate::tr(
                            "curtain_wall.cw.issue.body",
                            &[("id", &b.id.to_string()), ("e", &err.to_string())],
                        ));
                        continue;
                    }
                },
            };
            let all: Vec<usize> = (0..m.triangles.len()).collect();
            out.push(to_scene(e, tag, &m, &all, dz));
        }
    }

    /// A curtain wall or slab under a plan point (after openings, before walls, by distance).
    pub fn cw_pick(&self, p: V2) -> Option<(Sel, f64)> {
        let r = self.tool.radius;
        let cw = self.pick_cw.with(
            (self.level, self.revision),
            || self.cw_boxes(),
            |ix| {
                ix.near(p, r)
                    .into_iter()
                    // `get`: a list swapped without a revision bump must not panic a hover
                    .filter_map(|i| cw_hit(self.curtains.get(i as usize)?, p, r))
                    .min_by(|a, b| a.1.total_cmp(&b.1))
            },
        );
        self.pick_or_slab(cw, p)
    }

    /// One padded box per curtain wall (arcs sampled, margin covers the sag between samples).
    fn cw_boxes(&self) -> super::pick_index::BoxIndex {
        const N: usize = 32;
        let boxes = self
            .curtains
            .iter()
            .map(|c| {
                let ax = axis(c.spec())?;
                let len = ax.length();
                let pad = cw_half(c) + if ax.arc.is_some() { len / N as f64 } else { 0.0 };
                super::pick_index::bx_of((0..=N).map(|k| ax.point_at(len * k as f64 / N as f64)), pad)
            })
            .collect();
        super::pick_index::BoxIndex::build(boxes)
    }

    fn pick_or_slab(&self, cw: Option<(Sel, f64)>, p: V2) -> Option<(Sel, f64)> {
        cw.or_else(|| {
            let cls = crate::plan_range::classes(self);
            self.slabs.iter().enumerate().find_map(|(i, s)| {
                if cls.slab(i) == crate::plan_range::RangeClass::Hidden {
                    return None;
                }
                let (o, hs) = s.rings.split_first()?;
                (inside(p, o) && !hs.iter().any(|h| inside(p, h)))
                    .then_some((Sel::Slab(s.id), f64::MAX))
            })
        })
    }

    /// The "Curtain wall" tool: the axis is drawn like a wall (snaps, length input, arc).
    pub fn start_curtain(&mut self) {
        self.set_mode(crate::wall_tool::Mode::Polyline);
        self.tool.curtain = true;
    }

    fn curtain_spec(&self, a: V2, b: V2, bulge: f64) -> CurtainSpec {
        let d = self.cw_defaults;
        let sec = MullionSection::Rect {
            w: d.mullion.0,
            d: d.mullion.1,
        };
        CurtainSpec {
            level: self.level,
            name: crate::tr("curtain_wall.cw.default_name", &[]),
            a: (a.x, a.y),
            b: (b.x, b.y),
            bulge,
            base_offset: 0.0,
            height: self.height,
            u: GridLines::Spacing(d.u),
            v: GridLines::Spacing(d.v),
            interior: sec,
            border: sec,
            lines: Default::default(),
            top: vec![],
        }
    }

    pub fn add_curtains(&mut self, segs: &[(V2, V2, f64)]) {
        let specs: Vec<CurtainSpec> = segs
            .iter()
            .map(|&(a, b, k)| self.curtain_spec(a, b, k))
            .collect();
        match self.apply_add_curtains(&specs) {
            Ok(ids) => {
                self.status = Some(crate::tr(
                    "curtain_wall.cw.added",
                    &[("n", &ids.len().to_string())],
                ));
                self.selection = ids.iter().map(|&id| Sel::Curtain(id)).collect();
                self.push(Cmd::AddCurtains(specs, ids));
            }
            Err(e) => {
                self.status = Some(crate::tr("curtain_wall.cw.add_failed", &[("e", &e.to_string())]))
            }
        }
    }

    pub(super) fn apply_add_curtains(
        &mut self,
        specs: &[CurtainSpec],
    ) -> Result<Vec<i64>, ddc_model::Error> {
        let ids = self.model.add_curtain_walls(specs)?;
        self.reload()?;
        Ok(ids)
    }

    pub fn curtain(&self, id: i64) -> Option<&CurtainItem> {
        self.curtains.iter().find(|c| c.id == id)
    }

    pub fn slab(&self, id: i64) -> Option<&SlabItem> {
        self.slabs.iter().find(|s| s.id == id)
    }

    /// Curtain wall edit from properties (height, grid step, mullions): one operation.
    pub fn edit_curtain(&mut self, id: i64, f: impl FnOnce(&mut CurtainSpec)) {
        let Some(before) = self.curtain(id).map(|c| c.spec().clone()) else {
            return;
        };
        let mut after = before.clone();
        f(&mut after);
        if after == before {
            return;
        }
        match self.model.set_curtain_wall(id, &after).and_then(|w| {
            self.reload()?;
            Ok(w)
        }) {
            Ok(warn) => {
                self.status = Some(match warn.first() {
                    Some(w) => crate::tr(
                        "curtain_wall.cw.edited_warn",
                        &[("id", &id.to_string()), ("w", &w.to_string())],
                    ),
                    None => crate::tr("curtain_wall.cw.edited", &[("id", &id.to_string())]),
                });
                self.push(Cmd::EditCurtain(id, after));
            }
            Err(e) => self.status = Some(crate::tr("curtain_wall.cw.not_changed", &[("e", &e.to_string())])),
        }
    }

    /// Panel type of cell `k`: one operation.
    pub fn set_panel_kind(&mut self, id: i64, k: u32, kind: PanelKind) {
        self.set_panel(id, k, |p| p.kind = kind);
    }

    /// Panel of cell `k` (type, name): one undoable operation.
    pub fn set_panel(&mut self, id: i64, k: u32, f: impl FnOnce(&mut PanelSpec)) {
        let Some(c) = self.curtain(id) else { return };
        let Some(cell) = c.layout.layout.cells.get(k as usize) else {
            return;
        };
        let (i, j) = (cell.i as u32, cell.j as u32);
        let mut p = c.layout.panels.get(k as usize).cloned().unwrap_or_default();
        let before = p.clone();
        f(&mut p);
        if p == before {
            return;
        }
        let kind = p.kind;
        match self
            .model
            .set_curtain_panel(id, i, j, &p)
            .and_then(|_| self.reload())
        {
            Ok(()) => {
                self.status = Some(crate::tr(
                    "curtain_props.cw.status.panel",
                    &[
                        ("i", &(i + 1).to_string()),
                        ("j", &(j + 1).to_string()),
                        ("kind", &crate::elem::curtain::kind_label(kind)),
                    ],
                ));
                self.push(Cmd::Panel(id, i, j, p));
            }
            Err(e) => self.status = Some(crate::tr("curtain_wall.cw.not_changed", &[("e", &e.to_string())])),
        }
    }

    /// Room `id`: name, number, height: one undoable operation.
    pub fn edit_room(&mut self, id: i64, f: impl FnOnce(&mut ddc_model::RoomSpec)) {
        let got = self.model.sql().query_row(
            "SELECT level, name, number, px, py, height FROM room WHERE id=?1",
            [id],
            |r| {
                Ok(ddc_model::RoomSpec {
                    level: r.get(0)?,
                    name: r.get(1)?,
                    number: r.get(2)?,
                    point: (r.get(3)?, r.get(4)?),
                    height: r.get(5)?,
                })
            },
        );
        let Ok(before) = got else { return };
        let mut after = before.clone();
        f(&mut after);
        if after == before {
            return;
        }
        match self
            .model
            .update_room(id, &after)
            .and_then(|_| self.reload())
        {
            Ok(()) => {
                self.status = Some(crate::tr("curtain_wall.cw.room_edited", &[("id", &id.to_string())]));
                self.push(Cmd::Room(id, after));
            }
            Err(e) => self.status = Some(crate::tr("curtain_wall.cw.not_changed", &[("e", &e.to_string())])),
        }
    }

    /// Slab slope in percent; the direction is the previous one or along the longest edge.
    pub fn set_slab_slope_percent(&mut self, id: i64, pct: f64) {
        let Some(s) = self.slab(id) else { return };
        let Some(after) = slope_for_percent(s, pct) else {
            self.status = Some(crate::tr("curtain_wall.cw.slope_range", &[]));
            return;
        };
        if after == s.slope {
            return;
        }
        match self
            .model
            .set_slab_slope(id, after)
            .and_then(|_| self.reload())
        {
            Ok(()) => {
                self.status = Some(crate::tr(
                    "curtain_wall.cw.slope_set",
                    &[
                        ("id", &id.to_string()),
                        ("pct", &crate::units::fmt_percent(pct)),
                    ],
                ));
                self.push(Cmd::SlabSlope(id, after));
            }
            Err(e) => self.status = Some(crate::tr("curtain_wall.cw.not_changed", &[("e", &e.to_string())])),
        }
    }

    /// Replay of curtain wall and slab commands; `None` means the command is not from here.
    pub(super) fn cw_redo(&mut self, c: &Cmd) -> Option<Result<Cmd, ddc_model::Error>> {
        Some(match c {
            Cmd::AddCurtains(specs, old) => self.apply_add_curtains(specs).map(|new| {
                let map = |id: &mut i64| {
                    if let Some(k) = old.iter().position(|o| o == id) {
                        *id = new[k];
                    }
                };
                for cmd in &mut self.redo {
                    match cmd {
                        Cmd::EditCurtain(id, _) | Cmd::Panel(id, ..) => map(id),
                        Cmd::Items(items) => super::delete::remap(items, "curtain_wall", map),
                        _ => {}
                    }
                }
                Cmd::AddCurtains(specs.clone(), new)
            }),
            Cmd::EditCurtain(id, after) => self
                .model
                .set_curtain_wall(*id, after)
                .and_then(|_| self.reload())
                .map(|_| Cmd::EditCurtain(*id, after.clone())),
            Cmd::Panel(id, i, j, p) => self
                .model
                .set_curtain_panel(*id, *i, *j, p)
                .and_then(|_| self.reload())
                .map(|_| Cmd::Panel(*id, *i, *j, p.clone())),
            Cmd::SlabSlope(id, after) => self
                .model
                .set_slab_slope(*id, *after)
                .and_then(|_| self.reload())
                .map(|_| Cmd::SlabSlope(*id, *after)),
            Cmd::Room(id, after) => self
                .model
                .update_room(*id, after)
                .and_then(|_| self.reload())
                .map(|_| Cmd::Room(*id, after.clone())),
            _ => return None,
        })
    }
}

/// Stable id of a panel type (what is stored and what the schedule keys on).
pub fn panel_key(k: PanelKind) -> &'static str {
    k.as_str()
}

/// Panel type label in an explicit language.
pub fn panel_label_in(k: PanelKind, ru: bool) -> String {
    crate::tr_lang(ru, &format!("curtain_props.cw.kind.{}", panel_key(k)), &[])
}

/// Panel type from typed text: the stable key or the label in either language.
pub fn panel_kind_parse(text: &str) -> Option<PanelKind> {
    let t = text.trim().to_lowercase();
    PANEL_KINDS.iter().copied().find(|k| {
        t == panel_key(*k)
            || t == panel_label_in(*k, false).to_lowercase()
            || t == panel_label_in(*k, true).to_lowercase()
    })
}

pub const PANEL_KINDS: [PanelKind; 5] = [
    PanelKind::Glazed,
    PanelKind::Solid,
    PanelKind::Door,
    PanelKind::Window,
    PanelKind::Empty,
];

/// Grid step for display: a step or "by lines".
pub fn grid_step(g: &GridLines) -> Option<f64> {
    match g {
        GridLines::Spacing(s) => Some(*s),
        GridLines::At(_) => None,
    }
}

pub fn panel_of(c: &CurtainItem, k: u32) -> Option<(&ddc_geom::curtain::Cell, PanelSpec)> {
    let cell = c.layout.layout.cells.get(k as usize)?;
    Some((
        cell,
        c.layout.panels.get(k as usize).cloned().unwrap_or_default(),
    ))
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use ddc_render::TAG_GLASS;

    const QA12B_KEYS: [&str; 10] = [
        "curtain_wall.cw.issue.slab",
        "curtain_wall.cw.issue.body",
        "curtain_wall.cw.added",
        "curtain_wall.cw.add_failed",
        "curtain_wall.cw.edited_warn",
        "curtain_wall.cw.edited",
        "curtain_wall.cw.not_changed",
        "curtain_wall.cw.room_edited",
        "curtain_wall.cw.slope_range",
        "curtain_wall.cw.slope_set",
    ];

    fn has_cyrillic(t: &str) -> bool {
        t.chars().any(|c| ('\u{0400}'..='\u{04FF}').contains(&c))
    }

    #[test]
    fn cw_status_en_has_no_cyrillic() {
        for k in QA12B_KEYS {
            let t = crate::tr_lang(false, k, &[]);
            assert!(!has_cyrillic(&t), "{k}: {t}");
        }
        assert_eq!(
            crate::tr_lang(false, "curtain_wall.cw.added", &[("n", "1")]),
            "Curtain walls added: 1"
        );
    }

    #[test]
    fn cw_status_ru_unchanged_text() {
        let ru = |k: &str, a: &[(&str, &str)]| crate::tr_lang(true, k, a);
        let cases: [(&str, &[(&str, &str)]); 10] = [
            ("curtain_wall.cw.added", &[("n", "2")]),
            ("curtain_wall.cw.add_failed", &[("e", "x")]),
            ("curtain_wall.cw.edited_warn", &[("id", "3"), ("w", "w")]),
            ("curtain_wall.cw.edited", &[("id", "3")]),
            ("curtain_wall.cw.not_changed", &[("e", "x")]),
            ("curtain_wall.cw.room_edited", &[("id", "4")]),
            ("curtain_wall.cw.slope_range", &[]),
            ("curtain_wall.cw.slope_set", &[("id", "5"), ("pct", "2%")]),
            ("curtain_wall.cw.issue.slab", &[("id", "6"), ("e", "x")]),
            ("curtain_wall.cw.issue.body", &[("id", "7"), ("e", "x")]),
        ];
        for (k, a) in cases {
            let want = crate::app2_fx::fx(&format!("cw_ru_{k}"));
            assert_eq!(ru(k, a), want, "{k}");
        }
    }

    #[test]
    fn qa12b_keys_both_languages() {
        for k in QA12B_KEYS {
            let en = crate::tr_lang(false, k, &[]);
            let ru = crate::tr_lang(true, k, &[]);
            assert_ne!(en, k, "en missing {k}");
            assert_ne!(ru, k, "ru missing {k}");
            assert_ne!(en, ru, "{k} untranslated");
        }
    }

    #[test]
    fn cw_panel_labels_use_e3_keys() {
        for k in PANEL_KINDS {
            for ru in [false, true] {
                let key = format!("curtain_props.cw.kind.{}", panel_key(k));
                let v = crate::tr_lang(ru, &key, &[]);
                assert_ne!(v, key, "missing {key}");
                assert_eq!(panel_label_in(k, ru), v);
                let dup = format!("qa12d.cw.panel.{}", panel_key(k));
                assert_eq!(crate::tr_lang(ru, &dup, &[]), dup, "duplicate key {dup}");
            }
        }
    }

    #[test]
    fn panel_kind_parses_en_ru_and_key() {
        for k in PANEL_KINDS {
            assert_eq!(panel_kind_parse(panel_key(k)), Some(k));
            assert_eq!(panel_kind_parse(&panel_label_in(k, false)), Some(k));
            assert_eq!(panel_kind_parse(&panel_label_in(k, true)), Some(k));
            assert!(!has_cyrillic(&panel_label_in(k, false)));
        }
        assert_eq!(panel_kind_parse("  door "), Some(PanelKind::Door));
        assert_eq!(panel_kind_parse("nonsense"), None);
    }

    #[test]
    fn cw_default_name_en() {
        let en = crate::tr_lang(false, "curtain_wall.cw.default_name", &[]);
        assert!(!en.is_empty() && !has_cyrillic(&en), "{en}");
        assert_ne!(en, crate::tr_lang(true, "curtain_wall.cw.default_name", &[]));
        let (e, id) = facade();
        assert_eq!(
            e.curtain(id).unwrap().spec().name,
            crate::tr("curtain_wall.cw.default_name", &[])
        );
    }

    fn facade() -> (Editor, i64) {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&[(v2(0.0, 0.0), v2(6.0, 0.0), 0.0)]);
        let id = e.curtains[0].id;
        (e, id)
    }

    /// Seeded curtain walls on a square, a fifth of them arcs.
    pub(crate) fn random_segs(n: usize, side: f64, seed: u64) -> Vec<(V2, V2, f64)> {
        let mut st = seed;
        let mut rnd = move || {
            st = st.wrapping_mul(6364136223846793005).wrapping_add(1442695040888963407);
            (st >> 33) as f64 / (1u64 << 31) as f64
        };
        (0..n)
            .map(|i| {
                let (x, y) = (rnd() * side, rnd() * side);
                let (a, t) = (rnd() * std::f64::consts::TAU, 1.0 + rnd() * 5.0);
                let bulge = if i % 5 == 0 { 0.3 } else { 0.0 };
                (v2(x, y), v2(x + a.cos() * t, y + a.sin() * t), bulge)
            })
            .collect()
    }

    /// The full scan the index replaced.
    fn cw_pick_scan(e: &Editor, p: V2) -> Option<(Sel, f64)> {
        let r = e.tool.radius;
        e.curtains
            .iter()
            .filter_map(|c| cw_hit(c, p, r))
            .min_by(|a, b| a.1.total_cmp(&b.1))
    }

    #[test]
    fn cw_pick_matches_linear_scan() {
        let mut e = Editor::new_in_memory().unwrap();
        e.add_curtains(&random_segs(2000, 120.0, 7));
        assert!(e.curtains.len() > 1500, "{}", e.curtains.len());
        e.tool.radius = 0.4;
        let mut hits = 0;
        for (q, _, _) in random_segs(500, 120.0, 99) {
            let want = cw_pick_scan(&e, q);
            hits += want.is_some() as usize;
            let got = e.cw_pick(q).filter(|g| matches!(g.0, Sel::Curtain(_)));
            assert_eq!(got, want, "at {q:?}");
        }
        assert!(hits > 20, "test points never hit a wall: {hits}");
    }

    #[test]
    fn index_rebuilds_only_on_revision_or_level_change() {
        use std::sync::atomic::Ordering::Relaxed;
        let mut e = Editor::new_in_memory().unwrap();
        let l0 = e.level;
        e.add_curtains(&random_segs(50, 30.0, 3));
        let l1 = e.add_level("L2", 3.0).unwrap();
        e.set_level(l0);
        let p = v2(5.0, 5.0);
        e.cw_pick(p);
        let b = e.pick_cw.builds.load(Relaxed);
        for _ in 0..20 {
            e.cw_pick(p);
        }
        assert_eq!(e.pick_cw.builds.load(Relaxed), b);
        e.reload().unwrap();
        e.cw_pick(p);
        assert_eq!(e.pick_cw.builds.load(Relaxed), b + 1);
        e.set_level(l1);
        e.cw_pick(p);
        e.cw_pick(p);
        assert_eq!(e.pick_cw.builds.load(Relaxed), b + 2);
    }

    #[test]
    fn pick_index_follows_edit_undo_and_redo() {
        let mut e = Editor::new_in_memory().unwrap();
        e.tool.radius = 0.3;
        let p = v2(50.0, 0.0);
        let is_cw = |e: &Editor| e.cw_pick(p).is_some_and(|g| matches!(g.0, Sel::Curtain(_)));
        assert!(!is_cw(&e), "empty model");
        e.add_curtains(&[(v2(48.0, 0.0), v2(52.0, 0.0), 0.0)]);
        assert!(is_cw(&e), "a cached index must see the added wall");
        e.undo();
        assert!(e.curtains.is_empty());
        assert!(!is_cw(&e), "undone wall still picked from a stale index");
        e.redo();
        assert!(is_cw(&e), "redone wall not picked");
        // matches the full scan after each step
        assert_eq!(e.cw_pick(p).filter(|g| matches!(g.0, Sel::Curtain(_))), cw_pick_scan(&e, p));
    }

    #[test]
    fn curtain_tool_quantities_selection_and_scene() {
        let (mut e, id) = facade();
        // 6 × 3 m, step 1.5 × 1: 4 × 3 cells, mullions 50 × 150
        let q = &e.curtain(id).unwrap().qty;
        assert_eq!((q.cells, q.glazed), (12, 12));
        assert!((q.gross_area - 18.0).abs() < 1e-9);
        assert!((q.glass_area - 16.1).abs() < 1e-9, "{}", q.glass_area);
        let (lo, hi) = e.element_range(Sel::Curtain(id)).unwrap();
        assert_eq!(hi - lo, 13);
        assert_eq!(e.sel_of_element(lo), Some(Sel::Curtain(id)));
        assert_eq!(e.sel_of_element(lo + 3), Some(Sel::Panel(id, 2)));
        let s = e.scene();
        let glass = s.meshes.iter().filter(|m| m.tag == TAG_GLASS).count();
        assert_eq!(glass, 12);
        assert!(s
            .meshes
            .iter()
            .any(|m| m.element == lo && m.tag == TAG_MULLION));
        // panel to door, undo, redo
        e.set_panel_kind(id, 1, PanelKind::Door);
        assert_eq!(e.curtain(id).unwrap().qty.doors, 1);
        e.undo();
        assert_eq!(e.curtain(id).unwrap().qty.doors, 0);
        e.redo();
        assert_eq!(e.curtain(id).unwrap().qty.doors, 1);
    }

    #[test]
    fn curtain_redo_remaps_ids_for_later_edits() {
        let (mut e, id) = facade();
        e.edit_curtain(id, |s| s.u = GridLines::Spacing(2.0));
        assert_eq!(e.curtain(id).unwrap().qty.cells, 9);
        e.undo();
        e.undo();
        assert!(e.curtains.is_empty());
        e.redo();
        e.redo();
        assert_eq!(e.curtains.len(), 1);
        assert_eq!(e.curtains[0].qty.cells, 9);
        e.selection = vec![Sel::Curtain(e.curtains[0].id)];
        e.delete_selection();
        assert!(e.curtains.is_empty());
    }

    #[test]
    fn sloped_slab_cutters_follow_the_plane() {
        let ring = vec![v2(0.0, 0.0), v2(6.0, 0.0), v2(6.0, 4.0), v2(0.0, 4.0)];
        assert_eq!(slab_cutters(&[ring.clone()], 3.0, 0.2, None).len(), 1);
        let pl = SlabSlope::arrow(v2(0.0, 0.0), v2(6.0, 0.0), 0.3).unwrap(); // 5 %
        let cs = slab_cutters(&[ring], 3.0, 0.2, Some(pl));
        assert!(cs.len() > 10);
        let area: f64 = cs
            .iter()
            .map(|c| ddc_geom::profile::signed_area(&c.ring).abs())
            .sum();
        assert!((area - 24.0).abs() < 1e-9, "{area}");
        for c in &cs {
            let xm = c.ring.iter().map(|p| p.x).sum::<f64>() / c.ring.len() as f64;
            assert!((c.z1 - (3.0 + 0.05 * xm)).abs() < 1e-9);
            assert!((c.z1 - c.z0 - 0.2).abs() < 1e-12);
        }
    }

    #[test]
    fn slab_slope_percent_is_one_undoable_op_and_meshed() {
        let mut e = Editor::new_in_memory().unwrap();
        let st = e
            .model
            .add_slab_type(
                "P",
                &[ddc_model::LayerSpec {
                    material: "c".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let id = e
            .model
            .add_slab(&ddc_model::SlabSpec {
                level: e.level,
                slab_type: st,
                offset: 3.0,
                outer: vec![(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)],
                holes: vec![],
            })
            .unwrap();
        e.reload().unwrap();
        e.set_slab_slope_percent(id, 5.0);
        let s = e.slab(id).unwrap();
        assert!((s.slope_percent() - 5.0).abs() < 1e-9);
        assert!((s.surface_area() - 24.0 * (1.0f64 + 0.0025).sqrt()).abs() < 1e-9);
        let sc = e.scene();
        let m = sc.meshes.iter().find(|m| m.tag == TAG_SLAB).unwrap();
        let zmax = m.positions.iter().map(|p| p[2]).fold(f32::MIN, f32::max);
        assert!((zmax - 3.3).abs() < 1e-4, "{zmax}");
        assert_eq!(e.cw_pick(v2(3.0, 2.0)).map(|h| h.0), Some(Sel::Slab(id)));
        e.undo();
        assert!(e.slab(id).unwrap().slope.is_none());
    }
}
