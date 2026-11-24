// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Views: a level plan at a cut height. The result is primitives in model coordinates (m),
//! with no paper quantities: line weights, text heights and dimension offsets are set by the sheet (ddc-io).

use ddc_geom::arc::{ArcAxis, CHORD_SAG};
use ddc_geom::layered::{Layer, Strip};
use ddc_geom::{v2, Tol};

use crate::{Error, Model};

type P = (f64, f64);
/// id, type, [ax, ay, bx, by, base_offset, height], offset0, bulge.
type RawWall = (i64, i64, [f64; 6], Option<f64>, f64);

/// A grid axis: label and segment in plan.
#[derive(Clone, Debug, PartialEq)]
pub struct GridAxis {
    pub label: String,
    pub a: P,
    pub b: P,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanViewSpec {
    pub level: i64,
    /// Cut height above the level elevation, m.
    pub cut_height: f64,
    pub grids: Vec<GridAxis>,
}

/// The side of the outer outline along which a dimension chain runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub enum Side {
    South,
    North,
    West,
    East,
}

#[derive(Clone, Debug, PartialEq)]
pub enum ViewPrim {
    /// A view annotation (text, detail line and region, joint element, mark, cloud).
    Anno { item: crate::Anno },
    /// A wall layer section (fill/hatch by material, outline is the cut line).
    Cut {
        wall: i64,
        layer: usize,
        material: String,
        ring: Vec<P>,
    },
    /// A door leaf in the open position.
    DoorLeaf { opening: i64, a: P, b: P },
    /// Swing arc: the centre is the hinge; angles in degrees, counter-clockwise from `a0` to `a1`.
    DoorSwing {
        opening: i64,
        center: P,
        radius: f64,
        a0: f64,
        a1: f64,
    },
    /// A window symbol line (wall faces and glass).
    WindowLine { opening: i64, a: P, b: P },
    /// Curtain wall: two thin lines along the faces and mullion ticks; `curtain` is the curtain wall id.
    CurtainLine { curtain: i64, a: P, b: P },
    RoomTag {
        room: i64,
        pos: P,
        name: String,
        number: String,
        /// m2.
        area: f64,
        /// Floor finish id or text of the room; None on sections and where none is known.
        finish: Option<String>,
        /// On a section, the room's cut: its width along the section and its height, m; the
        /// tag stands inside it or is left off. `None` on plans.
        cut: Option<(f64, f64)>,
    },
    Grid {
        label: String,
        a: P,
        b: P,
        /// Tick marks (at the start `a`, at the end `b`).
        bubbles: (bool, bool),
    },
    /// A dimension between points on an outline side; `chain` 0 is openings, 1 is overall.
    Dim {
        side: Side,
        chain: u8,
        a: P,
        b: P,
        value: f64,
    },
    /// A model dimension object (`Model::dims`); when such dimensions exist, implicit `Dim` chains are not built.
    Annot {
        dim: i64,
        tier: i32,
        orphan: bool,
        geom: crate::DimGeom,
    },
    /// Saved measurement (`Model::measures_in_view`): a line `a`-`b` (equal points when it has
    /// no span) and its number; `value` None says why in `reason` (never a zero).
    Measure {
        measure: i64,
        a: P,
        b: P,
        value: Option<f64>,
        unit: &'static str,
        reason: String,
    },
    /// An element tag (`Model::tags`): the head at `tag.head`, the leader to `tag.anchor`.
    Tag { tag: crate::Tag },
    /// An element section in a cut, coordinates (s, z): outer outlines counter-clockwise, holes
    /// clockwise; `element` is the id of a wall, slab, opening (frame, leaf, glass) or curtain wall.
    SectionCut {
        /// Element table: wall, opening, slab, curtain_wall (ids are unique only within a table).
        table: &'static str,
        element: i64,
        material: String,
        rings: Vec<P>,
        holes: Vec<Vec<P>>,
    },
    /// A visible projection edge in a section/elevation.
    Edge {
        table: &'static str,
        element: i64,
        class: LineClass,
        a: P,
        b: P,
    },
    /// A level line with elevation `elevation` (m), the label at end `b`.
    Level {
        name: String,
        elevation: f64,
        a: P,
        b: P,
    },
}

/// Projection line class: the element is cut by the section plane or lies behind it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum LineClass {
    Projection,
    Beyond,
    /// Above the cut plane, inside the view range: drawn dashed.
    Overhead,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PlanView {
    pub prims: Vec<ViewPrim>,
    /// [xmin, ymin, xmax, ymax] of wall sections (without dimensions), m.
    pub bbox: [f64; 4],
}

impl PlanView {
    pub fn count(&self, f: impl Fn(&ViewPrim) -> bool) -> usize {
        self.prims.iter().filter(|p| f(p)).count()
    }
}

pub(crate) struct WallRow {
    pub(crate) id: i64,
    pub(crate) a: P,
    pub(crate) b: P,
    pub(crate) base_offset: f64,
    pub(crate) height: f64,
    pub(crate) offset0: f64,
    pub(crate) total: f64,
    pub(crate) mats: Vec<String>,
    pub(crate) thick: Vec<f64>,
    pub(crate) arc: Option<ArcAxis>,
    /// Top polyline (u, h above the bottom), None means flat.
    pub(crate) top: Option<Vec<(f64, f64)>>,
    /// Bottom polyline (u, dz above the bottom), None means flat.
    pub(crate) base: Option<Vec<(f64, f64)>>,
}

