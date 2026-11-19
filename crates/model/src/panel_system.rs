//! Curtain systems on freeform surfaces (geometry plan W7). Table `panel_system` keeps the
//! host freeform id, the layout (divisions, pattern, limits, planarize rounds; JSON) and the
//! mullion and glass sizes. Panels, mullions and quantities are derived from the host on every
//! read, so a host edit moves the system with it; deleting the host deletes its systems.
//! Panels whose centre falls outside the host trim are left out. Glass panels are closed
//! prisms of the panel polygon along its normal; mullions are rectangular prisms along every
//! kept grid edge, behind the glass (against the surface normal), rolled to the mean of the
//! surface normals at their ends.

use std::collections::BTreeSet;

use ddc_geom::mesh::Mesh;
use ddc_geom::panelize::{self, PanelKind, PanelLimits, Panels, Pattern, Rows, Spacing, MAX_DIV};
use ddc_geom::V3;
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value};

use crate::ops::Inv;
use crate::{Error, Model};

pub(crate) const DDL: &str = "
CREATE TABLE IF NOT EXISTS panel_system (
    id INTEGER PRIMARY KEY,
    host INTEGER NOT NULL,
    name TEXT NOT NULL DEFAULT '',
    material TEXT NOT NULL DEFAULT '',
    layout TEXT NOT NULL,
    mullion_w REAL NOT NULL DEFAULT 0,
    mullion_d REAL NOT NULL DEFAULT 0,
    glass REAL NOT NULL DEFAULT 0
);
CREATE INDEX IF NOT EXISTS panel_system_host ON panel_system(host);
";

/// Most panels in one system.
pub const MAX_PANELS: usize = 20_000;
/// Most planar-quad rounds.
pub const MAX_PLANARIZE: u32 = 2_000;
/// Largest mullion side and glass thickness, m.
pub const MAX_MULLION: f64 = 1.0;
pub const MAX_GLASS: f64 = 0.2;
/// Pull of planarized nodes back to the surface per round (0..1).
pub const STICK: f64 = 0.25;

