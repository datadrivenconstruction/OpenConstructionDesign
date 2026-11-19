// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Derived typed solids: quantities and meshes (cached by definition content), subtraction of
//! the union of voids from hosts. The deduction starts from the host's original quantities (`solid_base`), so
//! repeating regeneration without rebuilding the host (an invalid slab) accumulates nothing.

use std::collections::hash_map::DefaultHasher;
use std::collections::{HashMap, HashSet};
use std::hash::{Hash, Hasher};
use std::sync::{Arc, Mutex};

use ddc_geom::gsolid::{self, Body, Form, Frame, Qty, Section};
use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, v3, Tol, V2, V3};
use rusqlite::{params, types::Value, Connection, OptionalExtension};

use super::{elev, form_json, frame_json, hosts_of, ids, read, SolidSpec};
use crate::ops::Inv;
use crate::Error;

// ---------- solid cache ----------

/// A solid's quantities and mesh (an error is the reason text).
pub(crate) struct Built {
    pub qty: Result<Qty, String>,
    pub mesh: Result<Arc<Mesh>, String>,
}

const CACHE_MAX: usize = 8192;

static BUILT: Mutex<Option<HashMap<u64, Arc<Built>>>> = Mutex::new(None);
static PROTO: Mutex<Option<HashMap<u64, Arc<Built>>>> = Mutex::new(None);
static CUTS: Mutex<Option<HashMap<u64, f64>>> = Mutex::new(None);

fn spec_key(s: &SolidSpec, z: f64) -> u64 {
    let mut h = DefaultHasher::new();
    form_json(&s.form).to_string().hash(&mut h);
    frame_json(&s.frame).to_string().hash(&mut h);
    z.to_bits().hash(&mut h);
    h.finish()
}

fn cached<T: Clone>(m: &Mutex<Option<HashMap<u64, T>>>, k: u64, f: impl FnOnce() -> T) -> T {
    if let Some(v) = m
        .lock()
        .ok()
        .and_then(|g| g.as_ref().and_then(|c| c.get(&k).cloned()))
    {
        return v;
    }
    let v = f();
    if let Ok(mut g) = m.lock() {
        let c = g.get_or_insert_with(HashMap::new);
        if c.len() >= CACHE_MAX {
            c.clear();
        }
        c.insert(k, v.clone());
    }
    v
}

/// A solid by specification: editing one solid rebuilds only it.
pub(crate) fn built(db: &Connection, s: &SolidSpec) -> Result<Arc<Built>, Error> {
    let z = elev(db, s.level)?;
    Ok(cached(&BUILT, spec_key(s, z), || {
        let mut fr = s.frame;
        fr.o.z += z;
        // the mesh maps its points through the frame one by one: the form's own mesh, placed
        let p = proto(&s.form);
        Arc::new(Built {
            qty: p.qty.clone(),
            mesh: p.mesh.as_ref().map(|m| Arc::new(placed(m, &fr))).map_err(Clone::clone),
        })
    }))
}

/// Quantities and mesh of a form in its own frame, shared by every solid of that form (the
/// bolts of a thousand connections tessellate once).
pub(crate) fn proto(f: &Form) -> Arc<Built> {
    proto_keyed(f).1
}

/// [`proto`] with its key (hash of the form's definition).
pub(crate) fn proto_keyed(f: &Form) -> (u64, Arc<Built>) {
    let mut h = DefaultHasher::new();
    form_json(f).to_string().hash(&mut h);
    let key = h.finish();
    let b = cached(&PROTO, key, || {
        Arc::new(Built {
            qty: gsolid::quantities(f).map_err(|e| e.to_string()),
            mesh: gsolid::mesh(f, &Frame::default(), gsolid::VIEW_SAG)
                .map(Arc::new)
                .map_err(|e| e.to_string()),
        })
    });
    (key, b)
}

/// A form's mesh placed by `fr`.
pub(crate) fn placed(m: &Mesh, fr: &Frame) -> Mesh {
    Mesh {
        positions: m.positions.iter().map(|&q| fr.at(q)).collect(),
        triangles: m.triangles.clone(),
        face_of: m.face_of.clone(),
    }
}

fn geom_text(e: &str) -> Error {
    Error::Degenerate(format!("solid: {e}"))
}

pub(crate) fn mesh_of(db: &Connection, s: &SolidSpec) -> Result<Arc<Mesh>, Error> {
    built(db, s)?.mesh.clone().map_err(|e| geom_text(&e))
}

// ---------- deduction input hash ----------

struct Key(DefaultHasher);

impl Key {
    fn new(tag: &str) -> Key {
        let mut h = DefaultHasher::new();
        tag.hash(&mut h);
        Key(h)
    }
    fn f(&mut self, x: f64) {
        x.to_bits().hash(&mut self.0);
    }
    fn body(&mut self, b: &Body) {
        match b {
            Body::Prism { rings, z0, z1 } => {
                0u8.hash(&mut self.0);
                for r in rings.iter() {
                    r.len().hash(&mut self.0);
                    for p in r {
                        self.f(p.x);
                        self.f(p.y);
                    }
                }
                self.f(*z0);
                self.f(*z1);
            }
            Body::Mesh(m) => {
                1u8.hash(&mut self.0);
                m.triangles.hash(&mut self.0);
                for p in &m.positions {
                    self.f(p.x);
                    self.f(p.y);
                    self.f(p.z);
                }
            }
        }
    }
    fn bodies(&mut self, bs: &[Body]) {
        bs.len().hash(&mut self.0);
        for b in bs {
            self.body(b);
        }
    }
}

