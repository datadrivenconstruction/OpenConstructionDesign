// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The saved model as numbers the oracles measure: members with world ends, components, mep
//! runs, rooms, shafts. One read, then every oracle works on this snapshot.
//! A table the model does not have is absent from `tables`: an oracle that needs it fails with
//! "no capability" instead of passing on zero.

use ddc_model::Model;
use regex::Regex;
use rusqlite::{Connection, Row};
use std::collections::{HashMap, HashSet};

pub type P3 = [f64; 3];

#[derive(Clone, Debug)]
pub struct Member {
    pub kind: &'static str,
    pub a: P3,
    pub b: P3,
    pub mtype: Option<i64>,
    pub bulge: f64,
}

#[derive(Clone, Debug)]
pub struct Comp {
    pub id: i64,
    pub level: i64,
    pub at: P3,
    pub family: String,
    pub category: String,
}

#[derive(Clone, Debug)]
pub struct Run {
    pub id: i64,
    pub system: String,
    pub level: i64,
    pub domain: String,
    pub shape: String,
    pub w: f64,
    pub h: f64,
    pub dn: Option<i64>,
    pub slope: f64,
    pub pts: Vec<P3>,
    pub cap: [bool; 2],
}

#[derive(Clone, Debug)]
pub struct Room {
    pub id: i64,
    pub level: i64,
    pub name: String,
    pub at: [f64; 2],
}

#[derive(Clone, Debug)]
pub struct Shaft {
    pub level: i64,
    pub top: i64,
    pub bbox: Option<[f64; 4]>,
}

/// One swept solid read from its definition.
#[derive(Clone, Debug, PartialEq, Default)]
pub struct SweepFact {
    pub id: i64,
    /// Profile twist end minus start, radians.
    pub twist: f64,
    /// Largest profile size over the smallest along the path (1 = constant).
    pub scale: f64,
    /// Turning of the path about its own centre, radians (a helix angle, or the winding of the points).
    pub turn: f64,
    /// Height gained along the path, metres.
    pub rise: f64,
}

fn law_span(v: Option<&serde_json::Value>) -> (f64, f64) {
    match v {
        Some(x) if x.is_number() => (x.as_f64().unwrap_or(0.0), x.as_f64().unwrap_or(0.0)),
        Some(x) => (x.get("a").and_then(|a| a.as_f64()).unwrap_or(0.0), x.get("b").and_then(|a| a.as_f64()).unwrap_or(0.0)),
        None => (0.0, 0.0),
    }
}

fn ring_size(r: &serde_json::Value) -> f64 {
    let p: Vec<[f64; 2]> = r.as_array().map(|a| a.iter().filter_map(|q| Some([q.get(0)?.as_f64()?, q.get(1)?.as_f64()?])).collect()).unwrap_or_default();
    let n = p.len();
    ((0..n).map(|i| p[i][0] * p[(i + 1) % n][1] - p[(i + 1) % n][0] * p[i][1]).sum::<f64>() / 2.0).abs().sqrt()
}

/// What a sweep or swept-blend definition says about turning, scaling and rise; None when the
/// definition is another kind of solid.
/// Vertices of a mesh plus points about a metre apart up its tall edges, so a lofted shaft
/// with vertices only at its two ends still shows its radius at every height.
pub fn mesh_points(mesh: &ddc_geom::mesh::Mesh) -> Vec<P3> {
    let p = |i: u32| mesh.positions[i as usize];
    let mut out: Vec<P3> = mesh.positions.iter().map(|v| [v.x, v.y, v.z]).collect();
    for t in &mesh.triangles {
        for (a, b) in [(t[0], t[1]), (t[1], t[2]), (t[2], t[0])] {
            let (a, b) = (p(a), p(b));
            let n = ((b.z - a.z).abs().ceil() as usize).min(64);
            for k in 1..n {
                let s = k as f64 / n as f64;
                out.push([a.x + (b.x - a.x) * s, a.y + (b.y - a.y) * s, a.z + (b.z - a.z) * s]);
            }
        }
    }
    out
}