#[derive(Clone, Debug, PartialEq)]
pub struct PanelLayout {
    pub cols: Spacing,
    /// `Rows::Levels` heights are z above the host level.
    pub rows: Rows,
    pub pattern: Pattern,
    pub limits: PanelLimits,
    pub planarize: u32,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelSystemSpec {
    /// Freeform surface id.
    pub host: i64,
    pub name: String,
    pub material: String,
    pub layout: PanelLayout,
    /// Mullion width and depth, m; zero is no mullions.
    pub mullion: [f64; 2],
    /// Glass thickness, m; zero draws the panels as their mid-surface.
    pub glass: f64,
}

/// Derived system in world coordinates.
#[derive(Clone, Debug)]
pub struct PanelSystemGeom {
    pub panels: Panels,
    /// Panel indices inside the host trim.
    pub kept: Vec<usize>,
    /// Grid edges carrying a mullion (node index pairs, smaller first).
    pub edges: Vec<[usize; 2]>,
    /// One mesh per kept panel (same order as `kept`) and per edge.
    pub glass: Vec<Mesh>,
    pub frames: Vec<Mesh>,
    /// Largest planarity before and after the planar-quad rounds, m.
    pub planarity: (f64, f64),
    /// Largest node distance from the host after planarizing, m.
    pub off_surface: f64,
    /// `what: measure` lines.
    pub loss: Vec<String>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PanelQty {
    pub panels: usize,
    pub flat: usize,
    pub cold_bent: usize,
    pub hot_bent: usize,
    /// Distinct panel shapes among the kept panels.
    pub types: usize,
    /// Glass area (panel triangles), m2, and volume, m3.
    pub area: f64,
    pub glass_volume: f64,
    /// Mullion run, m, and volume (length x section; overlaps at nodes counted), m3.
    pub mullion_length: f64,
    pub mullion_volume: f64,
    pub max_planarity: f64,
    pub off_surface: f64,
}

fn bad<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

// ---------- JSON ----------

fn spacing_json(s: Spacing) -> Value {
    match s {
        Spacing::Params(n) => json!({"by": "params", "n": n}),
        Spacing::Equal(n) => json!({"by": "equal", "n": n}),
        Spacing::Width(w) => json!({"by": "width", "w": w}),
    }
}

fn spacing_of(v: &Value) -> Result<Spacing, Error> {
    let n = || {
        v["n"]
            .as_u64()
            .filter(|n| (1..=MAX_DIV as u64).contains(n))
            .map(|n| n as usize)
            .ok_or_else(|| Error::Invalid(format!("panel system: a division needs 1 to {MAX_DIV} panels")))
    };
    Ok(match v["by"].as_str() {
        Some("params") => Spacing::Params(n()?),
        Some("equal") => Spacing::Equal(n()?),
        Some("width") => Spacing::Width(v["w"].as_f64().ok_or_else(|| Error::Invalid("panel system: width".into()))?),
        _ => return bad(format!("panel system: unknown division {v}")),
    })
}

fn pattern_key(p: Pattern) -> &'static str {
    match p {
        Pattern::Quad => "quad",
        Pattern::Triangle => "triangle",
        Pattern::Diamond => "diamond",
    }
}

pub fn layout_json(l: &PanelLayout) -> Value {
    let rows = match &l.rows {
        Rows::Spacing(s) => spacing_json(*s),
        Rows::Levels(z) => json!({"by": "levels", "z": z}),
    };
    json!({
        "cols": spacing_json(l.cols),
        "rows": rows,
        "pattern": pattern_key(l.pattern),
        "flat": l.limits.flat,
        "cold": l.limits.cold,
        "cluster": l.limits.cluster,
        "planarize": l.planarize,
    })
}

pub fn layout_of(v: &Value) -> Result<PanelLayout, Error> {
    let rows = match v["rows"]["by"].as_str() {
        Some("levels") => Rows::Levels(
            v["rows"]["z"]
                .as_array()
                .ok_or_else(|| Error::Invalid("panel system: levels must be an array".into()))?
                .iter()
                .map(|x| x.as_f64().ok_or_else(|| Error::Invalid("panel system: bad level".into())))
                .collect::<Result<_, _>>()?,
        ),
        _ => Rows::Spacing(spacing_of(&v["rows"])?),
    };
    let pattern = match v["pattern"].as_str() {
        Some("quad") => Pattern::Quad,
        Some("triangle") => Pattern::Triangle,
        Some("diamond") => Pattern::Diamond,
        p => return bad(format!("panel system: pattern {p:?}")),
    };
    let num = |k: &str| v[k].as_f64().ok_or_else(|| Error::Invalid(format!("panel system: {k} missing")));
    Ok(PanelLayout {
        cols: spacing_of(&v["cols"])?,
        rows,
        pattern,
        limits: PanelLimits { flat: num("flat")?, cold: num("cold")?, cluster: num("cluster")? },
        planarize: v["planarize"].as_u64().unwrap_or(0).min(u32::MAX as u64) as u32,
    })
}

// ---------- storage ----------

fn host_level(db: &Connection, host: i64) -> Result<(i64, f64), Error> {
    db.query_row(
        "SELECT f.level, l.elevation FROM freeform f JOIN level l ON l.id=f.level WHERE f.id=?1",
        [host],
        |r| Ok((r.get(0)?, r.get(1)?)),
    )
    .optional()?
    .ok_or_else(|| Error::Invalid(format!("panel system: no freeform {host}")))
}

/// Refuses what cannot be built: unknown host, bad sizes or limits, a layout the surface
/// cannot carry, more than [`MAX_PANELS`] panels.
pub(crate) fn check(db: &Connection, s: &PanelSystemSpec) -> Result<(), Error> {
    if s.name.len() > 200 || s.material.len() > 200 {
        return bad("panel system: name or material too long");
    }
    let l = &s.layout;
    let lim = l.limits;
    let pos = |x: f64| x.is_finite() && x > 0.0;
    if !(pos(lim.flat) && pos(lim.cold) && pos(lim.cluster) && lim.flat <= lim.cold) {
        return bad("panel system: limits must be > 0 with flat <= cold");
    }
    if l.planarize > MAX_PLANARIZE {
        return bad(format!("panel system: planarize is at most {MAX_PLANARIZE} rounds"));
    }
    let [w, d] = s.mullion;
    let side_ok = |x: f64| x.is_finite() && (0.0..=MAX_MULLION).contains(&x);
    if !(side_ok(w) && side_ok(d) && ((w == 0.0) == (d == 0.0))) {
        return bad(format!("panel system: mullion sides are both 0 or both in (0, {MAX_MULLION}] m"));
    }
    if !(s.glass.is_finite() && (0.0..=MAX_GLASS).contains(&s.glass)) {
        return bad(format!("panel system: glass thickness is outside 0..{MAX_GLASS} m"));
    }
    let host = crate::freeform::read(db, s.host)?;
    let p = panelize::panelize(&host.surface, l.cols, &l.rows, l.pattern, l.limits)
        .map_err(|e| Error::Invalid(format!("panel system: {e}")))?;
    if p.panels.len() > MAX_PANELS {
        return bad(format!("panel system: {} panels is over {MAX_PANELS}", p.panels.len()));
    }
    Ok(())
}

fn write(db: &Connection, s: &PanelSystemSpec, id: Option<i64>) -> Result<i64, Error> {
    let args = params![
        id,
        s.host,
        s.name,
        s.material,
        layout_json(&s.layout).to_string(),
        s.mullion[0],
        s.mullion[1],
        s.glass
    ];
    if id.is_some() {
        db.execute(
            "UPDATE panel_system SET host=?2,name=?3,material=?4,layout=?5,mullion_w=?6,mullion_d=?7,\
             glass=?8 WHERE id=?1",
            args,
        )?;
        Ok(id.unwrap_or_default())
    } else {
        db.execute(
            "INSERT INTO panel_system(id,host,name,material,layout,mullion_w,mullion_d,glass) \
             VALUES (?1,?2,?3,?4,?5,?6,?7,?8)",
            args,
        )?;
        Ok(db.last_insert_rowid())
    }
}

pub(crate) fn read(db: &Connection, id: i64) -> Result<PanelSystemSpec, Error> {
    let row = db
        .query_row(
            "SELECT host,name,material,layout,mullion_w,mullion_d,glass FROM panel_system WHERE id=?1",
            [id],
            |r| {
                Ok((
                    (r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?),
                    (r.get::<_, f64>(4)?, r.get::<_, f64>(5)?, r.get::<_, f64>(6)?),
                ))
            },
        )
        .optional()?
        .ok_or_else(|| Error::Invalid(format!("no panel system {id}")))?;
    let ((host, name, material, layout), (mw, md, glass)) = row;
    Ok(PanelSystemSpec {
        host,
        name,
        material,
        layout: layout_of(&serde_json::from_str(&layout)?)?,
        mullion: [mw, md],
        glass,
    })
}

/// Rows deleted with host `host` and the inverse that puts them back.
pub(crate) fn drop_hosted(db: &Connection, host: i64, inv: Inv) -> Result<Inv, Error> {
    let inv = inv.restore(db, "panel_system", "host", host)?;
    db.execute("DELETE FROM panel_system WHERE host=?1", [host])?;
    Ok(inv)
}

// ---------- geometry ----------

fn unit(v: V3) -> Option<V3> {
    let l = v.len();
    (l.is_finite() && l > 1e-15).then(|| v * (1.0 / l))
}

/// Newell normal of a polygon (length = twice its projected area).
fn newell(p: &[V3]) -> V3 {
    let mut n = V3::default();
    for k in 0..p.len() {
        n = n + p[k].cross(p[(k + 1) % p.len()]);
    }
    n
}

/// Closed prism of polygon `base` swept by `off`, outward triangles.
fn prism(base: &[V3], off: V3) -> Mesh {
    let k = base.len();
    let base: Vec<V3> = if newell(base).dot(off) > 0.0 {
        base.iter().rev().copied().collect()
    } else {
        base.to_vec()
    };
    let mut positions = base.clone();
    positions.extend(base.iter().map(|p| *p + off));
    let mut triangles = vec![];
    let ku = k as u32;
    for i in 1..ku - 1 {
        triangles.push([0, i, i + 1]);
        triangles.push([ku, ku + i + 1, ku + i]);
    }
    for i in 0..ku {
        let j = (i + 1) % ku;
        triangles.push([i, j + ku, j]);
        triangles.push([i, i + ku, j + ku]);
    }
    let face_of = vec![0; triangles.len()];
    Mesh { positions, triangles, face_of }
}

/// Panels, edges and meshes of a spec on its host (`z0`: host level elevation).
pub fn geom_of(s: &PanelSystemSpec, host: &crate::FreeformSpec, z0: f64) -> Result<PanelSystemGeom, Error> {
    let sf = &host.surface;
    let l = &s.layout;
    let mut p = panelize::panelize(sf, l.cols, &l.rows, l.pattern, l.limits)
        .map_err(|e| Error::Invalid(format!("panel system: {e}")))?;
    let before = p.max_planarity();
    let mut loss = vec![];
    if l.planarize > 0 && p.pattern != Pattern::Triangle {
        p.planarize(sf, l.planarize as usize, STICK);
    }
    let off_surface = p.off_surface(sf);
    if off_surface > 0.0 {
        loss.push(format!("planar panels: nodes up to {:.1} mm off the surface", off_surface * 1000.0));
    }
    let (u0, u1) = sf.domain_u();
    let (v0, v1) = sf.domain_v();
    let kept: Vec<usize> = (0..p.panels.len())
        .filter(|&k| {
            let ns = &p.panels[k].nodes;
            let c = ns.iter().fold([0.0, 0.0], |a, &n| [a[0] + p.uv[n][0], a[1] + p.uv[n][1]]);
            let m = ns.len() as f64;
            host.trim.keeps([(c[0] / m - u0) / (u1 - u0), (c[1] / m - v0) / (v1 - v0)])
        })
        .collect();
    if kept.len() < p.panels.len() {
        loss.push(format!("trim: {} panels outside the host left out", p.panels.len() - kept.len()));
    }
    let normals: Vec<V3> = p
        .uv
        .iter()
        .map(|t| sf.normal(t[0], t[1]).unwrap_or(V3 { x: 0.0, y: 0.0, z: 1.0 }))
        .collect();
    for n in p.nodes.iter_mut() {
        n.z += z0;
    }
    let mut edges = BTreeSet::new();
    for &k in &kept {
        let ns = &p.panels[k].nodes;
        for i in 0..ns.len() {
            let (a, b) = (ns[i], ns[(i + 1) % ns.len()]);
            if a != b {
                edges.insert([a.min(b), a.max(b)]);
            }
        }
    }
    let edges: Vec<[usize; 2]> = if s.mullion[0] > 0.0 { edges.into_iter().collect() } else { vec![] };
    let glass = kept
        .iter()
        .map(|&k| {
            let poly: Vec<V3> = p.panels[k].nodes.iter().map(|&n| p.nodes[n]).collect();
            if s.glass > 0.0 {
                let n = unit(newell(&poly)).unwrap_or(V3 { x: 0.0, y: 0.0, z: 1.0 });
                let base: Vec<V3> = poly.iter().map(|q| *q - n * (0.5 * s.glass)).collect();
                prism(&base, n * s.glass)
            } else {
                let k = poly.len() as u32;
                let triangles: Vec<[u32; 3]> = (1..k - 1).map(|i| [0, i, i + 1]).collect();
                let face_of = vec![0; triangles.len()];
                Mesh { positions: poly, triangles, face_of }
            }
        })
        .collect();
    if s.glass == 0.0 {
        loss.push("glass drawn as its mid-surface (no thickness)".into());
    }
    let [w, d] = s.mullion;
    let frames = edges
        .iter()
        .filter_map(|&[a, b]| {
            let (pa, pb) = (p.nodes[a], p.nodes[b]);
            let dir = unit(pb - pa)?;
            let n = normals[a] + normals[b];
            let n = unit(n - dir * n.dot(dir))?;
            let side = dir.cross(n);
            let h = side * (0.5 * w);
            let back = n * (-0.5 * s.glass);
            let base = [pa + back - h, pa + back + h, pa + back + h - n * d, pa + back - h - n * d];
            Some(prism(&base, pb - pa))
        })
        .collect();
    Ok(PanelSystemGeom { panels: p, kept, edges, glass, frames, planarity: (before, 0.0), off_surface, loss }
        .with_after())
}

impl PanelSystemGeom {
    fn with_after(mut self) -> Self {
        self.planarity.1 = self.kept.iter().map(|&k| self.panels.panels[k].planarity).fold(0.0, f64::max);
        self
    }