impl WallRow {
    /// The point at axis position `t` from `a`, offset `o` to the left of the axis (on an arc, along the face circle).
    pub(crate) fn at(&self, t: f64, o: f64) -> P {
        match &self.arc {
            Some(ax) => {
                let a = ax.angle_at(t);
                let r = ax.radius_at(o);
                (ax.center.x + r * a.cos(), ax.center.y + r * a.sin())
            }
            None => {
                let (d, n) = self.frame();
                (self.a.0 + d.0 * t + n.0 * o, self.a.1 + d.1 * t + n.1 * o)
            }
        }
    }
    /// The inverse of `at`: (along the axis, offset).
    fn local(&self, p: P) -> P {
        match &self.arc {
            Some(ax) => {
                let r = ((p.0 - ax.center.x).powi(2) + (p.1 - ax.center.y).powi(2)).sqrt();
                (ax.param_of(v2(p.0, p.1)), ax.sigma() * (ax.radius - r))
            }
            None => {
                let (d, n) = self.frame();
                let q = (p.0 - self.a.0, p.1 - self.a.1);
                (q.0 * d.0 + q.1 * d.1, q.0 * n.0 + q.1 * n.1)
            }
        }
    }
    /// The left axis normal at `t`.
    fn normal(&self, t: f64) -> P {
        match &self.arc {
            Some(ax) => {
                let u = ax.tangent_at(t);
                (-u.y, u.x)
            }
            None => self.frame().1,
        }
    }
    pub(crate) fn frame(&self) -> (P, P) {
        let (dx, dy) = (self.b.0 - self.a.0, self.b.1 - self.a.1);
        let len = (dx * dx + dy * dy).sqrt();
        let d = (dx / len, dy / len);
        (d, (-d.1, d.0))
    }
}

struct OpRow {
    id: i64,
    along: f64,
    width: f64,
    sill: f64,
    height: f64,
    kind: String,
    side: bool,
    hinge: bool,
}

/// Axis spans (u0, u1) where the top profile (u, h) is above `rel`.
fn high_spans(top: &[(f64, f64)], rel: f64) -> Vec<(f64, f64)> {
    let mut us: Vec<f64> = top.iter().map(|p| p.0).collect();
    for w in top.windows(2) {
        let (p, q) = (w[0], w[1]);
        if (p.1 - rel) * (q.1 - rel) < 0.0 && q.0 > p.0 {
            us.push(p.0 + (q.0 - p.0) * (rel - p.1) / (q.1 - p.1));
        }
    }
    us.sort_by(f64::total_cmp);
    us.dedup_by(|a, b| (*a - *b).abs() < 1e-12);
    let h = |u: f64| {
        top.windows(2)
            .find(|w| u >= w[0].0 && u <= w[1].0 && w[1].0 > w[0].0)
            .map_or(0.0, |w| w[0].1 + (w[1].1 - w[0].1) * (u - w[0].0) / (w[1].0 - w[0].0))
    };
    let mut out: Vec<(f64, f64)> = vec![];
    for w in us.windows(2) {
        if w[1] - w[0] > 1e-12 && h((w[0] + w[1]) / 2.0) > rel {
            match out.last_mut() {
                Some(l) if (l.1 - w[0]).abs() < 1e-9 => l.1 = w[1],
                _ => out.push((w[0], w[1])),
            }
        }
    }
    out
}

/// Clipping a polygon by the half-plane `dot(p - o, d) >= 0`.
pub(crate) fn clip(ring: &[P], o: P, d: P) -> Vec<P> {
    let f = |p: P| (p.0 - o.0) * d.0 + (p.1 - o.1) * d.1;
    let mut out = vec![];
    for i in 0..ring.len() {
        let (p, q) = (ring[i], ring[(i + 1) % ring.len()]);
        let (fp, fq) = (f(p), f(q));
        if fp >= 0.0 {
            out.push(p);
        }
        if (fp >= 0.0) != (fq >= 0.0) {
            let t = fp / (fp - fq);
            out.push((p.0 + t * (q.0 - p.0), p.1 + t * (q.1 - p.1)));
        }
    }
    out
}

fn area(r: &[P]) -> f64 {
    let mut s = 0.0;
    for i in 0..r.len() {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        s += p.0 * q.1 - q.0 * p.1;
    }
    s / 2.0
}

fn ang(v: P) -> f64 {
    let a = v.1.atan2(v.0).to_degrees();
    let a = if a < 0.0 { a + 360.0 } else { a };
    // Removes -0/360 and rounding noise so the output is byte-for-byte stable.
    (a * 1e9).round() / 1e9 % 360.0
}

/// Level walls by id and their joined layer outlines (joints use all walls of the level).
pub(crate) fn load_walls(
    db: &rusqlite::Connection,
    level: i64,
    elev: f64,
    tol: Tol,
) -> Result<(Vec<WallRow>, ddc_geom::arc::Walls), Error> {
    load_walls_at(db, level, elev, tol, None)
}

