// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Sections and elevations: view definitions in `view_def` (journal, undo), assembly of
//! the model's prisms and output of the same `ViewPrim` primitives as the plan, in
//! (s, z) coordinates: s is along the view line from `a`, z is the absolute level, m.

use ddc_geom::section::{self, ArcVoid, Frame, Prism, Void, Zfn};
use ddc_geom::{v2, Tol, V2};
use rusqlite::{params, Connection, OptionalExtension};

use crate::curtain::PanelKind;
use crate::ops::Inv;
use crate::views::{load_walls, GridAxis, LineClass, PlanView, Side, ViewPrim};
use crate::{Error, Model};

type P = (f64, f64);

/// Width of an opening-frame member in the wall plane, m.
/// Depth (along the view normal) at which a body spanning `lo..hi` is cut: 0 when the plane
/// crosses it; a hair behind the plane when the plane lies on its near face (a section on a
/// ridge, a joint or a face still cuts what is behind it); None when it is not cut.
pub(crate) fn cut_depth(lo: f64, hi: f64) -> Option<f64> {
    const HAIR: f64 = 1e-4;
    if lo.abs() <= HAIR && hi > 2.0 * HAIR {
        Some(lo.max(0.0) + HAIR)
    } else if lo < 0.0 && hi > 0.0 {
        Some(0.0)
    } else {
        None
    }
}

pub const FRAME_W: f64 = 0.05;
/// Maximum frame depth (across the wall thickness), m.
pub const FRAME_D: f64 = 0.1;
pub const LEAF_T: f64 = 0.04;
pub const GLASS_T: f64 = 0.024;
/// Curtain-wall panel thickness, the same as for the curtain-wall solid.
pub const PANEL_T: f64 = 0.025;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ViewKind {
    Section,
    Elevation,
}

impl ViewKind {
    fn as_str(self) -> &'static str {
        match self {
            ViewKind::Section => "section",
            ViewKind::Elevation => "elevation",
        }
    }
    fn parse(s: &str) -> Option<Self> {
        match s {
            "section" => Some(ViewKind::Section),
            "elevation" => Some(ViewKind::Elevation),
            _ => None,
        }
    }
}

/// A section or elevation: line a→b, looking to the left of the direction (reverse with `flipped`),
/// `depth` is the view depth; with `far_clip` everything beyond it is clipped.
#[derive(Clone, Debug, PartialEq)]
pub struct SectionSpec {
    pub kind: ViewKind,
    pub name: String,
    pub a: P,
    pub b: P,
    pub depth: f64,
    pub far_clip: bool,
}

impl SectionSpec {
    /// The same view, looking in the opposite direction.
    pub fn flipped(&self) -> Self {
        let mut s = self.clone();
        std::mem::swap(&mut s.a, &mut s.b);
        s
    }
}

/// Elevation names by compass side (looking toward the model centre).
pub const COMPASS: [&str; 4] = ["North", "South", "East", "West"];

fn check(s: &SectionSpec) -> Result<(), Error> {
    let l = ((s.b.0 - s.a.0).powi(2) + (s.b.1 - s.a.1).powi(2)).sqrt();
    let ok = [s.a.0, s.a.1, s.b.0, s.b.1, s.depth]
        .iter()
        .all(|v| v.is_finite());
    if !ok || l < 1e-6 || s.depth <= 0.0 {
        return Err(Error::Invalid(
            "view line must be longer than 1e-6 m, depth positive".into(),
        ));
    }
    Ok(())
}

fn read(db: &Connection, id: i64) -> Result<Option<SectionSpec>, Error> {
    let r = db
        .query_row(
            "SELECT kind,name,ax,ay,bx,by,depth,far_clip FROM view_def WHERE id=?1 AND kind<>'view3d'",
            [id],
            |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get(1)?,
                    (r.get(2)?, r.get(3)?),
                    (r.get(4)?, r.get(5)?),
                    r.get(6)?,
                    r.get::<_, i64>(7)? != 0,
                ))
            },
        )
        .optional()?;
    let Some((k, name, a, b, depth, far_clip)) = r else {
        return Ok(None);
    };
    let kind = ViewKind::parse(&k).ok_or_else(|| Error::Invalid(format!("view kind {k}")))?;
    Ok(Some(SectionSpec {
        kind,
        name,
        a,
        b,
        depth,
        far_clip,
    }))
}

fn insert(db: &Connection, s: &SectionSpec) -> Result<i64, Error> {
    db.execute(
        "INSERT INTO view_def(kind,name,ax,ay,bx,by,depth,far_clip) VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
        params![
            s.kind.as_str(),
            s.name,
            s.a.0,
            s.a.1,
            s.b.0,
            s.b.1,
            s.depth,
            i64::from(s.far_clip)
        ],
    )?;
    Ok(db.last_insert_rowid())
}

impl Model {
    pub fn add_view_def(&mut self, s: &SectionSpec) -> Result<i64, Error> {
        check(s)?;
        self.op("view_def_add", |db| {
            let id = insert(db, s)?;
            Ok((id, id, Inv::default().delete("view_def", "id", id)))
        })
    }