/// Computation memoized by solid content: unchanged hosts are not recomputed.
fn memo(tag: &str, host: &[Body], minus: &[Body], voids: &[Body], f: impl FnOnce() -> f64) -> f64 {
    let mut k = Key::new(tag);
    k.bodies(host);
    k.bodies(minus);
    k.bodies(voids);
    cached(&CUTS, k.0.finish(), f)
}

// ---------- hosts ----------

/// Volume already removed from a host: openings, shafts, columns, the region above a polyline top.
enum Minus {
    Prism(Vec<Vec<V2>>, f64, f64),
    Mesh(Mesh),
}

impl Minus {
    fn body(&self) -> Body<'_> {
        match self {
            Minus::Prism(r, z0, z1) => Body::Prism {
                rings: r,
                z0: *z0,
                z1: *z1,
            },
            Minus::Mesh(m) => Body::Mesh(m),
        }
    }
}

struct Host {
    kind: &'static str,
    id: i64,
    /// Prism layers (pos, rings, z0, z1); rings are even-odd: layer pieces and holes.
    prisms: Vec<(i64, Vec<Vec<V2>>, f64, f64)>,
    /// Otherwise a closed mesh and the layer shares (pos, thickness).
    mesh: Option<Arc<Mesh>>,
    layers: Vec<(i64, f64)>,
    bbox: (V3, V3),
    minus: Vec<Minus>,
}

fn bb_overlap(a: (V3, V3), b: (V3, V3)) -> bool {
    a.0.x < b.1.x
        && b.0.x < a.1.x
        && a.0.y < b.1.y
        && b.0.y < a.1.y
        && a.0.z < b.1.z
        && b.0.z < a.1.z
}

fn grow(a: (V3, V3), b: (V3, V3)) -> (V3, V3) {
    (
        v3(a.0.x.min(b.0.x), a.0.y.min(b.0.y), a.0.z.min(b.0.z)),
        v3(a.1.x.max(b.1.x), a.1.y.max(b.1.y), a.1.z.max(b.1.z)),
    )
}

fn ring_box(rings: &[Vec<V2>], z0: f64, z1: f64) -> (V3, V3) {
    let mut lo = v3(f64::INFINITY, f64::INFINITY, z0);
    let mut hi = v3(f64::NEG_INFINITY, f64::NEG_INFINITY, z1);
    for p in rings.iter().flatten() {
        lo = v3(lo.x.min(p.x), lo.y.min(p.y), z0);
        hi = v3(hi.x.max(p.x), hi.y.max(p.y), z1);
    }
    (lo, hi)
}

fn pts(r: &[(f64, f64)]) -> Vec<V2> {
    r.iter().map(|p| v2(p.0, p.1)).collect()
}

/// A grid index of void extents in plan: finds candidates for a host without scanning all of them.
pub(crate) struct Grid {
    cell: f64,
    cells: HashMap<(i64, i64), Vec<usize>>,
    wide: Vec<usize>,
    boxes: Vec<(V3, V3)>,
}

impl Grid {
    pub(crate) fn new(boxes: Vec<(V3, V3)>) -> Grid {
        let mut sizes: Vec<f64> = boxes
            .iter()
            .map(|b| (b.1.x - b.0.x).max(b.1.y - b.0.y))
            .filter(|s| s.is_finite())
            .collect();
        sizes.sort_by(f64::total_cmp);
        let cell = sizes
            .get(sizes.len() / 2)
            .copied()
            .unwrap_or(1.0)
            .clamp(0.5, 50.0);
        let mut g = Grid {
            cell,
            cells: HashMap::new(),
            wide: vec![],
            boxes,
        };
        for i in 0..g.boxes.len() {
            let Some((x0, y0, x1, y1)) = g.span(g.boxes[i]) else {
                g.wide.push(i);
                continue;
            };
            if ((x1 - x0 + 1) as f64) * ((y1 - y0 + 1) as f64) > 4096.0 {
                g.wide.push(i);
                continue;
            }
            for x in x0..=x1 {
                for y in y0..=y1 {
                    g.cells.entry((x, y)).or_default().push(i);
                }
            }
        }
        g
    }

    fn span(&self, b: (V3, V3)) -> Option<(i64, i64, i64, i64)> {
        let c = |v: f64| (v / self.cell).floor();
        let (x0, y0, x1, y1) = (c(b.0.x), c(b.0.y), c(b.1.x), c(b.1.y));
        if ![x0, y0, x1, y1]
            .iter()
            .all(|v| v.is_finite() && v.abs() < 1e12)
        {
            return None;
        }
        Some((x0 as i64, y0 as i64, x1 as i64, y1 as i64))
    }

    /// Voids whose extent intersects `b` (in ascending number order).
    pub(crate) fn query(&self, b: (V3, V3)) -> Vec<usize> {
        let mut out: HashSet<usize> = self.wide.iter().copied().collect();
        match self.span(b) {
            Some((x0, y0, x1, y1))
                if ((x1 - x0 + 1) as f64) * ((y1 - y0 + 1) as f64) <= 65536.0 =>
            {
                for x in x0..=x1 {
                    for y in y0..=y1 {
                        if let Some(v) = self.cells.get(&(x, y)) {
                            out.extend(v.iter().copied());
                        }
                    }
                }
            }
            _ => out.extend(0..self.boxes.len()),
        }
        let mut v: Vec<usize> = out
            .into_iter()
            .filter(|&i| bb_overlap(self.boxes[i], b))
            .collect();
        v.sort_unstable();
        v
    }

    pub(crate) fn any(&self, b: (V3, V3)) -> bool {
        !self.query(b).is_empty()
    }
}