/// `load_walls` with leaning walls taken at `cut` above the level (a plan's cut height): axis
/// and layers of their horizontal section there, joins solved at that height.
pub(crate) fn load_walls_at(
    db: &rusqlite::Connection,
    level: i64,
    elev: f64,
    tol: Tol,
    cut: Option<f64>,
) -> Result<(Vec<WallRow>, ddc_geom::arc::Walls), Error> {
    let mut walls: Vec<WallRow> = vec![];
    {
        let mut st = db.prepare(
                "SELECT id,wtype,ax,ay,bx,by,base_offset,height,offset0,bulge,top_profile FROM wall WHERE level=?1 ORDER BY id",
            )?;
        let mut ls = db.prepare(
            "SELECT material, thickness FROM wall_type_layer WHERE wtype=?1 ORDER BY pos",
        )?;
        let rows: Vec<(RawWall, Option<String>)> = st
            .query_map([level], |r| {
                let mut f = [0.0; 6];
                for (k, s) in f.iter_mut().enumerate() {
                    *s = r.get(k + 2)?;
                }
                Ok((
                    (r.get(0)?, r.get(1)?, f, r.get(8)?, r.get(9)?),
                    r.get::<_, Option<String>>(10)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        let mut shapes = crate::cuts::Shapes::default();
        for ((id, wt, f, off, bulge), top) in rows {
            let layers: Vec<(String, f64)> = ls
                .query_map([wt], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let total: f64 = layers.iter().map(|l| l.1).sum();
            let arc = if bulge != 0.0 {
                Some(
                    ArcAxis::new(v2(f[0], f[1]), v2(f[2], f[3]), bulge, tol)
                        .map_err(|e| Error::Geom(format!("wall {id}: arc {e:?}")))?,
                )
            } else {
                None
            };
            walls.push(WallRow {
                id,
                a: (f[0], f[1]),
                b: (f[2], f[3]),
                base_offset: f[4],
                height: f[5],
                offset0: off.unwrap_or(-total / 2.0),
                total,
                mats: layers.iter().map(|l| l.0.clone()).collect(),
                thick: layers.iter().map(|l| l.1).collect(),
                arc,
                top: top.as_deref().and_then(crate::walls::parse_top_profile),
                base: if bulge == 0.0 {
                    crate::cuts::base_profile(db, &mut shapes, id)?
                } else {
                    None
                },
            });
        }
    }
    // Joints are computed over all walls of the level: junction drawing does not depend on the cut height.
    let mut strips: Vec<Strip> = walls
        .iter()
        .map(|w| Strip {
            a: v2(w.a.0, w.a.1),
            b: v2(w.b.0, w.b.1),
            layers: w.thick.iter().map(|&t| Layer::new(t)).collect(),
            offset0: w.offset0,
            base: elev + w.base_offset,
            height: w.height,
        })
        .collect();
    if let Some(c) = cut {
        for (w, s) in walls.iter_mut().zip(strips.iter_mut()) {
            let l = crate::walls::effective_lean(db, w.id, w.top.is_some())?;
            if l.is_plain() {
                continue;
            }
            let z = (c - w.base_offset).clamp(0.0, w.height);
            let bulge = w.arc.map_or(0.0, |a| a.bulge);
            let at = ddc_geom::wall_lean::strip_at(s, bulge, &l, z);
            if bulge != 0.0 {
                // the concentric arc of the section: same sweep, ends moved radially
                w.arc = Some(
                    ArcAxis::new(at.a, at.b, bulge, tol)
                        .map_err(|e| Error::Geom(format!("wall {}: arc {e:?}", w.id)))?,
                );
            }
            w.a = (at.a.x, at.a.y);
            w.b = (at.b.x, at.b.y);
            w.offset0 = at.offset0;
            w.thick = at.layers.iter().map(|x| x.thickness).collect();
            w.total = w.thick.iter().sum();
            *s = Strip {
                base: elev + w.base_offset,
                ..at
            };
        }
    }
    let geom = |e| Error::Geom(format!("{e:?}"));
    let bulges: Vec<f64> = walls
        .iter()
        .map(|w| w.arc.map_or(0.0, |a| a.bulge))
        .collect();
    let ids: Vec<i64> = walls.iter().map(|w| w.id).collect();
    let prefs = crate::wall_edit::join_prefs(db, &ids)?;
    let joined = crate::walls::join_tolerant(&strips, &bulges, &prefs, tol)
        .map_err(geom)?
        .0;
    Ok((walls, joined))
}

impl Model {
    /// A level plan: sections of walls cut by the section plane, with gaps at openings,
    /// door/window symbols, room tags, axes, dimension chains along the outer walls.
    pub fn plan_view(&self, spec: &PlanViewSpec) -> Result<PlanView, Error> {
        let tol = Tol::default();
        let db = &self.db;
        let elev: f64 = db
            .query_row(
                "SELECT elevation FROM level WHERE id=?1",
                [spec.level],
                |r| r.get(0),
            )
            .map_err(|_| Error::Invalid(format!("level {} not found", spec.level)))?;
        if !spec.cut_height.is_finite() {
            return Err(Error::Invalid("cut height must be finite".into()));
        }
        let (walls, joined) = load_walls_at(db, spec.level, elev, tol, Some(spec.cut_height))?;
        let mut prims = vec![];
        let mut ops_all: Vec<(usize, OpRow)> = vec![];
        let mut op_st = db.prepare(
            "SELECT id,along,width,sill,height,kind,swing_side,swing_hinge FROM opening WHERE wall=?1 ORDER BY along, id",
        )?;
        for (i, w) in walls.iter().enumerate() {
            let rel = spec.cut_height - w.base_offset;
            // under a roof the top follows the profile: cut where the wall is above the plane, "behind" it below
            let profiled = w.top.as_ref();
            let hmax = profiled.map_or(w.height, |t| t.iter().fold(0.0f64, |m, p| m.max(p.1)));
            if !(rel >= 0.0 && rel < hmax) {
                // a low wall (parapet, upstand) is seen from above: its footprint as projection
                if rel >= hmax {
                    for pr in joined.rings[i].iter().flatten() {
                        let r = pr.tessellate(CHORD_SAG);
                        for k in 0..r.len() {
                            let (a, b) = (r[k], r[(k + 1) % r.len()]);
                            prims.push(ViewPrim::Edge {
                                table: "wall",
                                element: w.id,
                                class: LineClass::Projection,
                                a: (a.x, a.y),
                                b: (b.x, b.y),
                            });
                        }
                    }
                }
                continue;
            }
            let ops: Vec<OpRow> = op_st
                .query_map([w.id], |r| {
                    Ok(OpRow {
                        id: r.get(0)?,
                        along: r.get(1)?,
                        width: r.get(2)?,
                        sill: r.get(3)?,
                        height: r.get(4)?,
                        kind: r.get(5)?,
                        side: r.get::<_, i64>(6)? != 0,
                        hinge: r.get::<_, i64>(7)? != 0,
                    })
                })?
                .collect::<Result<_, _>>()?;
            let cut: Vec<OpRow> = ops
                .into_iter()
                .filter(|o| rel >= o.sill && rel < o.sill + o.height)
                .collect();
            for (l, profs) in joined.rings[i].iter().enumerate() {
                for pr in profs {
                    let ring: Vec<P> = pr
                        .tessellate(CHORD_SAG)
                        .iter()
                        .map(|p| (p.x, p.y))
                        .collect();
                    // Solid axis intervals between openings: (-inf, s1], [e1, s2], ..., [en, +inf).
                    let mut lo: Option<f64> = None;
                    let mut cuts: Vec<(Option<f64>, Option<f64>)> = vec![];
                    for o in &cut {
                        cuts.push((lo, Some(o.along)));
                        lo = Some(o.along + o.width);
                    }
                    cuts.push((lo, None));
                    // axis spans where the top profile is below the plane are not cut (top view)
                    let mut lows: Vec<(Option<f64>, Option<f64>)> = vec![];
                    if let Some(t) = profiled {
                        let len = t.last().map_or(0.0, |p| p.0);
                        let hi = high_spans(t, rel);
                        if !(hi.len() == 1 && hi[0].0 <= 1e-9 && hi[0].1 >= len - 1e-9) {
                            let b = |u: f64| if u <= 1e-9 || u >= len - 1e-9 { None } else { Some(u) };
                            let mut next = vec![];
                            for &(s0, e0) in &cuts {
                                for &(h0, h1) in &hi {
                                    let lo2 = s0.unwrap_or(f64::NEG_INFINITY).max(if h0 <= 1e-9 { f64::NEG_INFINITY } else { h0 });
                                    let hi2 = e0.unwrap_or(f64::INFINITY).min(if h1 >= len - 1e-9 { f64::INFINITY } else { h1 });
                                    if hi2 - lo2 > 1e-9 {
                                        next.push(((lo2.is_finite()).then_some(lo2), (hi2.is_finite()).then_some(hi2)));
                                    }
                                }
                            }
                            cuts = next;
                            let mut at = 0.0;
                            for &(h0, h1) in &hi {
                                if h0 - at > 1e-9 {
                                    lows.push((b(at), Some(h0)));
                                }
                                at = h1;
                            }
                            if len - at > 1e-9 {
                                lows.push((b(at), None));
                            }
                            if hi.is_empty() {
                                lows = vec![(None, None)];
                            }
                        }
                    }
                    // Clipping in wall coordinates (along the axis, offset): for an arc the opening bounds are
                    // radial lines, for a straight wall they are normals to the axis.
                    let multi = cuts.len() > 1 || !lows.is_empty();
                    let loc: Vec<P> = if multi {
                        ring.iter().map(|&p| w.local(p)).collect()
                    } else {
                        vec![]
                    };
                    for &(s, e) in &lows {
                        let mut r = loc.clone();
                        if let Some(s) = s {
                            r = clip(&r, (s, 0.0), (1.0, 0.0));
                        }
                        if let Some(e) = e {
                            r = clip(&r, (e, 0.0), (-1.0, 0.0));
                        }
                        // the edge on the cut/beyond boundary is the cut's own outline: one line, not two
                        let on = |q: &P| s.is_some_and(|s| (q.0 - s).abs() < 1e-9) || e.is_some_and(|e| (q.0 - e).abs() < 1e-9);
                        for k in 0..r.len() {
                            let (p, q) = (r[k], r[(k + 1) % r.len()]);
                            if on(&p) && on(&q) && (p.0 - q.0).abs() < 1e-9 {
                                continue;
                            }
                            prims.push(ViewPrim::Edge {
                                table: "wall",
                                element: w.id,
                                class: LineClass::Projection,
                                a: w.at(p.0, p.1),
                                b: w.at(q.0, q.1),
                            });
                        }
                    }
                    for (s, e) in cuts {
                        let r = if multi {
                            let mut r = loc.clone();
                            if let Some(s) = s {
                                r = clip(&r, (s, 0.0), (1.0, 0.0));
                            }
                            if let Some(e) = e {
                                r = clip(&r, (e, 0.0), (-1.0, 0.0));
                            }
                            r.into_iter().map(|q| w.at(q.0, q.1)).collect()
                        } else {
                            ring.clone()
                        };
                        if r.len() >= 3 && area(&r).abs() > 1e-9 {
                            prims.push(ViewPrim::Cut {
                                wall: w.id,
                                layer: l,
                                material: w.mats[l].clone(),
                                ring: r,
                            });
                        }
                    }
                }
            }
            let off = |t: f64, o: f64| w.at(t, o);
            let (right, left) = (w.offset0, w.offset0 + w.total);
            for o in &cut {
                match o.kind.as_str() {
                    "door" => {
                        let (th, tf) = if o.hinge {
                            (o.along + o.width, o.along)
                        } else {
                            (o.along, o.along + o.width)
                        };
                        let face = if o.side { right } else { left };
                        let n = w.normal(th);
                        let m = if o.side { (-n.0, -n.1) } else { n };
                        let hp = off(th, face);
                        let closed = off(tf, face);
                        let open = (hp.0 + m.0 * o.width, hp.1 + m.1 * o.width);
                        let u = (closed.0 - hp.0, closed.1 - hp.1);
                        let cross = u.0 * m.1 - u.1 * m.0;
                        let (a0, a1) = if cross > 0.0 {
                            (ang(u), ang(m))
                        } else {
                            (ang(m), ang(u))
                        };
                        prims.push(ViewPrim::DoorLeaf {
                            opening: o.id,
                            a: hp,
                            b: open,
                        });
                        prims.push(ViewPrim::DoorSwing {
                            opening: o.id,
                            center: hp,
                            radius: o.width,
                            a0,
                            a1,
                        });
                    }
                    "window" => {
                        for f in [right, (right + left) / 2.0, left] {
                            prims.push(ViewPrim::WindowLine {
                                opening: o.id,
                                a: off(o.along, f),
                                b: off(o.along + o.width, f),
                            });
                        }
                    }
                    _ => {}
                }
            }
            ops_all.extend(cut.into_iter().map(|o| (i, o)));
        }
        let mut bbox = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for p in &prims {
            if let ViewPrim::Cut { ring, .. } = p {
                for q in ring {
                    bbox = [
                        bbox[0].min(q.0),
                        bbox[1].min(q.1),
                        bbox[2].max(q.0),
                        bbox[3].max(q.1),
                    ];
                }
            }
        }
        let annots = self.dims(spec.level)?;
        let implicit = bbox[0].is_finite() && annots.is_empty();
        if !bbox[0].is_finite() {
            bbox = [0.0; 4];
        } else if implicit {
            prims.extend(dims(&walls, &ops_all, bbox));
            let flat: Vec<(usize, f64, f64)> = ops_all.iter().map(|(i, o)| (*i, o.along, o.width)).collect();
            prims.extend(crate::views_inner::opening_chains(&walls, &flat, bbox));
            prims.extend(crate::views_inner::oblique_chains(&walls, &flat));
            prims.extend(self.inner_room_dims(spec.level)?);
        }
        prims.extend(annots.into_iter().map(|a| ViewPrim::Annot {
            dim: a.id,
            tier: a.spec.tier,
            orphan: a.orphan,
            geom: a.geom,
        }));
        for i in self.measures_in_view(crate::phases::ViewKey::Plan(spec.level))? {
            let (v, _, reason) = i.value.parts();
            let at = |p: &[f64; 3]| (p[0], p[1]);
            let (Some(f), Some(l)) = (i.points.first(), i.points.last()) else {
                continue;
            };
            prims.push(ViewPrim::Measure {
                measure: i.id,
                a: at(f),
                b: at(l),
                value: v.or(i.last),
                unit: i.spec.kind.unit(),
                reason,
            });
        }
        let finishes = self.plan_room_finishes(spec.level);
        let mut tags = self.tags(spec.level)?;
        // a tagged room names its finish on its tag, as an untagged one on its RoomTag
        for t in &mut tags {
            if let crate::TagHost::Room(id) = t.spec.host {
                t.text.finish = finishes.get(&id).cloned();
            }
        }
        // a room with a tag element is labelled by the tag (as in the editor)
        let tagged: Vec<i64> = tags
            .iter()
            .filter_map(|t| match t.spec.host {
                crate::TagHost::Room(id) => Some(id),
                _ => None,
            })
            .collect();
        {
            let mut st = db.prepare(
                "SELECT r.id, r.name, r.number, r.px, r.py, q.area FROM room r JOIN room_qty q ON q.room=r.id WHERE r.level=?1 ORDER BY r.id",
            )?;
            let rows = st.query_map([spec.level], |r| {
                Ok(ViewPrim::RoomTag {
                    room: r.get(0)?,
                    name: r.get(1)?,
                    number: r.get(2)?,
                    pos: (r.get(3)?, r.get(4)?),
                    area: r.get(5)?,
                    finish: None,
                    cut: None,
                })
            })?;
            for r in rows {
                let mut r = r?;
                if let ViewPrim::RoomTag { room, finish, .. } = &mut r {
                    if tagged.contains(room) {
                        continue;
                    }
                    *finish = finishes.get(room).cloned();
                }
                prims.push(r);
            }
        }
        prims.extend(tags.into_iter().map(|tag| ViewPrim::Tag { tag }));
        prims.extend(self.curtain_plan(spec.level)?);
        prims.extend(self.stairs_plan(spec.level, spec.cut_height)?);
        prims.extend(self.ramps_plan(spec.level, spec.cut_height)?);
        prims.extend(self.components_plan(spec.level, crate::view_gfx::plan_lod(db, spec.level)?)?);
        prims.extend(crate::solids::plan_prims(db, spec.level, spec.cut_height)?);
        prims.extend(self.mesh_plan_prims(spec.level, spec.cut_height)?);
        prims.extend(crate::annot_text::view_prims(
            self,
            crate::AnnoView::Plan(spec.level),
        )?);
        // model axes (by level range, with 2D bounds and view tags) replace the passed ones
        let own = if crate::datum::has_grids(db)? {
            self.plan_grid_axes(spec.level)?
        } else {
            spec.grids
                .iter()
                .map(|g| (g.clone(), (true, true)))
                .collect()
        };
        for (g, bubbles) in own {
            prims.push(ViewPrim::Grid {
                label: g.label,
                a: g.a,
                b: g.b,
                bubbles,
            });
        }
        if implicit {
            crate::views_axes::grid_chains(&mut prims, bbox);
        }
        self.hide_in_drawing(crate::phases::ViewKey::Plan(spec.level), &mut prims)?;
        Ok(PlanView { prims, bbox })
    }
}

impl Model {
    /// Level stairs in plan: outlines and risers below the cut are projected, above it are "behind"
    /// (dashed), plus the break line and the up arrow with its head.
    pub fn stairs_plan(&self, level: i64, cut_height: f64) -> Result<Vec<ViewPrim>, Error> {
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM stair WHERE level=?1 ORDER BY id")?;
            let v = st
                .query_map([level], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for id in ids {
            let Ok(p) = self.stair_plan(id, level, cut_height) else {
                continue;
            };
            let mut e = |a: ddc_geom::V2, b: ddc_geom::V2, class| {
                out.push(ViewPrim::Edge {
                    table: "stair",
                    element: id,
                    class,
                    a: (a.x, a.y),
                    b: (b.x, b.y),
                })
            };
            for r in &p.outline {
                for k in 0..r.len() {
                    e(r[k], r[(k + 1) % r.len()], LineClass::Projection);
                }
            }
            for (a, b) in &p.below {
                e(*a, *b, LineClass::Projection);
            }
            for (a, b) in &p.above {
                e(*a, *b, LineClass::Beyond);
            }
            let poly = |v: &[ddc_geom::V2]| v.windows(2).map(|w| (w[0], w[1])).collect::<Vec<_>>();
            let mut lines = vec![];
            if let Some(c) = p.cut {
                lines.extend(poly(&c));
            }
            lines.extend(poly(&p.arrow));
            if let Some(h) = p.head {
                lines.extend(poly(&h));
            }
            for (a, b) in lines {
                e(a, b, LineClass::Projection);
            }
        }
        Ok(out)
    }

    /// Level curtain walls: faces (an arc as chords per panel) and ticks of vertical mullions.
    pub fn curtain_plan(&self, level: i64) -> Result<Vec<ViewPrim>, Error> {
        let mut out = vec![];
        let ids: Vec<i64> = {
            let mut st = self
                .db
                .prepare("SELECT id FROM curtain_wall WHERE level=?1 ORDER BY id")?;
            let v = st
                .query_map([level], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        for id in ids {
            let l = self.curtain_layout(id)?;
            let sp = &l.spec;
            let half = sp.interior.width().max(sp.border.width()).max(0.05) / 2.0;
            let ax = ddc_geom::curtain::Axis::new(
                ddc_geom::v2(sp.a.0, sp.a.1),
                ddc_geom::v2(sp.b.0, sp.b.1),
                sp.bulge,
            )
            .map_err(|e| Error::Invalid(format!("curtain wall {id}: {e:?}")))?;
            let at = |s: f64, o: f64| {
                let p = ax.point_at(s) + ax.left_at(s) * o;
                (p.x, p.y)
            };
            let nodes = &l.layout.u;
            for w in nodes.windows(2) {
                for o in [-half, half] {
                    out.push(ViewPrim::CurtainLine {
                        curtain: id,
                        a: at(w[0], o),
                        b: at(w[1], o),
                    });
                }
            }
            for &s in nodes {
                out.push(ViewPrim::CurtainLine {
                    curtain: id,
                    a: at(s, -half),
                    b: at(s, half),
                });
            }
        }
        Ok(out)
    }
}

/// Chains along the extent sides: openings of walls whose outer face lies on the side, and the overall dimension.
fn dims(walls: &[WallRow], ops: &[(usize, OpRow)], bb: [f64; 4]) -> Vec<ViewPrim> {
    // Tolerance for a face coinciding with an extent side, m.
    let eps = 1e-3;
    let mut out = vec![];
    for side in [Side::South, Side::North, Side::West, Side::East] {
        let horiz = matches!(side, Side::South | Side::North);
        let c = match side {
            Side::South => bb[1],
            Side::North => bb[3],
            Side::West => bb[0],
            Side::East => bb[2],
        };
        let (lo, hi) = if horiz {
            (bb[0], bb[2])
        } else {
            (bb[1], bb[3])
        };
        let pt = |t: f64| if horiz { (t, c) } else { (c, t) };
        let mut edges: Vec<f64> = vec![];
        for (i, o) in ops {
            let w = &walls[*i];
            if w.arc.is_some() {
                continue;
            }
            let (dx, dy) = (w.b.0 - w.a.0, w.b.1 - w.a.1);
            let len = (dx * dx + dy * dy).sqrt();
            let d = (dx / len, dy / len);
            if (if horiz { d.1 } else { d.0 }).abs() > 1e-9 {
                continue;
            }
            let n = (-d.1, d.0);
            let on = [w.offset0, w.offset0 + w.total].iter().any(|&f| {
                let q = (w.a.0 + n.0 * f, w.a.1 + n.1 * f);
                ((if horiz { q.1 } else { q.0 }) - c).abs() < eps
            });
            if !on {
                continue;
            }
            for t in [o.along, o.along + o.width] {
                let q = (w.a.0 + d.0 * t, w.a.1 + d.1 * t);
                edges.push(if horiz { q.0 } else { q.1 });
            }
        }
        if !edges.is_empty() {
            let mut pts = vec![lo];
            pts.extend(edges);
            pts.push(hi);
            pts.sort_by(f64::total_cmp);
            pts.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
            for k in 1..pts.len() {
                out.push(ViewPrim::Dim {
                    side,
                    chain: 0,
                    a: pt(pts[k - 1]),
                    b: pt(pts[k]),
                    value: pts[k] - pts[k - 1],
                });
            }
        }
        out.push(ViewPrim::Dim {
            side,
            chain: 1,
            a: pt(lo),
            b: pt(hi),
            value: hi - lo,
        });
    }
    out
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::{LayerSpec, OpeningSpec, RoomSpec, Swing, WallSpec};

    /// A 6x4 m room by outer faces, 200 wall inward, a 900 door on the south, a 1500 window on the north.
    pub(crate) fn room_6x4(layers: &[(&str, f64)]) -> (Model, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let ls: Vec<LayerSpec> = layers
            .iter()
            .map(|(n, t)| LayerSpec {
                material: n.to_string(),
                thickness: *t,
            })
            .collect();
        let wt = m.add_wall_type("W", &ls).unwrap();
        let c = [(0.0, 0.0), (6.0, 0.0), (6.0, 4.0), (0.0, 4.0)];
        let specs: Vec<WallSpec> = (0..4)
            .map(|k| WallSpec {
                a: c[k],
                b: c[(k + 1) % 4],
                level: lv,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: Some(0.0),
            })
            .collect();
        let ids = m.add_walls(&specs).unwrap();
        m.add_opening_with(
            ids[0],
            &OpeningSpec {
                along: 1.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
            Swing {
                side: false,
                hinge: false,
            },
        )
        .unwrap();
        m.add_opening(
            ids[2],
            &OpeningSpec {
                along: 2.0,
                width: 1.5,
                sill: 0.9,
                height: 1.5,
                kind: "window".into(),
            },
        )
        .unwrap();
        m.add_room(&RoomSpec {
            level: lv,
            name: "Office".into(),
            number: "101".into(),
            point: (3.0, 2.0),
            height: 3.0,
        })
        .unwrap();
        (m, lv)
    }

    fn near_seg(a: (P, P), b: (P, P)) -> bool {
        [
            a.0 .0 - b.0 .0,
            a.0 .1 - b.0 .1,
            a.1 .0 - b.1 .0,
            a.1 .1 - b.1 .1,
        ]
        .iter()
        .all(|d| d.abs() < 1e-9)
    }

    /// A semicircle R=5 m (axis), a 0.2 m wall to the left of the axis, a 0.9 m door at 12 m along the arc.
    #[test]
    fn plan_semicircle_wall_follows_arc() {
        let mut m = Model::open(":memory:").unwrap();
        let lv = m.add_level("L1", 0.0).unwrap();
        let wt = m
            .add_wall_type(
                "W",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_arc_wall(
                &WallSpec {
                    a: (5.0, 0.0),
                    b: (-5.0, 0.0),
                    level: lv,
                    base_offset: 0.0,
                    height: 3.0,
                    wall_type: wt,
                    offset0: Some(0.0),
                },
                1.0,
            )
            .unwrap();
        assert!((m.wall_length(w).unwrap() - 5.0 * std::f64::consts::PI).abs() < 1e-9);
        m.add_opening_with(
            w,
            &OpeningSpec {
                along: 12.0,
                width: 0.9,
                sill: 0.0,
                height: 2.1,
                kind: "door".into(),
            },
            Swing::default(),
        )
        .unwrap();
        let v = m.plan_view(&spec(lv, 1.2)).unwrap();
        let rings: Vec<&Vec<P>> = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::Cut { ring, .. } => Some(ring),
                _ => None,
            })
            .collect();
        assert_eq!(rings.len(), 2);
        // All section vertices lie between the circles R=4.8 and R=5 (not on the chord).
        for r in &rings {
            for q in r.iter() {
                let rad = (q.0 * q.0 + q.1 * q.1).sqrt();
                assert!(rad > 4.8 - 1e-6 && rad < 5.0 + 1e-6, "{q:?} r={rad}");
            }
        }
        // The dome apex (0, 5) is in the section; the extent follows the arc, not the chord.
        assert!((v.bbox[3] - 5.0).abs() < 2e-3, "{:?}", v.bbox);
        let area: f64 = rings.iter().map(|r| area(r).abs()).sum();
        let full = std::f64::consts::PI / 2.0 * (25.0 - 4.8 * 4.8);
        let gap = 0.9 / 5.0 * (25.0 - 4.8 * 4.8) / 2.0;
        assert!(
            (area - (full - gap)).abs() < 2e-3,
            "{area} vs {}",
            full - gap
        );
        // The door hinge is on the arc at s=12.
        let leaf = v.prims.iter().find_map(|p| match p {
            ViewPrim::DoorLeaf { a, .. } => Some(*a),
            _ => None,
        });
        let h = leaf.unwrap();
        let rad = (h.0 * h.0 + h.1 * h.1).sqrt();
        assert!(
            (rad - 4.8).abs() < 1e-9 || (rad - 5.0).abs() < 1e-9,
            "{rad}"
        );
        let ang = h.1.atan2(h.0);
        assert!(
            (ang - 12.0 / 5.0).abs() < 1e-9 || (ang - 12.9 / 5.0).abs() < 1e-9,
            "{ang}"
        );
        // No dimensions are built along arc walls; the overall extent exists.
        assert_eq!(v.count(|p| matches!(p, ViewPrim::Dim { chain: 0, .. })), 0);
    }

    fn spec(lv: i64, h: f64) -> PlanViewSpec {
        PlanViewSpec {
            level: lv,
            cut_height: h,
            grids: vec![],
        }
    }

    #[test]
    fn plan_6x4_counts_and_dims() {
        let (m, lv) = room_6x4(&[("Brick", 0.2)]);
        let v = m.plan_view(&spec(lv, 1.2)).unwrap();
        let n = |k: &str| {
            v.count(|p| {
                k == match p {
                    ViewPrim::Cut { .. } => "cut",
                    ViewPrim::DoorLeaf { .. } => "leaf",
                    ViewPrim::DoorSwing { .. } => "swing",
                    ViewPrim::WindowLine { .. } => "win",
                    ViewPrim::CurtainLine { .. } => "curtain",
                    ViewPrim::RoomTag { .. } => "tag",
                    ViewPrim::Grid { .. } => "grid",
                    ViewPrim::Dim { .. } => "dim",
                    _ => "section",
                }
            })
        };
        assert_eq!(
            [
                n("cut"),
                n("leaf"),
                n("swing"),
                n("win"),
                n("tag"),
                n("grid"),
                n("dim")
            ],
            [6, 1, 1, 3, 1, 0, 12]
        );
        assert_eq!(v.bbox, [0.0, 0.0, 6.0, 4.0]);
        let chain = |s: Side, c: u8| -> Vec<f64> {
            v.prims
                .iter()
                .filter_map(|p| match p {
                    ViewPrim::Dim {
                        side, chain, value, ..
                    } if *side == s && *chain == c => Some((value * 1e6).round() / 1e6),
                    _ => None,
                })
                .collect()
        };
        assert_eq!(chain(Side::South, 0), [1.0, 0.9, 4.1]);
        assert_eq!(chain(Side::North, 0), [2.5, 1.5, 2.0]);
        assert_eq!(chain(Side::South, 1), [6.0]);
        assert_eq!(chain(Side::North, 1), [6.0]);
        assert_eq!(chain(Side::West, 1), [4.0]);
        assert_eq!(chain(Side::East, 1), [4.0]);
        assert!(chain(Side::West, 0).is_empty());
        // The hinge is at the opening start (x=1000) on the inner face (y=200), the leaf swings inward.
        let leaf = v.prims.iter().find_map(|p| match p {
            ViewPrim::DoorLeaf { a, b, .. } => Some((*a, *b)),
            _ => None,
        });
        assert!(
            near_seg(leaf.unwrap(), ((1.0, 0.2), (1.0, 1.1))),
            "{leaf:?}"
        );
        let swing = v.prims.iter().find_map(|p| match p {
            ViewPrim::DoorSwing { radius, a0, a1, .. } => Some((*radius, *a0, *a1)),
            _ => None,
        });
        assert_eq!(swing, Some((0.9, 0.0, 90.0)));
        let tag = v.prims.iter().find_map(|p| match p {
            ViewPrim::RoomTag {
                name, number, area, ..
            } => Some((name.clone(), number.clone(), *area)),
            _ => None,
        });
        let (nm, nb, ar) = tag.unwrap();
        assert_eq!((nm.as_str(), nb.as_str()), ("Office", "101"));
        assert!((ar - 20.16).abs() < 1e-9, "{ar}");
    }

    #[test]
    fn plan_swing_hinge_mirrors_symbol() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let door: i64 = m
            .sql()
            .query_row("SELECT id FROM opening WHERE kind='door'", [], |r| r.get(0))
            .unwrap();
        m.set_opening_swing(
            door,
            Swing {
                side: true,
                hinge: true,
            },
        )
        .unwrap();
        let v = m.plan_view(&spec(lv, 1.2)).unwrap();
        let leaf = v.prims.iter().find_map(|p| match p {
            ViewPrim::DoorLeaf { a, b, .. } => Some((*a, *b)),
            _ => None,
        });
        assert!(
            near_seg(leaf.unwrap(), ((1.9, 0.0), (1.9, -0.9))),
            "{leaf:?}"
        );
    }

    #[test]
    fn plan_layers_and_cut_height() {
        let (m, lv) = room_6x4(&[("Brick", 0.12), ("Insulation", 0.08)]);
        let v = m.plan_view(&spec(lv, 1.2)).unwrap();
        let mats: Vec<&str> = v
            .prims
            .iter()
            .filter_map(|p| match p {
                ViewPrim::Cut { material, .. } => Some(material.as_str()),
                _ => None,
            })
            .collect();
        assert_eq!(mats.len(), 12);
        assert_eq!(mats.iter().filter(|m| **m == "Insulation").count(), 6);
        // Below the sill the window is not cut: the north wall is solid, no window.
        let v = m.plan_view(&spec(lv, 0.5)).unwrap();
        assert_eq!(v.count(|p| matches!(p, ViewPrim::WindowLine { .. })), 0);
        assert_eq!(v.count(|p| matches!(p, ViewPrim::Cut { .. })), 10);
        // Above the walls the plan is empty.
        let v = m.plan_view(&spec(lv, 3.5)).unwrap();
        assert_eq!(
            v.count(|p| matches!(p, ViewPrim::Cut { .. } | ViewPrim::Dim { .. })),
            0
        );
    }

    #[test]
    fn plan_grids_and_determinism() {
        let (m, lv) = room_6x4(&[("Brick", 0.2)]);
        let mut s = spec(lv, 1.2);
        s.grids = vec![
            GridAxis {
                label: "1".into(),
                a: (0.0, -1.5),
                b: (0.0, 5.5),
            },
            GridAxis {
                label: "A".into(),
                a: (-1.5, 0.0),
                b: (7.5, 0.0),
            },
        ];
        let v = m.plan_view(&s).unwrap();
        assert_eq!(v.count(|p| matches!(p, ViewPrim::Grid { .. })), 2);
        assert_eq!(v, m.plan_view(&s).unwrap());
        assert!(m.plan_view(&spec(999, 1.0)).is_err());
    }
}
