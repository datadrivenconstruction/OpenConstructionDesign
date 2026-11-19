// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Bodies on the active level picked and edited as one element each: imported meshes without a
//! parametric model (`mesh` rows, "Imported <category>") and native columns, beams and
//! ceilings. MEP runs are not bodies: `mep3d` draws them and selects them as `Sel::Mep`. Plan outline (convex hull of the projection) and z range for picking and drawing.

use std::collections::{BTreeMap, HashMap, HashSet};
use std::sync::Arc;

use ddc_geom::mesh::Mesh;
use ddc_geom::{v2, V2};
use ddc_model::{Model, Target};

/// Table an imported or native body lives in.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum BodyKind {
    Mesh,
    Column,
    Beam,
    Ceiling,
}

impl BodyKind {
    /// Inverse of [`Self::table`] (saved selections, palette keys).
    pub fn from_table(t: &str) -> Option<BodyKind> {
        Some(match t {
            "mesh" => BodyKind::Mesh,
            "col" => BodyKind::Column,
            "beam" => BodyKind::Beam,
            "ceiling" => BodyKind::Ceiling,
            _ => return None,
        })
    }

    pub fn table(self) -> &'static str {
        match self {
            BodyKind::Mesh => "mesh",
            BodyKind::Column => "col",
            BodyKind::Beam => "beam",
            BodyKind::Ceiling => "ceiling",
        }
    }

    pub fn target(self, id: i64) -> Target {
        match self {
            BodyKind::Mesh => Target::Mesh(id),
            BodyKind::Column => Target::Column(id),
            BodyKind::Beam => Target::Beam(id),
            BodyKind::Ceiling => Target::Ceiling(id),
        }
    }
}

#[derive(Clone, Debug)]
pub struct BodyItem {
    pub kind: BodyKind,
    pub id: i64,
    pub category: String,
    pub name: String,
    /// Convex hull of the plan projection, counter-clockwise.
    pub hull: Vec<V2>,
    pub zmin: f64,
    pub zmax: f64,
    /// Body in world metres, decoded once per change of its row.
    pub mesh: Option<Arc<Mesh>>,
}

/// `need_tris`: native bodies without triangles are dropped; an imported mesh row keeps its
/// outline (still pickable and deletable).
fn item(
    kind: BodyKind,
    id: i64,
    category: String,
    name: String,
    m: Mesh,
    need_tris: bool,
) -> Option<BodyItem> {
    let (mut zmin, mut zmax) = (f64::MAX, f64::MIN);
    let pts: Vec<V2> = m
        .positions
        .iter()
        .filter(|p| p.x.is_finite() && p.y.is_finite())
        .map(|p| {
            zmin = zmin.min(p.z);
            zmax = zmax.max(p.z);
            v2(p.x, p.y)
        })
        .collect();
    let hull = hull(pts);
    (!hull.is_empty() && (!need_tris || !m.triangles.is_empty())).then(|| BodyItem {
        kind,
        id,
        category,
        name,
        hull,
        zmin,
        zmax,
        mesh: Some(Arc::new(m)),
    })
}

/// id → (level, category, name).
fn names(m: &Model, sql: &str) -> HashMap<i64, (i64, String, String)> {
    let Ok(mut st) = m.sql().prepare(sql) else {
        return Default::default();
    };
    st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            (
                r.get::<_, Option<i64>>(1)?.unwrap_or_default(),
                r.get::<_, Option<String>>(2)?.unwrap_or_default(),
                r.get::<_, Option<String>>(3)?.unwrap_or_default(),
            ),
        ))
    })
    .map(|rows| rows.flatten().collect())
    .unwrap_or_default()
}

/// Tables whose writes change a body, and the id column a write is recorded by.
const WATCH: [(&str, &str); 12] = [
    ("mesh", "id"),
    ("col", "id"),
    ("beam", "id"),
    ("member_type", "id"),
    ("mep_run", "id"),
    ("mep_system", "id"),
    ("ceiling", "id"),
    ("ceiling_vertex", "ceiling"),
    ("slab_type", "id"),
    ("slab_type_layer", "stype"),
    ("shaft", "id"),
    ("level", "id"),
];