/// Slab layers top to bottom: (pos, thickness).
fn slab_layers(db: &Connection, stype: i64) -> Result<Vec<(i64, f64)>, Error> {
    let mut st = db
        .prepare_cached("SELECT pos, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos")?;
    let r = st.query_map([stype], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

struct WallRow {
    id: i64,
    level: i64,
    elev: f64,
    a: V2,
    b: V2,
    base: f64,
    height: f64,
    bulge: f64,
    top: Option<Vec<(f64, f64)>>,
    offset0: Option<f64>,
    thick: f64,
}

fn wall_rows(db: &Connection) -> Result<Vec<WallRow>, Error> {
    let mut st = db.prepare(
        "SELECT w.id, w.level, l.elevation, w.ax, w.ay, w.bx, w.by, w.base_offset, w.height, \
         w.bulge, w.top_profile, w.offset0, \
         (SELECT COALESCE(SUM(t.thickness), 0) FROM wall_type_layer t WHERE t.wtype=w.wtype) \
         FROM wall w JOIN level l ON l.id=w.level ORDER BY w.id",
    )?;
    let rows = st.query_map([], |r| {
        let top: Option<String> = r.get(10)?;
        Ok(WallRow {
            id: r.get(0)?,
            level: r.get(1)?,
            elev: r.get(2)?,
            a: v2(r.get(3)?, r.get(4)?),
            b: v2(r.get(5)?, r.get(6)?),
            base: r.get(7)?,
            height: r.get(8)?,
            bulge: r.get(9)?,
            top: top.as_deref().and_then(crate::walls::parse_top_profile),
            offset0: r.get(11)?,
            thick: r.get(12)?,
        })
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

impl WallRow {
    fn z0(&self) -> f64 {
        self.elev + self.base
    }
    fn top_max(&self) -> f64 {
        let h = self
            .top
            .iter()
            .flatten()
            .map(|p| p.1)
            .fold(self.height, f64::max);
        self.z0() + h
    }
    /// Half-width of the strip around the axis, certain to cover the layers.
    fn reach(&self) -> f64 {
        self.thick + self.offset0.unwrap_or(0.0).abs() + 0.5
    }
    fn bbox(&self) -> (V3, V3) {
        let len = (self.b - self.a).len();
        let m = self.reach() + self.bulge.abs() * len * 0.5;
        (
            v3(
                self.a.x.min(self.b.x) - m,
                self.a.y.min(self.b.y) - m,
                self.z0(),
            ),
            v3(
                self.a.x.max(self.b.x) + m,
                self.a.y.max(self.b.y) + m,
                self.top_max(),
            ),
        )
    }
    fn dirs(&self) -> Option<(V2, V2, f64)> {
        let d = self.b - self.a;
        let l = d.len();
        (l > 1e-9).then(|| {
            let d = d * (1.0 / l);
            (d, v2(-d.y, d.x), l)
        })
    }

    /// Openings of a straight wall: prisms across the full thickness.
    fn openings(&self, db: &Connection) -> Result<Vec<Minus>, Error> {
        let Some((d, n, _)) = self.dirs().filter(|_| self.bulge == 0.0) else {
            return Ok(vec![]);
        };
        let t = self.reach();
        let mut st = db.prepare_cached(
            "SELECT along, width, sill, height FROM opening WHERE wall=?1 ORDER BY id",
        )?;
        let rows: Vec<(f64, f64, f64, f64)> = st
            .query_map([self.id], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?))
            })?
            .collect::<Result<_, _>>()?;
        Ok(rows
            .into_iter()
            .filter(|o| o.1 > 0.0 && o.3 > 0.0)
            .map(|(along, width, sill, height)| {
                let p = |u: f64, s: f64| self.a + d * u + n * s;
                let ring = vec![
                    p(along, -t),
                    p(along + width, -t),
                    p(along + width, t),
                    p(along, t),
                ];
                let z = self.z0() + sill;
                Minus::Prism(vec![ring], z, z + height)
            })
            .collect())
    }

    /// The region above a straight wall's polyline top (removed from the wall volume).
    fn above_top(&self) -> Option<Minus> {
        let top = self
            .top
            .as_ref()
            .filter(|t| !t.is_empty() && self.bulge == 0.0)?;
        let (d, _, len) = self.dirs()?;
        let t = self.reach();
        let (first, last) = (top[0], top[top.len() - 1]);
        let (u0, u1) = (first.0.min(0.0) - t - 1.0, last.0.max(len) + t + 1.0);
        let hmax = self.top_max() - self.z0() + 1.0;
        let mut ring: Vec<V2> = vec![v2(u0, first.1)];
        ring.extend(top.iter().map(|p| v2(p.0, p.1)));
        ring.extend([v2(u1, last.1), v2(u1, hmax), v2(u0, hmax)]);
        ring.dedup_by(|a, b| (*a - *b).len() < 1e-9);
        // points collinear with their neighbours (a horizontal top) are not needed by the outline
        let n = ring.len();
        let keep: Vec<bool> = (0..n)
            .map(|i| {
                let (p, q, r) = (ring[(i + n - 1) % n], ring[i], ring[(i + 1) % n]);
                (q - p).cross(r - q).abs() > 1e-12
            })
            .collect();
        let ring: Vec<V2> = ring
            .into_iter()
            .zip(keep)
            .filter_map(|(p, k)| k.then_some(p))
            .collect();
        if ring.len() < 3 {
            return None;
        }
        let fr = Frame::vertical(v3(self.a.x, self.a.y, self.z0()), d);
        let f = Form::Extrusion {
            profile: Section::polygon(ring),
            start: -t,
            end: t,
        };
        gsolid::mesh(&f, &fr, gsolid::VIEW_SAG)
            .ok()
            .map(Minus::Mesh)
    }
}

fn wall_hosts(
    db: &Connection,
    grid: &Grid,
    cols: &[(Vec<V2>, f64, f64, bool)],
) -> Result<Vec<Host>, Error> {
    let rows: Vec<WallRow> = wall_rows(db)?
        .into_iter()
        .filter(|w| grid.any(w.bbox()))
        .collect();
    let mut levels: Vec<(i64, f64)> = rows.iter().map(|w| (w.level, w.elev)).collect();
    levels.sort_by_key(|l| l.0);
    levels.dedup_by_key(|l| l.0);
    let mut out: Vec<Host> = vec![];
    for (lv, e) in levels {
        let lw = match crate::walls::level_walls(db, lv, e) {
            Ok(lw) => lw,
            Err(Error::Geom(_)) => continue,
            Err(e) => return Err(e),
        };
        for (wall, pos, rings, z0, z1) in lw.layers {
            let Some(w) = rows.iter().find(|w| w.id == wall) else {
                continue;
            };
            if rings.is_empty() || z1 <= z0 {
                continue;
            }
            // a polyline top can rise above the wall height: the cut is by `above_top`
            let z1 = if w.top.is_some() {
                z1.max(w.top_max())
            } else {
                z1
            };
            let bb = ring_box(&rings, z0, z1);
            match out.iter_mut().find(|h| h.id == wall) {
                Some(h) => {
                    h.bbox = grow(h.bbox, bb);
                    h.prisms.push((pos, rings, z0, z1));
                }
                None => {
                    let mut minus = w.openings(db)?;
                    minus.extend(w.above_top());
                    for (sec, c0, c1, _) in cols {
                        let cb = ring_box(std::slice::from_ref(sec), *c0, *c1);
                        if bb_overlap(cb, w.bbox()) {
                            minus.push(Minus::Prism(vec![sec.clone()], *c0, *c1));
                        }
                    }
                    out.push(Host {
                        kind: "wall",
                        id: wall,
                        prisms: vec![(pos, rings, z0, z1)],
                        mesh: None,
                        layers: vec![],
                        bbox: bb,
                        minus,
                    });
                }
            }
        }
    }
    Ok(out)
}

fn slab_hosts(
    db: &Connection,
    grid: &Grid,
    shafts: &[(Vec<V2>, f64, f64)],
    cols: &[(Vec<V2>, f64, f64, bool)],
) -> Result<Vec<Host>, Error> {
    let rows: Vec<(i64, i64, f64, Option<String>)> = {
        let mut st = db.prepare(
            "SELECT s.id, s.stype, l.elevation + s.offset, s.slope FROM slab s \
             JOIN level l ON l.id=s.level ORDER BY s.id",
        )?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)))?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for (id, stype, top, slope) in rows {
        // an invalid outline: the slab quantities were not rebuilt, so no cutting
        let rs = match crate::slabs::rings(db, id) {
            Ok(r) => r,
            Err(Error::Geom(_)) => continue,
            Err(e) => return Err(e),
        };
        let Some((outer, holes)) = rs.split_first() else {
            continue;
        };
        let Ok(p) = crate::slabs::saved_profile(outer, holes) else {
            continue;
        };
        let ls = slab_layers(db, stype)?;
        let t: f64 = ls.iter().map(|l| l.1).sum();
        if t <= 0.0 {
            continue;
        }
        let rings: Vec<Vec<V2>> = rs.iter().map(|r| pts(r)).collect();
        let plane = slope
            .as_deref()
            .map(crate::slabs::parse_slope)
            .transpose()?
            .filter(|p| !p.is_flat());
        let probe = ring_box(&rings, top - t - 50.0, top + 50.0);
        if !grid.any(probe) {
            continue;
        }
        let mut minus = vec![];
        for (ring, s0, s1) in shafts {
            if *s1 <= top - t - 50.0 || *s0 >= top + 50.0 {
                continue;
            }
            minus.push(Minus::Prism(vec![ring.clone()], *s0, *s1));
        }
        for (sec, c0, c1, structural) in cols {
            if *structural {
                minus.push(Minus::Prism(vec![sec.clone()], *c0, *c1));
            }
        }
        match plane {
            None => {
                // a shaft that touches the thickness is removed from the full thickness (see slabs::slab_rows)
                for m in &mut minus {
                    if let Minus::Prism(r, s0, s1) = m {
                        let shaft = shafts
                            .iter()
                            .any(|s| s.0 == r[0] && s.1 == *s0 && s.2 == *s1);
                        if shaft && *s1 > top - t + 1e-9 && *s0 < top - 1e-9 {
                            *s0 = top - t - 1.0;
                            *s1 = top + 1.0;
                        }
                    }
                }
                let mut z = top;
                let mut prisms = vec![];
                for (pos, th) in &ls {
                    prisms.push((*pos, rings.clone(), z - th, z));
                    z -= th;
                }
                out.push(Host {
                    kind: "slab",
                    id,
                    bbox: ring_box(&rings, top - t, top),
                    prisms,
                    mesh: None,
                    layers: ls,
                    minus,
                });
            }
            Some(pl) => {
                let Ok(m) = ddc_geom::slope::slab_mesh(&p, top, &pl, t, Tol::default()) else {
                    continue;
                };
                out.push(Host {
                    kind: "slab",
                    id,
                    bbox: gsolid::bbox(&m),
                    prisms: vec![],
                    mesh: Some(Arc::new(m)),
                    layers: ls,
                    minus,
                });
            }
        }
    }
    Ok(out)
}