pub fn sweep_of(id: i64, def: &serde_json::Value) -> Option<SweepFact> {
    let kind = def.get("kind")?.as_str()?;
    if kind != "sweep" && kind != "swept_blend" {
        return None;
    }
    let pt = |v: &serde_json::Value| -> Option<P3> { Some([v.get(0)?.as_f64()?, v.get(1)?.as_f64()?, v.get(2)?.as_f64()?]) };
    let mut pts: Vec<P3> = def.get("path").and_then(|p| p.as_array()).map(|a| a.iter().filter_map(pt).collect()).unwrap_or_default();
    let mut s = SweepFact { id, scale: 1.0, ..Default::default() };
    let mut helix = 0.0;
    if let Some(sm) = def.get("smooth") {
        let (a, b) = law_span(sm.get("twist"));
        s.twist = (b - a).abs();
        let (a, b) = law_span(sm.get("scale"));
        if a.min(b) > 0.0 {
            s.scale = a.max(b) / a.min(b);
        }
        pts.extend(sm.get("start").and_then(pt));
        for seg in sm.get("segs").and_then(|x| x.as_array()).into_iter().flatten() {
            pts.extend(seg.get("via").and_then(|v| v.as_array()).into_iter().flatten().filter_map(pt));
            pts.extend(seg.get("to").and_then(pt));
            if seg.get("kind").and_then(|k| k.as_str()) == Some("helix") {
                helix += seg.get("angle").and_then(|a| a.as_f64()).unwrap_or(0.0).abs();
                s.rise += seg.get("rise").and_then(|a| a.as_f64()).unwrap_or(0.0).abs();
            }
        }
    }
    if kind == "swept_blend" {
        let (a, b) = (ring_size(def.get("bottom").unwrap_or(&serde_json::Value::Null)), ring_size(def.get("top").unwrap_or(&serde_json::Value::Null)));
        if a.min(b) > 0.0 {
            s.scale = a.max(b) / a.min(b);
        }
    }
    if pts.len() >= 3 {
        let n = pts.len() as f64;
        let (cx, cy) = (pts.iter().map(|p| p[0]).sum::<f64>() / n, pts.iter().map(|p| p[1]).sum::<f64>() / n);
        let mut wind = 0.0;
        for w in pts.windows(2) {
            let (a, b) = ((w[0][1] - cy).atan2(w[0][0] - cx), (w[1][1] - cy).atan2(w[1][0] - cx));
            let mut d = b - a;
            while d > std::f64::consts::PI {
                d -= 2.0 * std::f64::consts::PI;
            }
            while d < -std::f64::consts::PI {
                d += 2.0 * std::f64::consts::PI;
            }
            wind += d;
        }
        s.turn = wind.abs();
        let (lo, hi) = (pts.iter().map(|p| p[2]).fold(f64::MAX, f64::min), pts.iter().map(|p| p[2]).fold(f64::MIN, f64::max));
        s.rise = s.rise.max(hi - lo);
    }
    s.turn = s.turn.max(helix);
    Some(s)
}

#[derive(Clone, Debug, Default)]
pub struct Facts {
    pub tables: HashSet<String>,
    pub rows: HashMap<String, i64>,
    pub members: Vec<Member>,
    pub comps: Vec<Comp>,
    pub runs: Vec<Run>,
    pub rooms: Vec<Room>,
    pub shafts: Vec<Shaft>,
    pub level_z: HashMap<i64, f64>,
    pub slab_z: Vec<f64>,
    pub mtypes: HashMap<i64, (String, String, String)>,
    pub systems: Vec<String>,
    pub port_comps: HashSet<i64>,
    pub curved_walls: i64,
    /// Vault roofs: an extruded profile of four or more points that rises (an arch), not a flat or gable extrusion.
    pub curved_roofs: i64,
    /// Every wall as (type name, fire rating property or empty).
    pub walls: Vec<(String, String)>,
    /// Every point of the structure: member ends, shell nodes and solid box corners (no mep points).
    pub points: Vec<P3>,
    /// Gridshell nodes only.
    pub shell_pts: Vec<P3>,
    /// Walls and solids, one group of points per element: a wall's ends at its foot and head,
    /// a solid's mesh vertices (a masonry or cast tower has no member ends to measure).
    pub masses: Vec<Vec<P3>>,
    /// Swept solids: how far the profile turns and scales along the path.
    pub sweeps: Vec<SweepFact>,
    /// World position of every family connector (a port sits on the nozzle face, not at the
    /// origin of its component).
    pub ports: Vec<P3>,
    /// Connectors linked to a system: the system kind (as runs name it) and the world position.
    pub sys_ports: Vec<(String, P3)>,
    /// Footing positions (on their level) that members can stand on.
    pub bases: Vec<P3>,
}