/// Bodies of all levels. Temp triggers (on this connection only, never in the file) record every
/// row written since the last refresh: meshes and ceilings are rebuilt per written id, members
/// and MEP runs (joined, networked) once per change of their tables; edits elsewhere and
/// `scene_all`'s per-level reloads rebuild nothing.
#[derive(Default)]
pub struct BodyCache {
    ready: bool,
    meshes: BTreeMap<i64, (Option<i64>, BodyItem)>,
    ceilings: BTreeMap<i64, (i64, BodyItem)>,
    members: Vec<(i64, BodyItem)>,
    /// Level of meshes without a known level (the lowest one) and the known level ids.
    lowest: Option<i64>,
    levels: HashSet<i64>,
    /// Rebuilds so far: (full, members, unused) — tests; runs are not bodies.
    pub builds: (u32, u32, u32),
    /// Properties of the shown body, read once per (kind, id, editor revision).
    props: Option<((BodyKind, i64, u64), Vec<Prop>)>,
}

/// (key, value, unit).
pub type Prop = (String, rusqlite::types::Value, String);

const TRIGGER_PROBE: &str = "ddc_body_mesh_insert";

/// Creates the dirty table and triggers on a connection that has none; true when created.
fn install(m: &Model) -> rusqlite::Result<bool> {
    let n: i64 = m.sql().query_row(
        "SELECT count(*) FROM sqlite_temp_master WHERE type='trigger' AND name=?1",
        [TRIGGER_PROBE],
        |r| r.get(0),
    )?;
    if n > 0 {
        return Ok(false);
    }
    let mut sql = String::from(
        "CREATE TEMP TABLE IF NOT EXISTS ddc_body_dirty(tbl TEXT NOT NULL, id INTEGER);",
    );
    for (t, c) in WATCH {
        for (op, row) in [("insert", "NEW"), ("update", "NEW"), ("delete", "OLD")] {
            sql += &format!(
                "CREATE TEMP TRIGGER IF NOT EXISTS ddc_body_{t}_{op} AFTER {op} ON main.{t} \
                 BEGIN INSERT INTO ddc_body_dirty(tbl, id) VALUES ('{t}', {row}.{c}); END;"
            );
        }
        // an update may change the key itself: the old id is stale too
        sql += &format!(
            "CREATE TEMP TRIGGER IF NOT EXISTS ddc_body_{t}_rekey AFTER UPDATE OF {c} ON main.{t} \
             BEGIN INSERT INTO ddc_body_dirty(tbl, id) VALUES ('{t}', OLD.{c}); END;"
        );
    }
    m.sql().execute_batch(&sql)?;
    Ok(true)
}

impl BodyCache {
    /// Bodies of `level`: imported meshes (rows without a known level go to the lowest level),
    /// then native columns, beams, MEP runs and ceilings by (kind, id).
    pub fn level(&mut self, m: &Model, level: i64) -> Vec<BodyItem> {
        self.refresh(m);
        let mut out: Vec<BodyItem> = self
            .meshes
            .values()
            .filter(|(lv, _)| match lv {
                Some(l) if self.levels.contains(l) => *l == level,
                _ => self.lowest == Some(level),
            })
            .map(|(_, b)| b.clone())
            .collect();
        let mut native: Vec<BodyItem> = self
            .members
            .iter()
            .chain(self.ceilings.values())
            .filter(|(l, _)| *l == level)
            .map(|(_, b)| b.clone())
            .collect();
        native.sort_by_key(|b| (b.kind, b.id));
        out.extend(native);
        out
    }

    /// Columns placed on another level that span `elevation` (a multi-storey column seen on
    /// the plans of the storeys it passes). Call after `level`, which refreshes the cache.
    pub fn through(&self, level: i64, elevation: f64) -> Vec<BodyItem> {
        const EPS: f64 = 1e-6;
        let mut out: Vec<BodyItem> = self
            .members
            .iter()
            .filter(|(l, b)| {
                *l != level
                    && b.kind == BodyKind::Column
                    && b.zmin < elevation - EPS
                    && b.zmax > elevation + EPS
            })
            .map(|(_, b)| b.clone())
            .collect();
        out.sort_by_key(|b| b.id);
        out
    }