    pub fn update_view_def(&mut self, id: i64, s: &SectionSpec) -> Result<(), Error> {
        check(s)?;
        self.op("view_def_update", |db| {
            if read(db, id)?.is_none() {
                return Err(Error::Invalid(format!("no such view {id}")));
            }
            let inv = Inv::default().restore(db, "view_def", "id", id)?;
            let n = db.execute(
                "UPDATE view_def SET kind=?2,name=?3,ax=?4,ay=?5,bx=?6,by=?7,depth=?8,far_clip=?9 WHERE id=?1 AND kind<>'view3d'",
                params![
                    id,
                    s.kind.as_str(),
                    s.name,
                    s.a.0,
                    s.a.1,
                    s.b.0,
                    s.b.1,
                    s.depth,
                    i64::from(s.far_clip)
                ],
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("no such view {id}")));
            }
            Ok(((), id, inv))
        })
    }

    pub fn delete_view_def(&mut self, id: i64) -> Result<(), Error> {
        self.op("view_def_delete", |db| {
            // a 3D view is deleted by its own call (with a description in `prop`)
            if read(db, id)?.is_none() {
                return Err(Error::Invalid(format!("no such view {id}")));
            }
            let inv = Inv::default().restore(db, "view_def", "id", id)?;
            if db.execute("DELETE FROM view_def WHERE id=?1 AND kind<>'view3d'", [id])? == 0 {
                return Err(Error::Invalid(format!("no such view {id}")));
            }
            let inv = crate::datum_view::drop_for_view(db, inv, &format!("section:{id}"))?;
            let inv = crate::annot_text::drop_view(db, inv, crate::AnnoView::Section(id))?;
            // the view's viewports on sheets are removed with it
            let inv = inv.then(crate::sheets::drop_viewports(db, "view_def", id)?);
            Ok(((), id, inv))
        })
    }

    pub fn view_def(&self, id: i64) -> Result<SectionSpec, Error> {
        read(&self.db, id)?.ok_or_else(|| Error::Invalid(format!("no such view {id}")))
    }

    /// All sections and elevations in ascending id order.
    pub fn view_defs(&self) -> Result<Vec<(i64, SectionSpec)>, Error> {
        let ids: Vec<i64> = {
            // saved 3D views (kind = 'view3d') live in the same table, see views3d
            let mut st = self
                .db
                .prepare("SELECT id FROM view_def WHERE kind<>'view3d' ORDER BY id")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        ids.into_iter()
            .map(|id| Ok((id, self.view_def(id)?)))
            .collect()
    }

    /// Four elevations by the model's plan extent with offset `margin`; elevations with the same
    /// names are not duplicated. One journal operation. Returns the ids of the created ones.
    pub fn add_compass_elevations(&mut self, margin: f64) -> Result<Vec<i64>, Error> {
        let have: Vec<String> = self
            .view_defs()?
            .into_iter()
            .filter(|(_, s)| s.kind == ViewKind::Elevation)
            .map(|(_, s)| s.name)
            .collect();
        let specs: Vec<SectionSpec> = self
            .compass_elevation_specs(margin)?
            .into_iter()
            .filter(|s| !have.iter().any(|h| *h == s.name))
            .collect();
        if specs.is_empty() {
            return Ok(vec![]);
        }
        self.op("view_def_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in &specs {
                let id = insert(db, s)?;
                inv = inv.then(Inv::default().delete("view_def", "id", id));
                ids.push(id);
            }
            let first = ids[0];
            Ok((ids, first, inv))
        })
    }

    /// Stair bodies for sections and elevations: (table, id, material, mesh), by id.
    #[allow(clippy::type_complexity)]
    fn stair_bodies(
        &self,
    ) -> Result<Vec<(&'static str, i64, String, ddc_geom::mesh::Mesh)>, Error> {
        let ids: Vec<i64> = {
            let mut st = self.db.prepare("SELECT id FROM stair ORDER BY id")?;
            let v = st
                .query_map([], |r| r.get(0))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut out = vec![];
        for id in ids {
            // a stair whose levels no longer resolve is left out of the view, not an error
            for m in self.stair_meshes(id).unwrap_or_default() {
                out.push(("stair", id, "concrete".to_string(), m));
            }
        }
        Ok(out)
    }

    /// The four compass elevations around the plan extent with `margin`, named by `COMPASS`;
    /// nothing is stored. Empty for a model with nothing in plan.
    pub fn compass_elevation_specs(&self, margin: f64) -> Result<Vec<SectionSpec>, Error> {
        Ok(match self.plan_bbox()? {
            Some(bb) => compass_elevations_around(bb, margin),
            None => vec![],
        })
    }
}

/// The four compass elevations around the plan extent `bb` [x0, y0, x1, y1] with `margin`.
pub fn compass_elevations_around(bb: [f64; 4], margin: f64) -> Vec<SectionSpec> {
    let m = margin.max(0.0);
    let (x0, y0, x1, y1) = (bb[0] - m, bb[1] - m, bb[2] + m, bb[3] + m);
    let depth = (x1 - x0).max(y1 - y0) + m;
    // The view looks left of a→b: north faces south, south faces north, east faces west, west faces east.
    let lines = [
        ((x1, y1), (x0, y1)),
        ((x0, y0), (x1, y0)),
        ((x1, y0), (x1, y1)),
        ((x0, y1), (x0, y0)),
    ];
    COMPASS
        .iter()
        .zip(lines)
        .map(|(n, (a, b))| SectionSpec {
            kind: ViewKind::Elevation,
            name: n.to_string(),
            a,
            b,
            depth,
            far_clip: false,
        })
        .collect()
}

impl Model {
    /// Full plan extent: the outlines of all scene solids (wall thickness, arcs, slabs, mullions).
    pub fn plan_bbox(&self) -> Result<Option<[f64; 4]>, Error> {
        let sc = self.scene()?;
        let mut bb = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        for p in sc.prisms.iter().flat_map(|p| p.rings.iter().flatten()) {
            bb = [
                bb[0].min(p.x),
                bb[1].min(p.y),
                bb[2].max(p.x),
                bb[3].max(p.y),
            ];
        }
        // solids (scripted members, plates, free forms) by their stored extent
        let mut st = self.db.prepare(
            "SELECT MIN(q.x0), MIN(q.y0), MAX(q.x1), MAX(q.y1) FROM solid_qty q JOIN solid s ON s.id = q.solid WHERE s.void = 0",
        )?;
        let ext: Option<[f64; 4]> = st.query_row([], |r| {
            Ok(match (r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?) {
                (Some(a), Some(b), Some(c), Some(d)) => Some([a, b, c, d]),
                _ => None,
            })
        })?;
        if let Some(e) = ext {
            bb = [bb[0].min(e[0]), bb[1].min(e[1]), bb[2].max(e[2]), bb[3].max(e[3])];
        }
        Ok(bb[0].is_finite().then_some(bb))
    }