    pub fn quantities(&self, s: &PanelSystemSpec) -> PanelQty {
        let ps = &self.panels.panels;
        let kind = |k: PanelKind| self.kept.iter().filter(|&&i| ps[i].kind == k).count();
        let types: BTreeSet<usize> = self.kept.iter().map(|&i| ps[i].cluster).collect();
        let area: f64 = self.kept.iter().map(|&i| ps[i].area).sum();
        let len: f64 = self.edges.iter().map(|&[a, b]| (self.panels.nodes[b] - self.panels.nodes[a]).len()).sum();
        PanelQty {
            panels: self.kept.len(),
            flat: kind(PanelKind::Flat),
            cold_bent: kind(PanelKind::ColdBent),
            hot_bent: kind(PanelKind::HotBent),
            types: types.len(),
            area,
            glass_volume: area * s.glass,
            mullion_length: len,
            mullion_volume: len * s.mullion[0] * s.mullion[1],
            max_planarity: self.planarity.1,
            off_surface: self.off_surface,
        }
    }
}

// ---------- API ----------

impl Model {
    pub fn add_panel_system(&mut self, s: &PanelSystemSpec) -> Result<i64, Error> {
        check(&self.db, s)?;
        self.op("panel_system_add", |db| {
            let id = write(db, s, None)?;
            Ok((id, id, Inv::default().delete("panel_system", "id", id)))
        })
    }