    /// Properties of a body for the panel: one query per selection or model change, not per frame.
    pub fn props(&mut self, m: &Model, kind: BodyKind, id: i64, rev: u64) -> &[Prop] {
        let key = (kind, id, rev);
        if self.props.as_ref().map(|p| p.0) != Some(key) {
            let v = m.props(kind.table(), id).unwrap_or_default();
            self.props = Some((key, v));
        }
        self.props
            .as_ref()
            .map(|p| p.1.as_slice())
            .unwrap_or_default()
    }

    fn refresh(&mut self, m: &Model) {
        let fresh = install(m).unwrap_or(true);
        let dirty: Vec<(String, Option<i64>)> = m
            .sql()
            .prepare("SELECT DISTINCT tbl, id FROM ddc_body_dirty")
            .and_then(|mut st| {
                let v = st
                    .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                    .collect::<rusqlite::Result<Vec<_>>>();
                v
            })
            .unwrap_or_default();
        if !dirty.is_empty() {
            let _ = m.sql().execute("DELETE FROM ddc_body_dirty", []);
        }
        let any = |ts: &[&str]| dirty.iter().any(|(t, _)| ts.contains(&t.as_str()));
        let ids = |ts: &[&str]| -> Vec<i64> {
            let mut v: Vec<i64> = dirty
                .iter()
                .filter(|(t, _)| ts.contains(&t.as_str()))
                .filter_map(|(_, id)| *id)
                .collect();
            v.sort_unstable();
            v.dedup();
            v
        };
        let full = fresh || !self.ready || any(&["level"]);
        if full {
            self.builds.0 += 1;
            self.ready = true;
            let ls = m.level_list().unwrap_or_default();
            self.lowest = ls.first().map(|l| l.0);
            self.levels = ls.iter().map(|l| l.0).collect();
            self.meshes = m
                .imported_meshes(None)
                .unwrap_or_default()
                .into_iter()
                .filter_map(|x| {
                    let (id, lv) = (x.id, x.level);
                    item(BodyKind::Mesh, id, x.category, x.name, x.mesh, false)
                        .map(|b| (id, (lv, b)))
                })
                .collect();
        } else {
            for id in ids(&["mesh"]) {
                self.meshes.remove(&id);
                if let Ok(x) = m.imported_mesh(id) {
                    let lv = x.level;
                    if let Some(b) = item(BodyKind::Mesh, id, x.category, x.name, x.mesh, false) {
                        self.meshes.insert(id, (lv, b));
                    }
                }
            }
        }
        if full || any(&["col", "beam", "member_type"]) {
            self.builds.1 += 1;
            self.members = members(m);
        }
        if full || any(&["slab_type", "slab_type_layer", "shaft"]) {
            self.ceilings = ceilings(m, None);
        } else {
            let ids = ids(&["ceiling", "ceiling_vertex"]);
            if !ids.is_empty() {
                for id in &ids {
                    self.ceilings.remove(id);
                }
                self.ceilings.extend(ceilings(m, Some(ids.as_slice())));
            }
        }
    }
}

/// Columns and beams of all levels after joins (one `build_all`).
fn members(m: &Model) -> Vec<(i64, BodyItem)> {
    let cols = names(
        m,
        "SELECT c.id, c.level, CASE WHEN c.structural THEN 'structural columns' ELSE 'columns' END, \
         t.name FROM col c LEFT JOIN member_type t ON t.id=c.mtype",
    );
    let beams = names(
        m,
        "SELECT b.id, b.level, 'structural framing', t.name \
         FROM beam b LEFT JOIN member_type t ON t.id=b.mtype",
    );
    let mut out = vec![];
    for (kind, id, lv, mesh) in m.member_meshes().unwrap_or_default() {
        let (k, nm) = if kind == "column" {
            (BodyKind::Column, cols.get(&id))
        } else {
            (BodyKind::Beam, beams.get(&id))
        };
        let (_, c, n) = nm.cloned().unwrap_or_default();
        out.extend(item(k, id, c, n, mesh, true).map(|b| (lv, b)));
    }
    out
}