pub(super) fn rows<T>(
    db: &Connection,
    sql: &str,
    f: impl FnMut(&Row) -> rusqlite::Result<T>,
) -> Result<Vec<T>, String> {
    let mut st = db.prepare(sql).map_err(|e| format!("{sql}: {e}"))?;
    let it = st.query_map([], f).map_err(|e| format!("{sql}: {e}"))?;
    it.collect::<Result<Vec<_>, _>>().map_err(|e| format!("{sql}: {e}"))
}

/// «x y z;x y z» as the model stores mep points.
pub fn parse_points(s: &str) -> Vec<P3> {
    s.split(';')
        .filter_map(|p| {
            let v: Vec<f64> = p.split_whitespace().filter_map(|n| n.parse().ok()).collect();
            (v.len() == 3).then(|| [v[0], v[1], v[2]])
        })
        .collect()
}

/// Case-insensitive pattern, with the error naming the pattern.
pub fn rx(pat: &str) -> Result<Regex, String> {
    Regex::new(&format!("(?i){pat}")).map_err(|e| format!("bad pattern {pat}: {e}"))
}

impl Facts {
    pub fn has(&self, table: &str) -> bool {
        self.tables.contains(table)
    }

    pub fn n(&self, table: &str) -> i64 {
        self.rows.get(table).copied().unwrap_or(0)
    }

    pub fn z(&self, level: i64, off: f64) -> f64 {
        self.level_z.get(&level).copied().unwrap_or(0.0) + off
    }