    /// Solids of shapes, columns, beams, roofs and walls/slabs cut by joints, in a section:
    /// the plane cut and the edges in the view depth (prisms of the cut elements are replaced).
    fn cut_section_prims(
        &self,
        prims: &mut Vec<ViewPrim>,
        a: V2,
        u: V2,
        depth: f64,
        is_cut: bool,
        grow: &mut dyn FnMut(V2),
    ) -> Result<(), Error> {
        let cut: Vec<(String, i64)> = self
            .cut_elements()?
            .into_iter()
            .filter(|c| c.0 == "wall" || c.0 == "slab")
            .collect();
        let mut bodies: Vec<(&'static str, i64, String, ddc_geom::csg::Body)> = vec![];
        for (k, id) in &cut {
            let table: &'static str = if k == "wall" { "wall" } else { "slab" };
            let fallback = prims
                .iter()
                .find_map(|p| match p {
                    ViewPrim::SectionCut {
                        table: t,
                        element,
                        material,
                        ..
                    } if *t == table && element == id => Some(material.clone()),
                    _ => None,
                })
                .unwrap_or_else(|| "concrete".into());
            // each layer keeps its own material in the cut
            let sql = if table == "wall" {
                "SELECT l.pos,l.material FROM wall_type_layer l JOIN wall w ON w.wtype=l.wtype                  WHERE w.id=?1"
            } else {
                "SELECT l.pos,l.material FROM slab_type_layer l JOIN slab s ON s.stype=l.stype                  WHERE s.id=?1"
            };
            let mats: Vec<(i64, String)> = self
                .db
                .prepare(sql)?
                .query_map([*id], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let Ok(parts) = self.joined_parts(table, *id) else {
                // the uncut prisms stay when the cut cannot be built
                continue;
            };
            prims.retain(|p| match p {
                ViewPrim::SectionCut {
                    table: t, element, ..
                }
                | ViewPrim::Edge {
                    table: t, element, ..
                } => !(*t == table && element == id),
                _ => true,
            });
            for (pos, b) in parts {
                let mat = mats
                    .iter()
                    .find(|m| m.0 == pos)
                    .map_or_else(|| fallback.clone(), |m| m.1.clone());
                bodies.push((table, *id, mat, b));
            }
        }
        for (kind, id, mesh, _) in self.struct_bodies(None)? {
            let table: &'static str = match kind {
                "column" => "col",
                k => k,
            };
            let mat = if table == "mass" { "mass" } else { "concrete" };
            bodies.push((
                table,
                id,
                mat.into(),
                ddc_geom::csg::Body::from_mesh(&mesh, 0),
            ));
        }
        let n = v2(-u.y, u.x);
        let c = n.dot(a);
        for (table, id, mat, b) in bodies {
            let Some((lo, hi)) = b.bbox() else { continue };
            let ws = [lo.x, hi.x].map(|x| [lo.y, hi.y].map(|y| n.dot(v2(x, y)) - c));
            let (w0, w1) = ws
                .iter()
                .flatten()
                .fold((f64::MAX, f64::MIN), |(p, q), v| (p.min(*v), q.max(*v)));
            if w1 < 0.0 || w0 > depth {
                continue;
            }
            let at = cut_depth(w0, w1);
            let crossed = at.is_some();
            if let (true, Some(d)) = (is_cut, at) {
                for r in ddc_geom::mass::vertical_rings(&b, a + n * d, u) {
                    r.iter().for_each(|p| grow(*p));
                    prims.push(ViewPrim::SectionCut {
                        table,
                        element: id,
                        material: mat.clone(),
                        rings: r.iter().map(|p| (p.x, p.y)).collect(),
                        holes: vec![],
                    });
                }
            }
            let class = if !is_cut || crossed {
                LineClass::Projection
            } else {
                LineClass::Beyond
            };
            let to = |p: ddc_geom::V3| v2((v2(p.x, p.y) - a).dot(u), p.z);
            for (p, q) in b.feature_edges(1e-7, 0.966) {
                let (wp, wq) = (n.dot(v2(p.x, p.y)) - c, n.dot(v2(q.x, q.y)) - c);
                if is_cut && (wp < -1e-9 || wq < -1e-9) || wp > depth || wq > depth {
                    continue;
                }
                let (pa, pb) = (to(p), to(q));
                grow(pa);
                grow(pb);
                prims.push(ViewPrim::Edge {
                    table,
                    element: id,
                    class,
                    a: (pa.x, pa.y),
                    b: (pb.x, pb.y),
                });
            }
        }
        Ok(())
    }

    /// The view of a saved section/elevation.
    pub fn draw_view_def(&self, id: i64, grids: &[GridAxis]) -> Result<PlanView, Error> {
        let mut v = self.section_view(&self.view_def(id)?, grids)?;
        v.prims.extend(crate::annot_text::view_prims(
            self,
            crate::AnnoView::Section(id),
        )?);
        self.hide_in_drawing(crate::phases::ViewKey::Section(id), &mut v.prims)?;
        Ok(v)
    }

    /// Section/elevation: cuts (hatching by material), visible edges (projection for elements
    /// crossed by the plane, otherwise those beyond it; for an elevation everything is projection),
    /// levels with elevations, axes crossing the view line, the storey-height chain, and room
    /// tags on the section line.
    pub fn section_view(&self, spec: &SectionSpec, grids: &[GridAxis]) -> Result<PlanView, Error> {
        check(spec)?;
        // the model's axes replace the passed ones (auto-axes from walls)
        let own;
        let grids = if crate::datum::has_grids(&self.db)? {
            own = crate::datum::axes_at(&self.db, None)?;
            &own[..]
        } else {
            grids
        };
        let sc = self.scene()?;
        let (a, b) = (v2(spec.a.0, spec.a.1), v2(spec.b.0, spec.b.1));
        let u = (b - a) * (1.0 / (b - a).len());
        let is_cut = spec.kind == ViewKind::Section;
        let fr = Frame {
            o: a,
            u,
            far: spec.far_clip.then_some(spec.depth),
            cut: is_cut,
        };
        let out = section::section_with_arcs(&sc.prisms, &sc.arcs, &fr);
        if ddc_geom::cancel::requested() {
            return Err(Error::Cancelled);
        }
        let mut prims = vec![];
        let mut cut_el: Vec<(&'static str, i64)> = vec![];
        let mut bb = [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ];
        let mut grow = |p: V2| {
            bb = [
                bb[0].min(p.x),
                bb[1].min(p.y),
                bb[2].max(p.x),
                bb[3].max(p.y),
            ];
        };
        if is_cut {
            for c in &out.cuts {
                let m = &sc.meta[c.item];
                cut_el.push((m.2, m.0));
                for (outer, holes) in nest(&c.rings) {
                    outer.iter().for_each(|&p| grow(p));
                    prims.push(ViewPrim::SectionCut {
                        table: m.2,
                        element: m.0,
                        material: m.1.clone(),
                        rings: outer.iter().map(|p| (p.x, p.y)).collect(),
                        holes: holes
                            .iter()
                            .map(|h| h.iter().map(|p| (p.x, p.y)).collect())
                            .collect(),
                    });
                }
            }
        }
        if is_cut {
            for (el, material, ring) in self.topo_section_cuts(a, u, (b - a).len())? {
                ring.iter().for_each(|&p| grow(p));
                prims.push(ViewPrim::SectionCut {
                    table: "toposolid",
                    element: el,
                    material,
                    rings: ring.iter().map(|p| (p.x, p.y)).collect(),
                    holes: vec![],
                });
            }
        }
        cut_el.sort_unstable();
        cut_el.dedup();
        crate::mesh_edit::section_prims(&self.db, a, u, fr.far, is_cut, &mut prims, &mut grow)?;
        let stairs = self.stair_bodies()?;
        crate::mesh_edit::section_bodies(&stairs, a, u, fr.far, is_cut, &mut prims, &mut grow);
        let plants = self.plant_bodies().unwrap_or_default();
        crate::mesh_edit::section_bodies(&plants, a, u, fr.far, is_cut, &mut prims, &mut grow);
        for l in &out.lines {
            let (el, tb) = (sc.meta[l.item].0, sc.meta[l.item].2);
            grow(l.a);
            grow(l.b);
            let class = if !is_cut || cut_el.binary_search(&(tb, el)).is_ok() {
                LineClass::Projection
            } else {
                LineClass::Beyond
            };
            prims.push(ViewPrim::Edge {
                table: tb,
                element: el,
                class,
                a: (l.a.x, l.a.y),
                b: (l.b.x, l.b.y),
            });
        }
        self.cut_section_prims(&mut prims, a, u, spec.depth, is_cut, &mut grow)?;
        let depth = spec.far_clip.then_some(spec.depth);
        // a network with a graph error does not break the view: shown without it, but with a message
        let mep = self
            .mep_section_prims(a, u, depth, is_cut)
            .unwrap_or_else(|e| {
                eprintln!("section: MEP skipped: {e}");
                vec![]
            });
        for q in mep {
            match &q {
                ViewPrim::SectionCut { rings, .. } => {
                    rings.iter().for_each(|&(x, y)| grow(v2(x, y)))
                }
                ViewPrim::Edge { a, b, .. } => {
                    grow(v2(a.0, a.1));
                    grow(v2(b.0, b.1));
                }
                _ => {}
            }
            prims.push(q);
        }
        let (sp, spts) = crate::solids::section_prims(&self.db, a, u, fr.far, is_cut)?;
        spts.into_iter().for_each(&mut grow);
        prims.extend(sp);
        let (rp, rpts) = self.rebar_section_prims(a, u, fr.far, is_cut)?;
        rpts.into_iter().for_each(&mut grow);
        prims.extend(rp);
        if is_cut {
            for r in &sc.rooms {
                let iv = section::line_intervals(&r.rings, a, u);
                if let Some(&(s0, s1)) =
                    iv.iter().max_by(|p, q| (p.1 - p.0).total_cmp(&(q.1 - q.0)))
                {
                    prims.push(ViewPrim::RoomTag {
                        room: r.id,
                        pos: ((s0 + s1) / 2.0, r.z0 + r.height / 2.0),
                        name: r.name.clone(),
                        number: r.number.clone(),
                        area: r.area,
                        finish: None,
                        cut: Some((s1 - s0, r.height)),
                    });
                }
            }
        }
        if !bb[0].is_finite() {
            return Ok(PlanView {
                prims,
                bbox: [0.0; 4],
            });
        }
        // Levels: a line across the full view width with a 1 m extension; the storey-height chain on the left.
        let ext = 1.0;
        for (name, z) in &sc.levels {
            prims.push(ViewPrim::Level {
                name: name.clone(),
                elevation: *z,
                a: (bb[0] - ext, *z),
                b: (bb[2] + ext, *z),
            });
        }
        let zs: Vec<f64> = sc.levels.iter().map(|l| l.1).collect();
        for w in zs.windows(2) {
            prims.push(ViewPrim::Dim {
                side: Side::West,
                chain: 0,
                a: (bb[0], w[0]),
                b: (bb[0], w[1]),
                value: w[1] - w[0],
            });
        }
        if zs.len() > 2 {
            prims.push(ViewPrim::Dim {
                side: Side::West,
                chain: 1,
                a: (bb[0], zs[0]),
                b: (bb[0], zs[zs.len() - 1]),
                value: zs[zs.len() - 1] - zs[0],
            });
        }
        let (z0, z1) = (
            bb[1].min(zs.first().copied().unwrap_or(bb[1])),
            bb[3].max(zs.last().copied().unwrap_or(bb[3])),
        );
        for g in grids {
            let (ga, gb) = (v2(g.a.0, g.a.1), v2(g.b.0, g.b.1));
            let gd = gb - ga;
            let gl = gd.len();
            if gl < 1e-9 {
                continue;
            }
            let cr = u.cross(gd * (1.0 / gl));
            // Axes nearly parallel to the view line (< ~6°) are not shown.
            if cr.abs() < 0.1 {
                continue;
            }
            // a + u·s = ga + gd·t  ⇒  s = (ga − a)×gd / (u×gd)
            let s = (ga - a).cross(gd) / u.cross(gd);
            prims.push(ViewPrim::Grid {
                label: g.label.clone(),
                a: (s, z0 - ext),
                b: (s, z1 + ext),
                bubbles: (false, true),
            });
        }
        if ddc_geom::cancel::requested() {
            return Err(Error::Cancelled);
        }
        Ok(PlanView { prims, bbox: bb })
    }
}

/// Section outlines: outer ones (counter-clockwise) with their holes.
fn nest(rings: &[Vec<V2>]) -> Vec<(Vec<V2>, Vec<Vec<V2>>)> {
    let mut outs: Vec<(Vec<V2>, Vec<Vec<V2>>)> = rings
        .iter()
        .filter(|r| section::signed_area(r) > 0.0)
        .map(|r| (r.clone(), vec![]))
        .collect();
    for h in rings.iter().filter(|r| section::signed_area(r) <= 0.0) {
        let p = h[0];
        let best = outs
            .iter()
            .enumerate()
            .filter(|(_, o)| inside(p, &o.0))
            .min_by(|x, y| section::signed_area(&x.1 .0).total_cmp(&section::signed_area(&y.1 .0)))
            .map(|(i, _)| i);
        if let Some(i) = best {
            outs[i].1.push(h.clone());
        }
    }
    outs
}

fn inside(p: V2, r: &[V2]) -> bool {
    let mut c = false;
    for i in 0..r.len() {
        let (a, b) = (r[i], r[(i + 1) % r.len()]);
        if (a.y > p.y) != (b.y > p.y) && p.x < a.x + (b.x - a.x) * (p.y - a.y) / (b.y - a.y) {
            c = !c;
        }
    }
    c
}

struct RoomRow {
    id: i64,
    name: String,
    number: String,
    z0: f64,
    height: f64,
    area: f64,
    rings: Vec<Vec<V2>>,
}

/// Model prisms and their (element, material); levels by elevation; rooms.
struct Scene {
    prisms: Vec<Prism>,
    /// Voids of arc walls, parallel to `prisms`.
    arcs: Vec<Vec<ArcVoid>>,
    meta: Vec<(i64, String, &'static str)>,
    levels: Vec<(String, f64)>,
    rooms: Vec<RoomRow>,
}

fn ring(ps: &[P]) -> Vec<V2> {
    ps.iter().map(|p| v2(p.0, p.1)).collect()
}

/// A plan rectangle: from `a` along `d` over [t0, t1], along the normal `n` over [o0, o1] (counter-clockwise).
fn quad(a: V2, d: V2, n: V2, t: (f64, f64), o: (f64, f64)) -> Vec<V2> {
    let at = |t: f64, o: f64| a + d * t + n * o;
    let r = vec![at(t.0, o.0), at(t.1, o.0), at(t.1, o.1), at(t.0, o.1)];
    if section::signed_area(&r) < 0.0 {
        r.into_iter().rev().collect()
    } else {
        r
    }
}

/// Plane z(p) through three plan points with elevations; a degenerate triangle gives None.
fn tri_plane(p: [V2; 3], z: [f64; 3]) -> Option<Zfn> {
    let (u, v) = (p[1] - p[0], p[2] - p[0]);
    let det = u.cross(v);
    if det.abs() < 1e-14 {
        return None;
    }
    let (du, dv) = (z[1] - z[0], z[2] - z[0]);
    // grad·u = du, grad·v = dv
    let gx = (du * v.y - dv * u.y) / det;
    let gy = (dv * u.x - du * v.x) / det;
    Some(Zfn {
        o: p[0],
        z: z[0],
        grad: v2(gx, gy),
        dir: v2(1.0, 0.0),
        prof: vec![],
    })
}

fn boxp_rings(rings: Vec<Vec<V2>>, z0: f64, z1: f64) -> Prism {
    Prism {
        rings,
        bottom: Zfn::flat(z0),
        top: Zfn::flat(z1),
        voids: vec![],
    }
}

fn boxp(r: Vec<V2>, z0: f64, z1: f64) -> Prism {
    Prism {
        rings: vec![r],
        bottom: Zfn::flat(z0),
        top: Zfn::flat(z1),
        voids: vec![],
    }
}

impl Model {
    fn scene(&self) -> Result<Scene, Error> {
        let tol = Tol::default();
        let db = &self.db;
        let mut prisms = vec![];
        let mut meta: Vec<(i64, String, &'static str)> = vec![];
        let mut arc_voids: Vec<(usize, Vec<ArcVoid>)> = vec![];
        let mut push = |p: Prism, el: i64, mat: &str, prisms: &mut Vec<Prism>, t: &'static str| {
            prisms.push(p);
            meta.push((el, mat.to_string(), t));
        };
        let levels: Vec<(i64, String, f64)> = {
            let mut st =
                db.prepare("SELECT id, name, elevation FROM level ORDER BY elevation, id")?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut op_st = db.prepare(
            "SELECT id,along,width,sill,height,kind FROM opening WHERE wall=?1 ORDER BY along, id",
        )?;
        for (lv, _, elev) in &levels {
            let (walls, joined) = load_walls(db, *lv, *elev, tol)?;
            for (i, w) in walls.iter().enumerate() {
                let base = elev + w.base_offset;
                let straight = w.arc.is_none();
                let (d, n) = if straight {
                    let (d, n) = w.frame();
                    (v2(d.0, d.1), v2(n.0, n.1))
                } else {
                    (v2(1.0, 0.0), v2(0.0, 1.0))
                };
                let a = v2(w.a.0, w.a.1);
                let top = match &w.top {
                    Some(t) => Zfn {
                        o: a,
                        z: base,
                        grad: v2(0.0, 0.0),
                        dir: d,
                        prof: t.clone(),
                    },
                    None => Zfn::flat(base + w.height),
                };
                let bottom = match &w.base {
                    Some(b) if straight => Zfn {
                        o: a,
                        z: base,
                        grad: v2(0.0, 0.0),
                        dir: d,
                        prof: b.clone(),
                    },
                    _ => Zfn::flat(base),
                };
                let ops: Vec<(i64, f64, f64, f64, f64, String)> = op_st
                    .query_map([w.id], |r| {
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
                // an arc-wall opening is a wedge from the centre by the end angles (along is the length along the axis)
                let wedges: Vec<ArcVoid> = match &w.arc {
                    Some(ax) => ops
                        .iter()
                        .filter(|o| o.2 > 0.0 && o.2 < ax.length())
                        .map(|o| {
                            let (t0, t1) = (ax.angle_at(o.1), ax.angle_at(o.1 + o.2));
                            let (t0, t1) = if t0 <= t1 { (t0, t1) } else { (t1, t0) };
                            let t1 = t1.min(t0 + std::f64::consts::PI - 1e-6);
                            ArcVoid {
                                c: ax.center,
                                r0: v2(t0.cos(), t0.sin()),
                                r1: v2(t1.cos(), t1.sin()),
                                z0: base + o.3,
                                z1: base + o.3 + o.4,
                            }
                        })
                        .collect(),
                    None => vec![],
                };
                let voids: Vec<Void> = ops
                    .iter()
                    .filter(|_| straight)
                    .map(|o| Void {
                        o: a,
                        dir: d,
                        u0: o.1,
                        u1: o.1 + o.2,
                        z0: base + o.3,
                        z1: base + o.3 + o.4,
                    })
                    .collect();
                for (l, profs) in joined.rings[i].iter().enumerate() {
                    for pr in profs {
                        let r = pr.tessellate(ddc_geom::arc::CHORD_SAG);
                        // the arc top along the axis length is split into sectors, each with a flat top
                        let parts = match (&w.arc, &w.top) {
                            (Some(ax), Some(t)) => arc_sectors(ax, t, &r, base),
                            _ => vec![(r, top.clone())],
                        };
                        for (ring, top) in parts {
                            push(
                                Prism {
                                    rings: vec![ring],
                                    bottom: bottom.clone(),
                                    top,
                                    voids: voids.clone(),
                                },
                                w.id,
                                &w.mats[l],
                                &mut prisms,
                                "wall",
                            );
                            if !wedges.is_empty() {
                                arc_voids.push((prisms.len() - 1, wedges.clone()));
                            }
                        }
                    }
                }
                // Opening infill: the frame along the thickness axis, a door leaf or window glass.
                let c = w.offset0 + w.total / 2.0;
                let fd = FRAME_D.min(w.total) / 2.0;
                let f = FRAME_W;
                for (id, along, width, sill, height, kind) in ops.iter().filter(|_| straight) {
                    if width <= &(2.0 * f) || height <= &(2.0 * f) {
                        continue;
                    }
                    let (t0, t1) = (*along, along + width);
                    let (z0, z1) = (base + sill, base + sill + height);
                    let q = |t: (f64, f64), h: f64| quad(a, d, n, t, (c - h, c + h));
                    let mut part = |t: (f64, f64), h: f64, z: (f64, f64), mat: &str| {
                        push(boxp(q(t, h), z.0, z.1), *id, mat, &mut prisms, "opening");
                    };
                    let door = kind == "door";
                    if !door && kind != "window" {
                        continue;
                    }
                    part((t0, t0 + f), fd, (z0, z1), "Frame");
                    part((t1 - f, t1), fd, (z0, z1), "Frame");
                    part((t0 + f, t1 - f), fd, (z1 - f, z1), "Frame");
                    if door {
                        part((t0 + f, t1 - f), LEAF_T / 2.0, (z0, z1 - f), "Door");
                    } else {
                        part((t0 + f, t1 - f), fd, (z0, z0 + f), "Frame");
                        part((t0 + f, t1 - f), GLASS_T / 2.0, (z0 + f, z1 - f), "Glass");
                    }
                }
            }
        }
        // Slabs: layers from top to bottom, starting at the sloped top.
        let slabs: Vec<(i64, i64, f64)> = {
            let mut st = db.prepare(
                "SELECT s.id, s.stype, lv.elevation + s.offset FROM slab s JOIN level lv ON lv.id=s.level ORDER BY s.id",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                .collect::<Result<_, _>>()?;
            v
        };
        let mut ls = db.prepare(
            "SELECT material, thickness FROM slab_type_layer WHERE stype=?1 ORDER BY pos",
        )?;
        for (id, st, top) in slabs {
            let rings: Vec<Vec<V2>> = crate::slabs::rings(db, id)?
                .iter()
                .map(|r| ring(r))
                .collect();
            if rings.is_empty() {
                continue;
            }
            // plan after cutouts (shafts, openings, notches, pieces): rings in sequence, even-odd
            let rings = match crate::cuts::slab_cut(db, id)? {
                Some(c) => c.rings(),
                None => rings,
            };
            let pl = self.slab_slope(id)?;
            let layers: Vec<(String, f64)> = ls
                .query_map([st], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            // slab shape: layer prisms for each top triangle (a plane through three
            // points); with variable thickness the lower layers start from the flat bottom, the top one takes the remainder
            if let Some(sm) = crate::sketch::shape_mesh(db, id)? {
                let t: f64 = layers.iter().map(|l| l.1).sum();
                let rise = |p: V2| pl.map_or(0.0, |pl| pl.rise(p));
                for &tri in &sm.tris {
                    let mut tri = tri;
                    let q = tri.map(|k| sm.pts[k]);
                    if (q[1] - q[0]).cross(q[2] - q[0]) < 0.0 {
                        tri.swap(1, 2);
                    }
                    let p = tri.map(|k| sm.pts[k]);
                    let z = tri.map(|k| top + sm.dz[k] + rise(sm.pts[k]));
                    let Some(zt) = tri_plane(p, z) else { continue };
                    let mut zb = zt.clone();
                    zb.z -= t;
                    let varying = sm.mode == ddc_geom::shape_edit::Thickness::Varying;
                    if varying {
                        zb = tri_plane(p, tri.map(|k| top - t + rise(sm.pts[k]))).unwrap_or(zb);
                    }
                    let mut layer = |mat: &str, lo: Zfn, hi: Zfn| {
                        push(
                            Prism {
                                rings: vec![p.to_vec()],
                                bottom: lo,
                                top: hi,
                                voids: vec![],
                            },
                            id,
                            mat,
                            &mut prisms,
                            "slab",
                        )
                    };
                    if layers.is_empty() {
                        layer("Concrete", zb, zt);
                    } else if varying {
                        let mut lo = zb;
                        for (mat, lt) in layers[1..].iter().rev() {
                            let mut hi = lo.clone();
                            hi.z += lt;
                            layer(mat, lo, hi.clone());
                            lo = hi;
                        }
                        layer(&layers[0].0, lo, zt);
                    } else {
                        let mut hi = zt;
                        for (mat, lt) in &layers {
                            let mut lo = hi.clone();
                            lo.z -= lt;
                            layer(mat, lo.clone(), hi);
                            hi = lo;
                        }
                    }
                }
                continue;
            }
            let mut z = Zfn::flat(top);
            if let Some(p) = pl {
                z.o = p.p0;
                z.grad = v2(p.gx, p.gy);
            }
            for (mat, t) in layers {
                let mut bottom = z.clone();
                bottom.z -= t;
                push(
                    Prism {
                        rings: rings.clone(),
                        bottom: bottom.clone(),
                        top: z.clone(),
                        voids: vec![],
                    },
                    id,
                    &mat,
                    &mut prisms,
                    "slab",
                );
                z = bottom;
            }
        }
        // Ceilings: layers from top to bottom starting at the bottom + thicknesses; shafts are holes.
        for (id, st, bottom) in crate::shafts::ceilings(db)? {
            let mut rings: Vec<Vec<V2>> = crate::shafts::ceiling_rings(db, id)?
                .iter()
                .map(|r| ring(r))
                .collect();
            if rings.is_empty() {
                continue;
            }
            let layers: Vec<(String, f64)> = ls
                .query_map([st], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            let mut z = bottom + layers.iter().map(|l| l.1).sum::<f64>();
            if let Some(c) = crate::cuts::ceiling_cut(db, id)? {
                rings = c.rings();
            }
            for (mat, t) in layers {
                push(
                    boxp_rings(rings.clone(), z - t, z),
                    id,
                    &mat,
                    &mut prisms,
                    "ceiling",
                );
                z -= t;
            }
        }
        // Stairs: flights with steps, and landings.
        for (id, mat, p) in crate::stairs::prisms(db)? {
            push(p, id, &mat, &mut prisms, "stair");
        }
        for (id, mat, p) in crate::ramps::prisms(db)? {
            push(p, id, &mat, &mut prisms, "ramp");
        }
        // Curtain walls: panels (except empty ones) and mullions.
        let cws: Vec<i64> = {
            let mut st = db.prepare("SELECT id FROM curtain_wall ORDER BY id")?;
            let v = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
            v
        };
        for id in cws {
            let l = self.curtain_layout(id)?;
            for (k, c) in l.layout.cells.iter().enumerate() {
                let kind = l.panels.get(k).map(|p| p.kind).unwrap_or_default();
                let mat = match kind {
                    PanelKind::Empty => continue,
                    PanelKind::Solid => "Panel",
                    PanelKind::Door => "Door",
                    PanelKind::Glazed | PanelKind::Window => "Glass",
                };
                let (p0, p1) = (
                    v2(c.corners[0].x, c.corners[0].y),
                    v2(c.corners[1].x, c.corners[1].y),
                );
                let along = p1 - p0;
                if along.len() <= 1e-9 || c.net_area <= 1e-9 {
                    continue;
                }
                let d = along * (1.0 / along.len());
                let n = v2(-d.y, d.x);
                let h = PANEL_T / 2.0;
                let x = |p: ddc_geom::V3| (v2(p.x, p.y) - p0).dot(d);
                let w = (c.net[1] - c.net[0]).len();
                if (c.net_area - w * (c.net[3] - c.net[0]).len()).abs() <= 1e-12 {
                    let t = (x(c.net[0]), x(c.net[1]));
                    push(
                        boxp(quad(p0, d, n, t, (-h, h)), c.net[0].z, c.net[3].z),
                        id,
                        mat,
                        &mut prisms,
                        "curtain_wall",
                    );
                    continue;
                }
                // Trimmed by a polyline top: the top is the upper chain of the panel outline.
                // The top is the whole outline chain without the bottom corner points (including vertices at hmin:
                // otherwise a triangular panel degenerates into a rectangle).
                let hmin = c.outline.iter().map(|q| q.1).fold(f64::INFINITY, f64::min);
                let xs = c.outline.iter().map(|q| q.0);
                let (xa, xb) = (
                    xs.clone().fold(f64::INFINITY, f64::min),
                    xs.fold(f64::NEG_INFINITY, f64::max),
                );
                let mut prof: Vec<(f64, f64)> = c.outline.iter().skip(2).copied().collect();
                prof.sort_by(|p, q| p.0.total_cmp(&q.0));
                push(
                    Prism {
                        rings: vec![quad(p0, d, n, (xa, xb), (-h, h))],
                        bottom: Zfn::flat(l.z0 + hmin),
                        top: Zfn {
                            o: p0,
                            z: l.z0,
                            grad: v2(0.0, 0.0),
                            dir: d,
                            prof,
                        },
                        voids: vec![],
                    },
                    id,
                    mat,
                    &mut prisms,
                    "curtain_wall",
                );
            }
            for bar in &l.layout.bars {
                let (w, dd) = match bar.section {
                    ddc_geom::curtain::Section::None => continue,
                    ddc_geom::curtain::Section::Rect { w, d } => (w, d),
                    ddc_geom::curtain::Section::Circle { d } => (d, d),
                };
                let sh = bar.inward * (w / 2.0);
                let (pa, pb) = (bar.a + sh, bar.b + sh);
                let nrm = v2(bar.normal.x, bar.normal.y);
                if bar.vertical {
                    let t = v2(nrm.y, -nrm.x);
                    let c = v2(pa.x, pa.y);
                    let r = quad(c, t, nrm, (-w / 2.0, w / 2.0), (-dd / 2.0, dd / 2.0));
                    push(
                        boxp(r, pa.z.min(pb.z), pa.z.max(pb.z)),
                        id,
                        "Mullion",
                        &mut prisms,
                        "curtain_wall",
                    );
                    continue;
                }
                let (ca, cb) = (v2(pa.x, pa.y), v2(pb.x, pb.y));
                let len = (cb - ca).len();
                if len <= 1e-9 {
                    continue;
                }
                let e = (cb - ca) * (1.0 / len);
                let r = quad(ca, e, v2(-e.y, e.x), (0.0, len), (-dd / 2.0, dd / 2.0));
                let lift = |dz: f64| Zfn {
                    o: ca,
                    z: pa.z + dz,
                    grad: v2(0.0, 0.0),
                    dir: e,
                    prof: if (pb.z - pa.z).abs() > 1e-12 {
                        vec![(0.0, 0.0), (len, pb.z - pa.z)]
                    } else {
                        vec![]
                    },
                };
                push(
                    Prism {
                        rings: vec![r],
                        bottom: lift(-w / 2.0),
                        top: lift(w / 2.0),
                        voids: vec![],
                    },
                    id,
                    "Mullion",
                    &mut prisms,
                    "curtain_wall",
                );
            }
        }
        // foundations, bracing, trusses, beam systems (schema 26)
        for (table, id, mat, p) in self.struct_section_prisms()? {
            push(p, id, &mat, &mut prisms, table);
        }
        let mut rooms = vec![];
        {
            let mut st = db.prepare(
                "SELECT r.id, r.name, r.number, lv.elevation, r.height, q.area, q.rings FROM room r \
                 JOIN room_qty q ON q.room=r.id JOIN level lv ON lv.id=r.level ORDER BY r.id",
            )?;
            let rows: Vec<(i64, String, String, f64, f64, f64, String)> = st
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
            for (id, name, number, z0, height, area, rs) in rows {
                let rs: Vec<Vec<P>> = serde_json::from_str(&rs)?;
                rooms.push(RoomRow {
                    id,
                    name,
                    number,
                    z0,
                    height,
                    area,
                    rings: rs.iter().map(|r| ring(r)).collect(),
                });
            }
        }
        let mut arcs = vec![vec![]; prisms.len()];
        for (k, v) in arc_voids {
            arcs[k] = v;
        }
        Ok(Scene {
            prisms,
            arcs,
            meta,
            levels: levels.into_iter().map(|l| (l.1, l.2)).collect(),
            rooms,
        })
    }
}

#[cfg(test)]
pub(crate) mod tests;

/// An arc wall with a top profile (u is the length along the axis): the outline is split into sectors between radial
/// lines, with a planar top along the chord in each sector. A sector is at most 10° wide and does not cross a profile break.
fn arc_sectors(ax: &ddc_geom::arc::ArcAxis, top: &[(f64, f64)], ring: &[V2], base: f64) -> Vec<(Vec<V2>, Zfn)> {
    let len = ax.length();
    let mut us = vec![0.0, len];
    us.extend(top.iter().map(|p| p.0).filter(|u| *u > 1e-9 && *u < len - 1e-9));
    us.sort_by(f64::total_cmp);
    us.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
    let step = (10f64.to_radians() * ax.radius).max(1e-3);
    let mut cuts = vec![];
    for w in us.windows(2) {
        let n = ((w[1] - w[0]) / step).ceil().max(1.0) as usize;
        for k in 0..n {
            cuts.push((w[0] + (w[1] - w[0]) * k as f64 / n as f64, w[0] + (w[1] - w[0]) * (k + 1) as f64 / n as f64));
        }
    }
    let half = |pts: Vec<(f64, f64)>, s: f64, fwd: bool| -> Vec<(f64, f64)> {
        let t = ax.tangent_at(s);
        let d = if fwd { (t.x, t.y) } else { (-t.x, -t.y) };
        crate::views::clip(&pts, (ax.center.x, ax.center.y), d)
    };
    let mut out = vec![];
    for (s0, s1) in cuts {
        let mut pts: Vec<(f64, f64)> = ring.iter().map(|p| (p.x, p.y)).collect();
        if s0 > 1e-9 {
            pts = half(pts, s0, true);
        }
        if s1 < len - 1e-9 {
            pts = half(pts, s1, false);
        }
        if pts.len() < 3 {
            continue;
        }
        let (p0, p1) = (ax.point_at(s0), ax.point_at(s1));
        let c = (p1 - p0).len();
        if c < 1e-9 {
            continue;
        }
        let eps = 1e-7;
        let (h0, h1) = (ddc_geom::top::height_at(top, s0 + eps), ddc_geom::top::height_at(top, s1 - eps));
        let z = Zfn { o: p0, z: base, grad: v2(0.0, 0.0), dir: (p1 - p0) * (1.0 / c), prof: vec![(0.0, h0), (c, h1)] };
        out.push((pts.into_iter().map(|p| v2(p.0, p.1)).collect(), z));
    }
    out
}