/// Ceilings (all, or the given ids) with their bodies.
fn ceilings(m: &Model, only: Option<&[i64]>) -> BTreeMap<i64, (i64, BodyItem)> {
    let filter = only.map_or(String::new(), |ids| {
        let v: Vec<String> = ids.iter().map(|i| i.to_string()).collect();
        format!(" WHERE c.id IN ({})", v.join(","))
    });
    let rows = names(
        m,
        &format!(
            "SELECT c.id, c.level, 'ceilings', t.name FROM ceiling c \
             LEFT JOIN slab_type t ON t.id=c.stype{filter}"
        ),
    );
    rows.into_iter()
        .filter_map(|(id, (lv, c, n))| {
            let mesh = m.ceiling_mesh(id).ok()?;
            item(BodyKind::Ceiling, id, c, n, mesh, true).map(|b| (id, (lv, b)))
        })
        .collect()
}

/// True when `p` lies in the plan projection of one of the body's triangles: the convex hull
/// alone would pick the empty corner of an L-shaped run.
pub fn covers(b: &BodyItem, p: V2) -> bool {
    let Some(m) = &b.mesh else { return true };
    if m.triangles.is_empty() {
        return true;
    }
    let e = 1e-9;
    m.triangles.iter().any(|t| {
        let [a, b, c] = t.map(|i| {
            let q = m.positions[i as usize];
            v2(q.x, q.y)
        });
        let d = |u: V2, v: V2| (v.x - u.x) * (p.y - u.y) - (v.y - u.y) * (p.x - u.x);
        let (d0, d1, d2) = (d(a, b), d(b, c), d(c, a));
        let area = (b.x - a.x) * (c.y - a.y) - (b.y - a.y) * (c.x - a.x);
        area.abs() > e && ((d0 >= -e && d1 >= -e && d2 >= -e) || (d0 <= e && d1 <= e && d2 <= e))
    })
}

/// Selection-filter category of a body kind (`selcat.<key>`).
pub fn category_key(k: BodyKind) -> &'static str {
    match k {
        BodyKind::Mesh => "imported",
        BodyKind::Column => "column",
        BodyKind::Beam => "beam",
        BodyKind::Ceiling => "ceiling",
    }
}

/// Site context (terrain, pads, masses): drawn as outline only and left out of zoom-to-fit.
pub fn is_site(category: &str) -> bool {
    matches!(
        category.to_ascii_lowercase().as_str(),
        "topography"
            | "toposolid"
            | "building pads"
            | "mass"
            | "mass floor"
            | "site"
            | "planting"
            | "entourage"
            | "parking"
    )
}

/// Monotone chain; degenerate input (a line or a point) gives a thin box so it stays pickable.
pub fn hull(mut p: Vec<V2>) -> Vec<V2> {
    p.sort_by(|a, b| a.x.total_cmp(&b.x).then(a.y.total_cmp(&b.y)));
    p.dedup_by(|a, b| (a.x - b.x).abs() < 1e-9 && (a.y - b.y).abs() < 1e-9);
    if p.is_empty() {
        return p;
    }
    let cross = |o: V2, a: V2, b: V2| (a.x - o.x) * (b.y - o.y) - (a.y - o.y) * (b.x - o.x);
    let mut h: Vec<V2> = Vec::with_capacity(p.len() * 2);
    for pass in 0..2 {
        let start = h.len();
        let it: Box<dyn Iterator<Item = &V2>> = if pass == 0 {
            Box::new(p.iter())
        } else {
            Box::new(p.iter().rev())
        };
        for &q in it {
            while h.len() >= start + 2 && cross(h[h.len() - 2], h[h.len() - 1], q) <= 1e-12 {
                h.pop();
            }
            h.push(q);
        }
        h.pop();
    }
    if h.len() < 3 {
        let (lo, hi) = p.iter().fold((p[0], p[0]), |(lo, hi), q| {
            (
                v2(lo.x.min(q.x), lo.y.min(q.y)),
                v2(hi.x.max(q.x), hi.y.max(q.y)),
            )
        });
        let e = 0.01;
        return vec![
            v2(lo.x - e, lo.y - e),
            v2(hi.x + e, lo.y - e),
            v2(hi.x + e, hi.y + e),
            v2(lo.x - e, hi.y + e),
        ];
    }
    h
}