    pub fn read(m: &Model) -> Result<Facts, String> {
        let db = m.sql();
        let mut f = Facts::default();
        for t in rows(db, "SELECT name FROM sqlite_master WHERE type='table'", |r| r.get::<_, String>(0))? {
            let n: i64 = db
                .query_row(&format!("SELECT count(*) FROM \"{t}\""), [], |r| r.get(0))
                .unwrap_or(0);
            f.rows.insert(t.clone(), n);
            f.tables.insert(t);
        }
        f.level_z = rows(db, "SELECT id, elevation FROM level", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?
            .into_iter()
            .collect();
        if f.has("member_type") {
            for (id, n, s, mat) in rows(db, "SELECT id, name, shape, material FROM member_type", |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?, r.get::<_, String>(2)?, r.get::<_, String>(3)?))
            })? {
                f.mtypes.insert(id, (n, s, mat));
            }
        }
        if f.has("col") {
            let list = rows(
                db,
                // an inclined column ends at its top point (tx, ty)
                "SELECT level, top_level, mtype, x, y, base_offset, top_offset, COALESCE(tx, x), COALESCE(ty, y) FROM col",
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, Option<i64>>(1)?,
                        r.get::<_, i64>(2)?,
                        [r.get::<_, f64>(3)?, r.get::<_, f64>(4)?],
                        r.get::<_, f64>(5)?,
                        r.get::<_, f64>(6)?,
                        [r.get::<_, f64>(7)?, r.get::<_, f64>(8)?],
                    ))
                },
            )?;
            for (lv, tl, mt, at, bo, to, top) in list {
                let a = [at[0], at[1], f.z(lv, bo)];
                let b = [top[0], top[1], f.z(tl.unwrap_or(lv), to)];
                f.members.push(Member { kind: "col", a, b, mtype: Some(mt), bulge: 0.0 });
            }
        }
        for (table, bulge) in [("beam", "bulge"), ("brace", "0.0")] {
            if !f.has(table) {
                continue;
            }
            let sql = format!("SELECT level, mtype, ax, ay, az, bx, by, bz, {bulge} FROM {table}");
            let list = rows(db, &sql, |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    [r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?],
                    [r.get::<_, f64>(5)?, r.get::<_, f64>(6)?, r.get::<_, f64>(7)?],
                    r.get::<_, f64>(8)?,
                ))
            })?;
            let kind = if table == "beam" { "beam" } else { "brace" };
            for (lv, mt, a, b, bu) in list {
                f.members.push(Member {
                    kind,
                    a: [a[0], a[1], f.z(lv, a[2])],
                    b: [b[0], b[1], f.z(lv, b[2])],
                    mtype: Some(mt),
                    bulge: bu,
                });
            }
        }
        if f.has("truss") {
            let list = rows(db, "SELECT level, ax, ay, az, bx, by, bz FROM truss", |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    [r.get::<_, f64>(1)?, r.get::<_, f64>(2)?, r.get::<_, f64>(3)?],
                    [r.get::<_, f64>(4)?, r.get::<_, f64>(5)?, r.get::<_, f64>(6)?],
                ))
            })?;
            for (lv, a, b) in list {
                f.members.push(Member {
                    kind: "truss",
                    a: [a[0], a[1], f.z(lv, a[2])],
                    b: [b[0], b[1], f.z(lv, b[2])],
                    mtype: None,
                    bulge: 0.0,
                });
            }
        }
        if f.has("slab") {
            let list = rows(db, "SELECT level, offset FROM slab", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?)))?;
            f.slab_z = list.into_iter().map(|(lv, o)| f.z(lv, o)).collect();
        }
        if f.has("wall") {
            f.curved_walls = db
                .query_row("SELECT count(*) FROM wall WHERE bulge <> 0", [], |r| r.get(0))
                .unwrap_or(0);
            let rating = if f.has("prop") {
                "(SELECT CAST(p.value AS TEXT) FROM prop p WHERE p.kind = 'wall' AND p.ref = w.id \
                 AND p.key = 'Pset_WallCommon.FireRating' LIMIT 1)"
            } else {
                "NULL"
            };
            let sql = format!(
                "SELECT COALESCE(t.name, ''), COALESCE({rating}, '') FROM wall w LEFT JOIN wall_type t ON t.id = w.wtype"
            );
            f.walls = rows(db, &sql, |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))?;
            let w = rows(db, "SELECT level, ax, ay, bx, by, base_offset, height FROM wall", |r| {
                Ok((r.get::<_, i64>(0)?, [r.get::<_, f64>(1)?, r.get(2)?, r.get(3)?, r.get(4)?, r.get(5)?, r.get(6)?]))
            })?;
            for (lv, [ax, ay, bx, by, bo, h]) in w {
                let z = f.z(lv, bo);
                f.masses.push(vec![[ax, ay, z], [bx, by, z], [ax, ay, z + h], [bx, by, z + h]]);
            }
        }
        if f.has("roof") {
            let pts = rows(
                db,
                "SELECT v.roof, v.x, v.y FROM roof_vertex v JOIN roof r ON r.id = v.roof                  WHERE r.kind = 'extrusion' ORDER BY v.roof, v.idx",
                |r| Ok((r.get::<_, i64>(0)?, (r.get::<_, f64>(1)?, r.get::<_, f64>(2)?))),
            )
            .unwrap_or_default();
            let mut curved = 0;
            let mut i = 0;
            while i < pts.len() {
                let j = pts[i..].iter().position(|p| p.0 != pts[i].0).map_or(pts.len(), |n| i + n);
                let prof: Vec<(f64, f64)> = pts[i..j].iter().map(|p| p.1).collect();
                curved += i64::from(arched(&prof));
                i = j;
            }
            f.curved_roofs = curved;
        }
        if f.has("solid") {
            // a solid that fails to build has no shape to measure; its error is reported elsewhere
            for id in rows(db, "SELECT id FROM solid WHERE void = 0", |r| r.get::<_, i64>(0))? {
                if let Ok(mesh) = m.solid_mesh(id) {
                    f.masses.push(mesh_points(&mesh));
                }
            }
        }
        if f.has("comp") && f.has("comp_type") && f.has("comp_family") {
            let list = rows(
                db,
                "SELECT c.id, c.level, c.x, c.y, c.z, fam.name, fam.category FROM comp c \
                 JOIN comp_type t ON t.id = c.ctype JOIN comp_family fam ON fam.id = t.family",
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        [r.get::<_, f64>(2)?, r.get::<_, f64>(3)?, r.get::<_, f64>(4)?],
                        r.get::<_, String>(5)?,
                        r.get::<_, String>(6)?,
                    ))
                },
            )?;
            for (id, level, at, family, category) in list {
                f.comps.push(Comp { id, level, at: [at[0], at[1], f.z(level, at[2])], family, category });
            }
        }
        if f.has("mep_port") {
            f.port_comps = rows(db, "SELECT DISTINCT comp FROM mep_port", |r| r.get::<_, i64>(0))?
                .into_iter()
                .collect();
        }
        if f.has("comp") && f.has("comp_family") && f.has("comp_type") {
            let all = m.component_ports(None).map_err(|e| format!("ports: {e:?}"))?;
            f.ports = all.iter().map(|p| [p.at.0, p.at.1, p.at.2]).collect();
            if f.has("mep_port") && f.has("mep_system") {
                let q = "SELECT p.comp, p.connector, s.kind FROM mep_port p JOIN mep_system s ON s.id=p.system";
                let linked: HashMap<(i64, String), String> =
                    rows(db, q, |r| Ok(((r.get::<_, i64>(0)?, r.get::<_, String>(1)?), r.get::<_, String>(2)?)))?.into_iter().collect();
                f.sys_ports = all
                    .iter()
                    .filter_map(|p| linked.get(&(p.comp, p.name.clone())).map(|k| (k.clone(), [p.at.0, p.at.1, p.at.2])))
                    .collect();
            }
        }
        if f.has("mep_system") {
            f.systems = rows(db, "SELECT kind FROM mep_system", |r| r.get::<_, String>(0))?;
        }
        if f.has("mep_run") {
            let sys: HashMap<i64, String> = if f.has("mep_system") {
                rows(db, "SELECT id, kind FROM mep_system", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))?
                    .into_iter()
                    .collect()
            } else {
                HashMap::new()
            };
            let list = rows(
                db,
                "SELECT id, system, level, domain, shape, w, h, dn, slope, points, cap0, cap1 FROM mep_run",
                |r| {
                    Ok((
                        r.get::<_, i64>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, String>(3)?,
                        r.get::<_, String>(4)?,
                        [r.get::<_, f64>(5)?, r.get::<_, f64>(6)?, r.get::<_, f64>(8)?],
                        r.get::<_, Option<i64>>(7)?,
                        r.get::<_, String>(9)?,
                        [r.get::<_, i64>(10)?, r.get::<_, i64>(11)?],
                    ))
                },
            )?;
            for (id, s, level, domain, shape, whs, dn, pts, caps) in list {
                f.runs.push(Run {
                    id,
                    system: sys.get(&s).cloned().unwrap_or_default(),
                    level,
                    domain,
                    shape,
                    w: whs[0],
                    h: whs[1],
                    dn,
                    slope: whs[2],
                    pts: parse_points(&pts),
                    cap: [caps[0] != 0, caps[1] != 0],
                });
            }
        }
        if f.has("room") {
            f.rooms = rows(db, "SELECT id, level, name, px, py FROM room", |r| {
                Ok(Room {
                    id: r.get(0)?,
                    level: r.get(1)?,
                    name: r.get(2)?,
                    at: [r.get(3)?, r.get(4)?],
                })
            })?;
        }
        if f.has("shaft") && f.has("shaft_vertex") {
            let list = rows(db, "SELECT id, level, top_level FROM shaft", |r| {
                Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?, r.get::<_, i64>(2)?))
            })?;
            for (id, level, top) in list {
                let v = rows(db, &format!("SELECT x, y FROM shaft_vertex WHERE shaft = {id}"), |r| {
                    Ok((r.get::<_, f64>(0)?, r.get::<_, f64>(1)?))
                })?;
                let bbox = (!v.is_empty()).then(|| {
                    let lo = |g: fn(&(f64, f64)) -> f64| v.iter().map(g).fold(f64::MAX, f64::min);
                    let hi = |g: fn(&(f64, f64)) -> f64| v.iter().map(g).fold(f64::MIN, f64::max);
                    [lo(|p| p.0), lo(|p| p.1), hi(|p| p.0), hi(|p| p.1)]
                });
                f.shafts.push(Shaft { level, top, bbox });
            }
        }
        if f.has("gridshell_node") {
            let n = rows(db, "SELECT x, y, z FROM gridshell_node", |r| {
                Ok([r.get::<_, f64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?])
            })?;
            f.points.extend(n.iter().copied());
            f.shell_pts = n;
        }
        if f.has("solid") {
            for (id, def) in rows(db, "SELECT id, def FROM solid WHERE kind IN ('sweep', 'swept_blend')", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                if let Some(s) = serde_json::from_str(&def).ok().and_then(|v: serde_json::Value| sweep_of(id, &v)) {
                    f.sweeps.push(s);
                }
            }
            if f.has("solid_qty") {
                // swept or extruded bodies have no member ends; their boxes give the height
                let sql = "SELECT q.x0, q.y0, q.z0, q.x1, q.y1, q.z1 FROM solid_qty q JOIN solid s ON s.id = q.solid \
                           WHERE s.void = 0 AND q.error IS NULL";
                for b in rows(db, sql, |r| (0..6usize).map(|i| r.get::<_, f64>(i)).collect::<rusqlite::Result<Vec<f64>>>())? {
                    f.points.extend([[b[0], b[1], b[2]], [b[3], b[4], b[5]]]);
                }
            }
        }
        if f.has("freeform") {
            // control nets: z is above the level
            for (level, net) in rows(db, "SELECT level, net FROM freeform", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
                if let Ok(v) = serde_json::from_str::<Vec<[f64; 3]>>(&net) {
                    let z0 = f.z(level, 0.0);
                    let pts: Vec<P3> = v.into_iter().map(|p| [p[0], p[1], p[2] + z0]).collect();
                    f.points.extend(pts.iter().copied());
                    f.shell_pts.extend(pts);
                }
            }
        }
        if f.has("iso_footing") {
            let b = rows(db, "SELECT level, x, y FROM iso_footing", |r| Ok((r.get::<_, i64>(0)?, r.get::<_, f64>(1)?, r.get::<_, f64>(2)?)))?;
            f.bases = b.into_iter().map(|(l, x, y)| [x, y, f.z(l, 0.0)]).collect();
        }
        let ends: Vec<P3> = f.members.iter().flat_map(|m| [m.a, m.b]).collect();
        f.points.extend(ends);
        Ok(f)
    }

    /// Count by spec: terms joined by `+`; `table`, `comp:re`, `mep:re`, `sysk:re`, `ptype:re`,
    /// `mat:re`, `wt:re` (walls by type name or fire rating), `room:re`, `curved`, `shaftmulti`.
    /// Err names the table the model lacks.
    pub fn count(&self, spec: &str) -> Result<i64, String> {
        let mut total = 0;
        for term in spec.split('+').map(str::trim).filter(|t| !t.is_empty()) {
            let (kind, pat) = term.split_once(':').unwrap_or((term, ""));
            total += match kind {
                "comp" => {
                    self.need("comp")?;
                    let r = rx(pat)?;
                    self.comps.iter().filter(|c| r.is_match(&c.family) || r.is_match(&c.category)).count() as i64
                }
                "mep" => {
                    self.need("mep_run")?;
                    let r = rx(pat)?;
                    self.runs.iter().filter(|c| r.is_match(&c.domain) || r.is_match(&c.shape)).count() as i64
                }
                "sysk" => {
                    self.need("mep_system")?;
                    let r = rx(pat)?;
                    self.systems.iter().filter(|s| r.is_match(s)).count() as i64
                }
                "ptype" => {
                    self.need("member_type")?;
                    let r = rx(pat)?;
                    self.mtypes.values().filter(|t| r.is_match(&t.0) || r.is_match(&t.1)).count() as i64
                }
                "mat" => {
                    self.need("member_type")?;
                    let r = rx(pat)?;
                    self.members
                        .iter()
                        .filter(|m| m.mtype.and_then(|t| self.mtypes.get(&t)).is_some_and(|t| r.is_match(&t.2)))
                        .count() as i64
                }
                "wt" => {
                    self.need("wall")?;
                    let r = rx(pat)?;
                    self.walls.iter().filter(|(t, fr)| r.is_match(t) || r.is_match(fr)).count() as i64
                }
                "room" => {
                    self.need("room")?;
                    let r = rx(pat)?;
                    self.rooms.iter().filter(|c| r.is_match(&c.name)).count() as i64
                }
                "doubly" => {
                    ["freeform", "shell_elem", "gridshell"].iter().map(|t| self.n(t)).sum::<i64>()
                }
                "curved" => self.curved_walls + self.curved_roofs + self.members.iter().filter(|m| m.bulge != 0.0).count() as i64,
                "shaftmulti" => {
                    self.need("shaft")?;
                    self.shafts.iter().filter(|s| s.top != s.level).count() as i64
                }
                table => {
                    self.need(table)?;
                    self.n(table)
                }
            };
        }
        Ok(total)
    }

    fn need(&self, table: &str) -> Result<(), String> {
        if table.is_empty() || !table.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
            return Err(format!("unknown count term: {table}"));
        }
        if self.has(table) {
            Ok(())
        } else {
            Err(format!("no capability: the model has no table {table}"))
        }
    }
}