    pub fn update_panel_system(&mut self, id: i64, s: &PanelSystemSpec) -> Result<(), Error> {
        check(&self.db, s)?;
        self.op("panel_system_edit", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "panel_system", "id", id)?;
            write(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_panel_system(&mut self, id: i64) -> Result<(), Error> {
        self.op("panel_system_delete", |db| {
            read(db, id)?;
            let inv = Inv::default().restore(db, "panel_system", "id", id)?;
            db.execute("DELETE FROM panel_system WHERE id=?1", [id])?;
            Ok(((), id, inv))
        })
    }

    pub fn panel_system(&self, id: i64) -> Result<PanelSystemSpec, Error> {
        read(&self.db, id)
    }

    /// Systems whose host lies on `level` (or all); unreadable rows are skipped.
    pub fn panel_systems(&self, level: Option<i64>) -> Result<Vec<(i64, PanelSystemSpec)>, Error> {
        let mut st = self.db.prepare(
            "SELECT p.id FROM panel_system p JOIN freeform f ON f.id=p.host \
             WHERE (?1 IS NULL OR f.level=?1) ORDER BY p.id",
        )?;
        let ids: Vec<i64> = st.query_map([level], |r| r.get(0))?.collect::<Result<_, _>>()?;
        Ok(ids.into_iter().filter_map(|id| read(&self.db, id).ok().map(|s| (id, s))).collect())
    }

    /// Systems hosted by freeform `host`.
    pub fn panel_systems_on(&self, host: i64) -> Result<Vec<i64>, Error> {
        let mut st = self.db.prepare("SELECT id FROM panel_system WHERE host=?1 ORDER BY id")?;
        let r = st.query_map([host], |r| r.get(0))?;
        Ok(r.collect::<Result<_, _>>()?)
    }

    /// Panels, mullions and meshes in world coordinates.
    pub fn panel_system_geom(&self, id: i64) -> Result<PanelSystemGeom, Error> {
        let s = self.panel_system(id)?;
        let host = self.freeform(s.host)?;
        let (_, z0) = host_level(&self.db, s.host)?;
        geom_of(&s, &host, z0)
    }

    pub fn panel_system_quantities(&self, id: i64) -> Result<PanelQty, Error> {
        let s = self.panel_system(id)?;
        Ok(self.panel_system_geom(id)?.quantities(&s))
    }

    /// Level and elevation of the host of a system.
    pub fn panel_system_level(&self, id: i64) -> Result<(i64, f64), Error> {
        host_level(&self.db, self.panel_system(id)?.host)
    }
}

#[cfg(test)]
#[path = "panel_system_tests.rs"]
mod tests;