pub fn inside_hull(p: V2, h: &[V2]) -> bool {
    let n = h.len();
    n >= 3
        && (0..n).all(|i| {
            let (a, b) = (h[i], h[(i + 1) % n]);
            (b.x - a.x) * (p.y - a.y) - (b.y - a.y) * (p.x - a.x) >= -1e-9
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::editor::{Editor, OpFn, Sel};

    fn cube(m: &Model, level: i64, x: f64, cat: &str) -> i64 {
        let mut ps = vec![];
        for k in 0..8 {
            ps.extend([
                x + (k & 1) as f64,
                ((k >> 1) & 1) as f64,
                ((k >> 2) & 1) as f64,
            ]);
        }
        let pb: Vec<u8> = ps.iter().flat_map(|v| v.to_le_bytes()).collect();
        let tris: [[u32; 3]; 12] = [
            [0, 2, 1],
            [1, 2, 3],
            [4, 5, 6],
            [5, 7, 6],
            [0, 1, 4],
            [1, 5, 4],
            [2, 6, 3],
            [3, 6, 7],
            [0, 4, 2],
            [2, 4, 6],
            [1, 3, 5],
            [3, 7, 5],
        ];
        let ib: Vec<u8> = tris
            .iter()
            .flatten()
            .flat_map(|i| i.to_le_bytes())
            .collect();
        m.sql()
            .execute(
                "INSERT INTO mesh(level, category, name, positions, indices) VALUES (?1,?2,'x',?3,?4)",
                rusqlite::params![level, cat, pb, ib],
            )
            .unwrap();
        m.sql().last_insert_rowid()
    }

    /// Every imported body is selectable in plan and 3D and moves with one undo step.
    #[test]
    fn every_imported_body_is_selectable_and_movable_with_undo() {
        let mut e = Editor::new_in_memory().unwrap();
        let ids: Vec<i64> = ["furniture", "generic models", "topography"]
            .iter()
            .enumerate()
            .map(|(i, c)| cube(&e.model, e.level, i as f64 * 3.0, c))
            .collect();
        e.reload().unwrap();
        assert_eq!(e.bodies.len(), 3);
        let sc = e.scene();
        for (i, &id) in ids.iter().enumerate() {
            e.pick(v2(i as f64 * 3.0 + 0.5, 0.5));
            assert_eq!(e.selection, vec![Sel::Body(BodyKind::Mesh, id)]);
            let (a, _) = e.element_range(Sel::Body(BodyKind::Mesh, id)).unwrap();
            assert_eq!(e.sel_of_element(a), Some(Sel::Body(BodyKind::Mesh, id)));
            let site = is_site(&e.bodies[i].category);
            let el = if site { super::super::levels::SITE } else { a };
            assert!(sc.meshes.iter().any(|m| m.element == el));
            let before = e.bodies[i].hull.clone();
            let ts = vec![ddc_model::Target::Mesh(id)];
            e.run_op(OpFn(std::sync::Arc::new(move |m: &mut Model| {
                m.move_elements(&ts, (1.0, 2.0))
            })))
            .unwrap();
            let moved = &e.bodies.iter().find(|b| b.id == id).unwrap().hull;
            assert!((moved[0].x - before[0].x - 1.0).abs() < 1e-9);
            assert!((moved[0].y - before[0].y - 2.0).abs() < 1e-9);
            e.undo();
            assert_eq!(e.bodies.iter().find(|b| b.id == id).unwrap().hull, before);
        }
        // topography stays out of zoom-to-fit
        let f = e.scene_all().focus.unwrap();
        assert!(f.max.x < 6.5, "{f:?}");
        e.selection = vec![
            Sel::Body(BodyKind::Mesh, ids[0]),
            Sel::Body(BodyKind::Mesh, ids[1]),
        ];
        e.delete_selection();
        assert_eq!(e.bodies.len(), 1);
        e.undo();
        assert_eq!(e.bodies.len(), 3);
    }

    /// Native columns, beams and ceilings are bodies too: drawn, picked in 3D, moved with one undo
    /// step and deleted together. MEP runs stay `Sel::Mep` (mep3d), never a second body.
    #[test]
    fn native_imported_rows_are_selectable_and_editable() {
        let mut e = Editor::new_in_memory().unwrap();
        let l = e.level;
        e.model
            .sql()
            .execute_batch(&format!(
                "INSERT INTO member_type(name,shape,material) VALUES ('R','rect 0.3 0.5','x');
                 INSERT INTO col(level,mtype,structural,x,y,base_offset,top_offset,rotation)
                     VALUES ({l},1,1,10.0,1.0,0.0,3.0,0.0);
                 INSERT INTO beam(level,mtype,ax,ay,az,bx,by,bz,bulge,yjust,zjust,rotation,
                     start_ext,end_ext,join_start,join_end)
                     VALUES ({l},1,12,0,3,16,0,3,0,0,0,0,0,0,'auto','auto');
                 INSERT INTO mep_system(name,kind,color) VALUES ('S','cold_water',255);
                 INSERT INTO mep_run(system,level,domain,shape,w,h,wall,insulation,lining,slope,
                     points,cap0,cap1)
                     VALUES (1,{l},'pipe','round',0.05,0.05,0,0,0,0,'20 2 2.5;24 2 2.5',0,0);
                 INSERT INTO slab_type(name) VALUES ('C');
                 INSERT INTO slab_type_layer(stype,pos,material,thickness) VALUES (1,0,'x',0.02);
                 INSERT INTO ceiling(level,stype,height) VALUES ({l},1,2.6);
                 INSERT INTO ceiling_vertex(ceiling,ring,idx,x,y)
                     VALUES (1,0,0,30,0),(1,0,1,34,0),(1,0,2,34,3),(1,0,3,30,3);"
            ))
            .unwrap();
        e.reload().unwrap();
        let kinds: Vec<BodyKind> = e.bodies.iter().map(|b| b.kind).collect();
        assert_eq!(kinds, [BodyKind::Column, BodyKind::Beam, BodyKind::Ceiling]);
        let sc = e.scene();
        let sels: Vec<Sel> = e.bodies.iter().map(|b| Sel::Body(b.kind, b.id)).collect();
        // the selection filter counts every native kind
        let cats: Vec<&str> = crate::selection::counts(&e, &sels)
            .iter()
            .map(|c| c.0)
            .collect();
        assert_eq!(cats, ["column", "beam", "ceiling"]);
        for &s in &sels {
            let (a, _) = e.element_range(s).unwrap();
            assert_eq!(e.sel_of_element(a), Some(s));
            assert!(sc.meshes.iter().any(|m| m.element == a), "{s:?} not drawn");
        }
        e.pick(v2(10.0, 1.0));
        assert_eq!(e.selection, vec![sels[0]]);
        e.pick(v2(32.0, 1.5));
        assert_eq!(e.selection, vec![sels[2]]);
        let before: Vec<Vec<V2>> = e.bodies.iter().map(|b| b.hull.clone()).collect();
        let ts: Vec<ddc_model::Target> = e.bodies.iter().map(|b| b.kind.target(b.id)).collect();
        e.run_op(OpFn(std::sync::Arc::new(move |m: &mut Model| {
            m.move_elements(&ts, (1.0, 0.0))
        })))
        .unwrap();
        for (b, h) in e.bodies.iter().zip(&before) {
            let dx = b.hull.iter().map(|p| p.x).fold(f64::MIN, f64::max)
                - h.iter().map(|p| p.x).fold(f64::MIN, f64::max);
            assert!((dx - 1.0).abs() < 1e-6, "{:?} moved by {dx}", b.kind);
        }
        e.undo();
        let after: Vec<Vec<V2>> = e.bodies.iter().map(|b| b.hull.clone()).collect();
        assert_eq!(after, before);
        e.selection = sels.clone();
        e.delete_selection();
        assert!(e.bodies.is_empty());
        e.undo();
        assert_eq!(e.bodies.len(), 3);
    }

    /// Writes elsewhere and `scene_all` rebuild no bodies; a column edit rebuilds members once;
    /// a new mesh row is added without a full rebuild.
    #[test]
    fn body_cache_rebuilds_only_written_tables() {
        let mut e = Editor::new_in_memory().unwrap();
        let l = e.level;
        e.model.add_level("Floor 2", 3.0).unwrap();
        e.model
            .sql()
            .execute_batch(&format!(
                "INSERT INTO member_type(name,shape,material) VALUES ('R','rect 0.3 0.5','x');
                 INSERT INTO col(level,mtype,structural,x,y,base_offset,top_offset,rotation)
                     VALUES ({l},1,1,10.0,1.0,0.0,3.0,0.0);"
            ))
            .unwrap();
        e.reload().unwrap();
        let b0 = e.body_cache.builds;
        let _ = e.scene_all();
        e.model
            .sql()
            .execute(
                "INSERT INTO prop(kind,ref,key,value,unit) VALUES ('col',1,'Mark','A','')",
                [],
            )
            .unwrap();
        e.reload().unwrap();
        assert_eq!(e.body_cache.builds, b0, "nothing body-related was written");
        e.model
            .sql()
            .execute("UPDATE col SET x=12.0 WHERE id=1", [])
            .unwrap();
        e.reload().unwrap();
        assert_eq!(e.body_cache.builds, (b0.0, b0.1 + 1, b0.2));
        let c = e
            .bodies
            .iter()
            .find(|b| b.kind == BodyKind::Column)
            .unwrap();
        assert!(inside_hull(v2(12.0, 1.0), &c.hull));
        let id = cube(&e.model, l, 0.0, "furniture");
        e.reload().unwrap();
        assert_eq!(e.body_cache.builds.0, b0.0);
        assert!(e
            .bodies
            .iter()
            .any(|b| b.kind == BodyKind::Mesh && b.id == id));
        e.model
            .sql()
            .execute("DELETE FROM mesh WHERE id=?1", [id])
            .unwrap();
        e.reload().unwrap();
        assert!(!e.bodies.iter().any(|b| b.kind == BodyKind::Mesh));
    }

    /// A run is never a body (no double draw/pick with `mep3d`); bodies are picked by their
    /// triangles, not the hull.
    #[test]
    fn mep_runs_are_not_bodies() {
        let mut e = Editor::new_in_memory().unwrap();
        let l = e.level;
        e.model
            .sql()
            .execute_batch(&format!(
                "INSERT INTO mep_system(name,kind,color) VALUES ('S','cold_water',255);
                 INSERT INTO mep_run(system,level,domain,shape,w,h,wall,insulation,lining,slope,
                     points,cap0,cap1)
                     VALUES (1,{l},'pipe','round',0.1,0.1,0,0,0,0,'0 0 2.5;4 0 2.5;4 4 2.5',0,0);"
            ))
            .unwrap();
        e.reload().unwrap();
        assert!(e.bodies.is_empty());
        assert_eq!(e.body_pick(v2(2.0, 0.0), false), None);
    }

    /// Terrain under a room never hides the room: rooms are picked first, terrain only where
    /// there is nothing else.
    #[test]
    fn site_body_does_not_occlude_rooms() {
        let mut e = Editor::new_in_memory().unwrap();
        let topo = cube(&e.model, e.level, 0.0, "topography");
        e.reload().unwrap();
        e.rooms.push(crate::editor::levels::RoomItem {
            id: 77,
            name: "R".into(),
            number: "1".into(),
            rings: vec![vec![
                v2(-1.0, -1.0),
                v2(2.0, -1.0),
                v2(2.0, 2.0),
                v2(-1.0, 2.0),
            ]],
        });
        e.pick(v2(0.5, 0.5));
        assert_eq!(e.selection, vec![Sel::Room(77)]);
        e.rooms.clear();
        e.pick(v2(0.5, 0.5));
        assert_eq!(e.selection, vec![Sel::Body(BodyKind::Mesh, topo)]);
    }

    #[test]
    fn hull_of_square_with_inner_points() {
        let h = hull(vec![
            v2(0.0, 0.0),
            v2(1.0, 0.0),
            v2(0.5, 0.5),
            v2(1.0, 1.0),
            v2(0.0, 1.0),
        ]);
        assert_eq!(h.len(), 4);
        assert!(inside_hull(v2(0.5, 0.2), &h));
        assert!(!inside_hull(v2(1.5, 0.2), &h));
    }
}