/// Cell hash over points: "is anything within r of p".
pub struct Cells {
    cell: f64,
    map: HashMap<(i64, i64, i64), Vec<(P3, usize)>>,
}

impl Cells {
    pub fn new(cell: f64) -> Cells {
        Cells { cell, map: HashMap::new() }
    }

    fn key(&self, p: P3) -> (i64, i64, i64) {
        let c = |v: f64| (v / self.cell).floor() as i64;
        (c(p[0]), c(p[1]), c(p[2]))
    }

    pub fn add(&mut self, p: P3, id: usize) {
        let k = self.key(p);
        self.map.entry(k).or_default().push((p, id));
    }

    /// Ids of stored points within `r` of `p`.
    pub fn near(&self, p: P3, r: f64) -> Vec<usize> {
        let n = (r / self.cell).ceil().max(1.0) as i64;
        let k = self.key(p);
        let mut out = vec![];
        for dx in -n..=n {
            for dy in -n..=n {
                for dz in -n..=n {
                    if let Some(v) = self.map.get(&(k.0 + dx, k.1 + dy, k.2 + dz)) {
                        out.extend(v.iter().filter(|(q, _)| dist(*q, p) <= r).map(|(_, i)| *i));
                    }
                }
            }
        }
        out
    }
}

pub fn dist(a: P3, b: P3) -> f64 {
    ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
}