fn roof_hosts(db: &Connection, grid: &Grid) -> Result<Vec<Host>, Error> {
    let rows: Vec<(i64, Option<f64>, Option<f64>, Option<f64>, Option<f64>, f64)> = {
        let mut st = db.prepare(
            "SELECT r.id, MIN(v.x), MIN(v.y), MAX(v.x), MAX(v.y), r.overhang FROM roof r \
             LEFT JOIN roof_vertex v ON v.roof=r.id GROUP BY r.id ORDER BY r.id",
        )?;
        let v = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for (id, x0, y0, x1, y1, over) in rows {
        // the outline is known: reject by plan (an extrusion without an outline is checked by the mesh)
        if let (Some(x0), Some(y0), Some(x1), Some(y1)) = (x0, y0, x1, y1) {
            let m = over.abs() + 1.0;
            let b = (v3(x0 - m, y0 - m, -1e9), v3(x1 + m, y1 + m, 1e9));
            if !grid.any(b) {
                continue;
            }
        }
        let s = crate::roofs::load(db, id)?;
        let ls = slab_layers(db, s.slab_type)?;
        let t: f64 = ls.iter().map(|l| l.1).sum();
        if t <= 0.0 {
            continue;
        }
        let Ok(g) = crate::roofs::geometry(db, &s) else {
            continue;
        };
        let m = g.mesh(t);
        let bbox = gsolid::bbox(&m);
        if !grid.any(bbox) {
            continue;
        }
        out.push(Host {
            kind: "roof",
            id,
            bbox,
            prisms: vec![],
            mesh: Some(Arc::new(m)),
            layers: ls,
            minus: vec![],
        });
    }
    Ok(out)
}

// ---------- original host quantities ----------

/// (host kind, quantities table, reference column, area column).
const QTY: [(&str, &str, &str, &str); 3] = [
    ("wall", "wall_qty", "wall", "plan_area"),
    ("slab", "slab_qty", "slab", "area"),
    ("roof", "roof_qty", "roof", "area"),
];

fn qty_table(kind: &str) -> Option<(&'static str, &'static str, &'static str)> {
    QTY.iter().find(|q| q.0 == kind).map(|q| (q.1, q.2, q.3))
}

fn same(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-12 * (1.0 + b.abs())
}

/// Quantity rows left over from the previous deduction (host not rebuilt) are restored to
/// their original values; rebuilt ones are already original.
fn restore_bases(db: &Connection) -> Result<(), Error> {
    type Row = (String, i64, i64, f64, f64, f64, f64);
    let rows: Vec<Row> = {
        let mut st = db
            .prepare("SELECT kind, ref, pos, volume, area, net_volume, net_area FROM solid_base")?;
        let v = st
            .query_map([], |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        v
    };
    for (kind, id, pos, bv, ba, nv, na) in rows {
        let Some((table, col, acol)) = qty_table(&kind) else {
            continue;
        };
        let cur: Option<(f64, f64)> = db
            .query_row(
                &format!("SELECT volume, {acol} FROM {table} WHERE {col}=?1 AND pos=?2"),
                params![id, pos],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()?;
        if let Some((v, a)) = cur {
            if same(v, nv) && same(a, na) {
                db.execute(
                    &format!("UPDATE {table} SET volume=?3, {acol}=?4 WHERE {col}=?1 AND pos=?2"),
                    params![id, pos, bv, ba],
                )?;
            }
        }
    }
    db.execute("DELETE FROM solid_base", [])?;
    Ok(())
}

/// Deduction from a quantity row, remembering the original values.
fn apply_cut(
    db: &Connection,
    kind: &str,
    id: i64,
    pos: i64,
    dv: f64,
    da: f64,
) -> Result<(), Error> {
    let Some((table, col, acol)) = qty_table(kind) else {
        return Ok(());
    };
    let cur: Option<(f64, f64)> = db
        .query_row(
            &format!("SELECT volume, {acol} FROM {table} WHERE {col}=?1 AND pos=?2"),
            params![id, pos],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .optional()?;
    let Some((v, a)) = cur else {
        return Ok(());
    };
    let (nv, na) = ((v - dv).max(0.0), (a - da).max(0.0));
    db.execute(
        &format!("UPDATE {table} SET volume=?3, {acol}=?4 WHERE {col}=?1 AND pos=?2"),
        params![id, pos, nv, na],
    )?;
    db.execute(
        "INSERT OR REPLACE INTO solid_base VALUES (?1,?2,?3,?4,?5,?6,?7)",
        params![kind, id, pos, v, a, nv, na],
    )?;
    Ok(())
}

// ---------- regeneration ----------

struct Void {
    id: i64,
    mesh: Arc<Mesh>,
    /// Explicit hosts (empty means all intersected ones).
    explicit: Vec<(String, i64)>,
}

impl Void {
    fn cuts(&self, h: &Host) -> bool {
        self.explicit.is_empty() || self.explicit.iter().any(|(k, r)| k == h.kind && *r == h.id)
    }
}

/// Writes the `solid_qty` row of solid `id`; its mesh and void flag when it builds.
fn qty_row(db: &Connection, id: i64) -> Result<Option<(Arc<Mesh>, bool)>, Error> {
    let res = read(db, id).and_then(|s| Ok((built(db, &s)?, s.void)));
    let (b, void) = match res {
        Ok(r) => r,
        Err(e) => {
            db.prepare_cached("INSERT INTO solid_qty VALUES (?1,0,0,0,0,0,0,0,0,0,?2)")?
                .execute(params![id, e.to_string()])?;
            return Ok(None);
        }
    };
    match (&b.qty, &b.mesh) {
        (Ok(q), Ok(m)) => {
            let (lo, hi) = gsolid::bbox(m);
            let net = if void { 0.0 } else { q.volume };
            db.prepare_cached("INSERT INTO solid_qty VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,NULL)")?
                .execute(params![id, q.volume, q.area, net, lo.x, lo.y, lo.z, hi.x, hi.y, hi.z])?;
            Ok(Some((m.clone(), void)))
        }
        (Err(e), _) | (_, Err(e)) => {
            db.prepare_cached("INSERT INTO solid_qty VALUES (?1,0,0,0,0,0,0,0,0,0,?2)")?
                .execute(params![id, geom_text(e).to_string()])?;
            Ok(None)
        }
    }
}

/// `solid_qty` of the solids `ids` alone (rows written or deleted by a connection rebuild).
/// A void or a cut base anywhere couples solids to each other: then every row is recomputed.
pub(crate) fn refresh(db: &Connection, ids: &[i64]) -> Result<(), Error> {
    if ids.is_empty() {
        return Ok(());
    }
    let coupled: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM solid WHERE void<>0) OR EXISTS(SELECT 1 FROM solid_base) \
         OR EXISTS(SELECT 1 FROM solid_cut)",
        [],
        |r| r.get(0),
    )?;
    if coupled {
        return regenerate(db);
    }
    for &id in ids {
        db.prepare_cached("DELETE FROM solid_qty WHERE solid=?1")?.execute([id])?;
        let live: bool = db
            .prepare_cached("SELECT EXISTS(SELECT 1 FROM solid WHERE id=?1)")?
            .query_row([id], |r| r.get(0))?;
        if live {
            qty_row(db, id)?;
        }
    }
    Ok(())
}

/// Regenerates `solid_qty` and `solid_cut`, subtracting the union of voids from hosts (after their
/// quantities are regenerated). A solid with a corrupt definition is skipped with an error in `solid_qty.error`.
pub(crate) fn regenerate(db: &Connection) -> Result<(), Error> {
    db.execute("DELETE FROM solid_qty", [])?;
    db.execute("DELETE FROM solid_cut", [])?;
    restore_bases(db)?;
    let mut solids: Vec<(i64, Arc<Mesh>)> = vec![];
    let mut voids: Vec<Void> = vec![];
    for id in ids(db, None)? {
        let Some((m, void)) = qty_row(db, id)? else { continue };
        if void {
            voids.push(Void {
                id,
                mesh: m,
                explicit: hosts_of(db, id)?,
            });
        } else {
            solids.push((id, m));
        }
    }
    if voids.is_empty() {
        return Ok(());
    }
    let grid = Grid::new(voids.iter().map(|v| gsolid::bbox(&v.mesh)).collect());
    let cols = crate::structure::column_prisms(db)?;
    let shafts = crate::shafts::prisms(db)?;
    let mut hosts = wall_hosts(db, &grid, &cols)?;
    hosts.extend(slab_hosts(db, &grid, &shafts, &cols)?);
    hosts.extend(roof_hosts(db, &grid)?);
    for (id, m) in &solids {
        let bbox = gsolid::bbox(m);
        if grid.any(bbox) {
            hosts.push(Host {
                kind: "solid",
                id: *id,
                bbox,
                prisms: vec![],
                mesh: Some(m.clone()),
                layers: vec![(0, 1.0)],
                minus: vec![],
            });
        }
    }
    for h in &hosts {
        cut_host(db, h, &grid, &voids)?;
    }
    Ok(())
}

fn cut_host(db: &Connection, h: &Host, grid: &Grid, voids: &[Void]) -> Result<(), Error> {
    let vs: Vec<&Void> = grid
        .query(h.bbox)
        .into_iter()
        .map(|i| &voids[i])
        .filter(|v| v.cuts(h) && !(h.kind == "solid" && v.id == h.id))
        .collect();
    if vs.is_empty() {
        return Ok(());
    }
    let minus: Vec<Body> = h.minus.iter().map(Minus::body).collect();
    let vb: Vec<Body> = vs.iter().map(|v| Body::Mesh(&v.mesh)).collect();
    let host_bodies: Vec<(i64, Body, f64, f64)> = if h.prisms.is_empty() {
        match &h.mesh {
            Some(m) => vec![(-1, Body::Mesh(m), 0.0, 0.0)],
            None => vec![],
        }
    } else {
        h.prisms
            .iter()
            .map(|(pos, rings, z0, z1)| {
                (
                    *pos,
                    Body::Prism {
                        rings,
                        z0: *z0,
                        z1: *z1,
                    },
                    *z0,
                    *z1,
                )
            })
            .collect()
    };
    // (pos, volume, area) per layer
    let mut parts: Vec<(i64, f64, f64)> = vec![];
    for (pos, body, z0, z1) in &host_bodies {
        let hb = [*body];
        let dv = memo("vol", &hb, &minus, &vb, || {
            gsolid::boolean_volume(&hb, &minus, &vb)
        });
        if dv <= 1e-9 {
            continue;
        }
        if *pos >= 0 {
            // the slab area shrinks only where a layer is cut through
            let da = if h.kind == "slab" {
                memo("thr", &hb, &minus, &vb, || {
                    gsolid::through_area(&hb, &minus, &vb, *z0, *z1)
                })
            } else {
                0.0
            };
            parts.push((*pos, dv, da));
        } else {
            let t: f64 = h.layers.iter().map(|l| l.1).sum::<f64>().max(1e-9);
            for (p, th) in &h.layers {
                // a mesh without layers: the volume is split by thickness, the area is approximately v/t
                let da = if h.kind == "solid" { 0.0 } else { dv / t };
                parts.push((*p, dv * th / t, da));
            }
        }
    }
    if parts.is_empty() {
        return Ok(());
    }
    let total: f64 = parts.iter().map(|p| p.1).sum();
    if h.kind == "solid" {
        db.execute(
            "UPDATE solid_qty SET net_volume = MAX(net_volume - ?2, 0) WHERE solid=?1",
            params![h.id, total],
        )?;
    } else {
        for (pos, dv, da) in &parts {
            apply_cut(db, h.kind, h.id, *pos, *dv, *da)?;
        }
    }
    // each void's contribution: its own cut (when voids overlap, the sum exceeds the union)
    for (k, v) in vs.iter().enumerate() {
        let own = if vs.len() == 1 {
            total
        } else {
            let one = [vb[k]];
            host_bodies
                .iter()
                .map(|(_, body, _, _)| {
                    let hb = [*body];
                    memo("vol", &hb, &minus, &one, || {
                        gsolid::boolean_volume(&hb, &minus, &one)
                    })
                })
                .sum()
        };
        if own > 1e-9 {
            db.execute(
                "INSERT OR REPLACE INTO solid_cut VALUES (?1,?2,?3,?4)",
                params![v.id, h.kind, h.id, own],
            )?;
        }
    }
    Ok(())
}

// ---------- dangling host references ----------

/// Deletes `solid_host` rows whose host disappeared (a deleted wall/slab/solid), so that a
/// new element with the same rowid does not inherit the void; the inverse command restores them.
pub(crate) fn prune_hosts(db: &Connection) -> Result<Inv, Error> {
    let n: i64 = db.query_row(
        "SELECT count(*) FROM sqlite_master WHERE type='table' AND name='solid_host'",
        [],
        |r| r.get(0),
    )?;
    if n == 0 {
        return Ok(Inv::default());
    }
    let mut inv = Inv::default();
    for (kind, table) in [
        ("wall", "wall"),
        ("slab", "slab"),
        ("roof", "roof"),
        ("solid", "solid"),
    ] {
        let rows: Vec<(i64, i64)> = {
            let mut st = db.prepare_cached(&format!(
                "SELECT solid, ref FROM solid_host WHERE kind=?1 \
                 AND ref NOT IN (SELECT id FROM {table})"
            ))?;
            let v = st
                .query_map([kind], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        for (s, r) in rows {
            inv = inv.sql(
                "INSERT OR IGNORE INTO solid_host VALUES (?1,?2,?3)",
                vec![
                    Value::Integer(s),
                    Value::Text(kind.into()),
                    Value::Integer(r),
                ],
            );
            db.execute(
                "DELETE FROM solid_host WHERE solid=?1 AND kind=?2 AND ref=?3",
                params![s, kind, r],
            )?;
        }
    }
    Ok(inv)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn bx(x0: f64, y0: f64, x1: f64, y1: f64) -> (V3, V3) {
        (v3(x0, y0, 0.0), v3(x1, y1, 1.0))
    }

    #[test]
    fn grid_finds_only_overlapping_boxes() {
        let g = Grid::new(vec![
            bx(0.0, 0.0, 1.0, 1.0),
            bx(10.0, 10.0, 11.0, 11.0),
            bx(-1e6, -1e6, 1e6, 1e6),
        ]);
        assert_eq!(g.query(bx(0.5, 0.5, 2.0, 2.0)), vec![0, 2]);
        assert_eq!(g.query(bx(10.5, 10.5, 10.6, 10.6)), vec![1, 2]);
        assert!(!Grid::new(vec![bx(0.0, 0.0, 1.0, 1.0)]).any(bx(5.0, 5.0, 6.0, 6.0)));
    }

    #[test]
    fn memo_returns_cached_value_for_same_input() {
        let r = vec![vec![v2(0.0, 0.0), v2(1.0, 0.0), v2(1.0, 1.0)]];
        let b = [Body::Prism {
            rings: &r,
            z0: 0.0,
            z1: 1.0,
        }];
        let a = memo("t-memo", &b, &[], &b, || 7.0);
        let c = memo("t-memo", &b, &[], &b, || 9.0);
        assert_eq!((a, c), (7.0, 7.0));
    }
}

// ---------- 3D: through voids in a slab body ----------

fn in_ring(p: V2, r: &[V2]) -> bool {
    let mut c = false;
    let n = r.len();
    for i in 0..n {
        let (a, b) = (r[i], r[(i + 1) % n]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (p.y - a.y) * (b.x - a.x) / (b.y - a.y) {
            c = !c;
        }
    }
    c
}

fn rings_cross(a: &[V2], b: &[V2]) -> bool {
    let o = |p: V2, q: V2, r: V2| (q - p).cross(r - p);
    let (n, m) = (a.len(), b.len());
    (0..n).any(|i| {
        let (p, q) = (a[i], a[(i + 1) % n]);
        (0..m).any(|j| {
            let (r, s) = (b[j], b[(j + 1) % m]);
            o(r, s, p) * o(r, s, q) <= 0.0 && o(p, q, r) * o(p, q, s) <= 0.0
        })
    })
}

/// Ring `a` is strictly inside `outer`.
fn ring_within(a: &[V2], outer: &[V2]) -> bool {
    a.iter().all(|p| in_ring(*p, outer)) && !rings_cross(a, outer)
}

fn rings_apart(a: &[V2], b: &[V2]) -> bool {
    !rings_cross(a, b) && !in_ring(a[0], b) && !in_ring(b[0], a)
}

/// Holes from voids cutting a slab: a through prism whose section lies entirely inside the
/// outline and touches no other hole is a hole; anything else is a warning.
pub(crate) fn through_holes(
    slab: (V3, V3),
    outer: &[V2],
    holes: &[Vec<V2>],
    voids: &[(i64, bool, Option<Arc<Mesh>>)],
) -> (Vec<Vec<V2>>, Vec<i64>) {
    let mut out: Vec<Vec<V2>> = vec![];
    let mut skipped = vec![];
    let zm = (slab.0.z + slab.1.z) * 0.5;
    for (id, prism, mesh) in voids {
        let Some(m) = mesh.as_ref().filter(|_| *prism) else {
            skipped.push(*id);
            continue;
        };
        let (z0, z1) = m
            .positions
            .iter()
            .fold((f64::INFINITY, f64::NEG_INFINITY), |a, p| {
                (a.0.min(p.z), a.1.max(p.z))
            });
        if z0 > slab.0.z + 1e-6 || z1 < slab.1.z - 1e-6 {
            skipped.push(*id);
            continue;
        }
        let secs = gsolid::plan_cut(m, zm);
        let ok = !secs.is_empty()
            && secs.iter().all(|(o, hs)| {
                hs.is_empty()
                    && o.len() >= 3
                    && ring_within(o, outer)
                    && holes.iter().chain(&out).all(|h| rings_apart(o, h))
            })
            && secs
                .iter()
                .enumerate()
                .all(|(i, a)| secs[i + 1..].iter().all(|b| rings_apart(&a.0, &b.0)));
        if ok {
            out.extend(secs.into_iter().map(|s| s.0));
        } else {
            skipped.push(*id);
        }
    }
    (out, skipped)
}

impl crate::Model {
    /// A slab body for 3D with through holes from the prism voids cutting it;
    /// the second is the ids of voids not cut in 3D (partial, not prisms, on the outline).
    pub fn slab_mesh_cut(&self, id: i64) -> Result<(Mesh, Vec<i64>), Error> {
        let plain = self.slab_mesh(id)?;
        let ids: Vec<i64> = {
            let mut st = self.db.prepare_cached(
                "SELECT solid FROM solid_cut WHERE kind='slab' AND ref=?1 ORDER BY solid",
            )?;
            let rows = st.query_map([id], |r| r.get(0))?;
            rows.collect::<Result<_, _>>()?
        };
        if ids.is_empty() || plain.positions.is_empty() {
            return Ok((plain, vec![]));
        }
        let bb = plain.positions.iter().fold(
            (
                v3(f64::INFINITY, f64::INFINITY, f64::INFINITY),
                v3(f64::NEG_INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY),
            ),
            |(lo, hi), p| {
                (
                    v3(lo.x.min(p.x), lo.y.min(p.y), lo.z.min(p.z)),
                    v3(hi.x.max(p.x), hi.y.max(p.y), hi.z.max(p.z)),
                )
            },
        );
        // a sloped slab: the void must pass through the body's entire height extent
        let rs = crate::slabs::rings(&self.db, id)?;
        let Some((outer, holes)) = rs.split_first() else {
            return Ok((plain, ids));
        };
        let voids: Vec<(i64, bool, Option<Arc<Mesh>>)> = ids
            .iter()
            .map(|&v| match read(&self.db, v) {
                Ok(s) => {
                    let prism =
                        matches!(s.form, Form::Extrusion { .. }) && s.frame.z.z.abs() > 1.0 - 1e-9;
                    (v, prism, mesh_of(&self.db, &s).ok())
                }
                Err(_) => (v, false, None),
            })
            .collect();
        let hs: Vec<Vec<V2>> = holes.iter().map(|h| pts(h)).collect();
        let (extra, mut skipped) = through_holes(bb, &pts(outer), &hs, &voids);
        if extra.is_empty() {
            return Ok((plain, skipped));
        }
        let extra: Vec<Vec<(f64, f64)>> = extra
            .iter()
            .map(|r| r.iter().map(|p| (p.x, p.y)).collect())
            .collect();
        match self.slab_mesh_holes(id, &extra) {
            Ok(m) => Ok((m, skipped)),
            Err(_) => {
                skipped = ids;
                Ok((plain, skipped))
            }
        }
    }
}

#[cfg(test)]
mod slab3d_tests {
    use super::*;

    fn sq(x0: f64, y0: f64, x1: f64, y1: f64) -> Vec<V2> {
        vec![v2(x0, y0), v2(x1, y0), v2(x1, y1), v2(x0, y1)]
    }

    #[test]
    fn rings_inside_and_apart() {
        let o = sq(0.0, 0.0, 10.0, 10.0);
        assert!(ring_within(&sq(1.0, 1.0, 2.0, 2.0), &o));
        assert!(!ring_within(&sq(9.0, 1.0, 11.0, 2.0), &o));
        assert!(rings_apart(
            &sq(1.0, 1.0, 2.0, 2.0),
            &sq(3.0, 3.0, 4.0, 4.0)
        ));
        assert!(!rings_apart(
            &sq(1.0, 1.0, 3.0, 3.0),
            &sq(2.0, 2.0, 4.0, 4.0)
        ));
        assert!(!rings_apart(
            &sq(1.0, 1.0, 5.0, 5.0),
            &sq(2.0, 2.0, 3.0, 3.0)
        ));
    }
}