/// Points along a..b no more than `step` apart, both ends included.
pub fn sample(a: P3, b: P3, step: f64) -> Vec<P3> {
    // rounded up: a floor would leave gaps a little over `step`, and a T on a run's middle,
    // half a step from the nearest sample, would miss it
    let n = ((dist(a, b) / step).ceil() as usize).max(1);
    (0..=n)
        .map(|i| {
            let t = i as f64 / n as f64;
            [a[0] + (b[0] - a[0]) * t, a[1] + (b[1] - a[1]) * t, a[2] + (b[2] - a[2]) * t]
        })
        .collect()
}

/// Smallest rise of a profile point over the chord of its neighbours that makes it an arch, m.
const ARCH_RISE: f64 = 0.05;

/// A roof profile (u, z) is arched when it has at least five points and one interior point
/// lies above the chord between its neighbours by more than `ARCH_RISE`; a trapezoid or a
/// pitched profile is not.
pub(super) fn arched(p: &[(f64, f64)]) -> bool {
    p.len() >= 5
        && p.windows(3).any(|w| {
            let (a, b, c) = (w[0], w[1], w[2]);
            let du = c.0 - a.0;
            du.abs() > 1e-9 && b.1 - (a.1 + (c.1 - a.1) * (b.0 - a.0) / du) > ARCH_RISE
        })
}

#[cfg(test)]
mod arch_tests {
    use super::arched;

    #[test]
    fn a_trapezoid_is_not_curved_and_a_nine_point_arch_is() {
        let trapezoid = [(0.0, 0.0), (1.0, 1.0), (5.0, 1.0), (6.0, 0.0)];
        assert!(!arched(&trapezoid));
        let pitched = [(0.0, 0.0), (3.0, 1.5), (6.0, 0.0)];
        assert!(!arched(&pitched));
        let five_flat = [(0.0, 0.0), (1.0, 0.5), (2.0, 1.0), (3.0, 1.5), (4.0, 2.0)];
        assert!(!arched(&five_flat), "a straight slope is not an arch");
        let arch: Vec<(f64, f64)> = (0..9)
            .map(|i| {
                let a = std::f64::consts::PI * i as f64 / 8.0;
                (4.0 - 4.0 * a.cos(), 3.0 * a.sin())
            })
            .collect();
        assert!(arched(&arch));
    }
}
