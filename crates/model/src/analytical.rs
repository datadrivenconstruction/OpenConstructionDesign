//! Analytical model built from physical elements: axes of load-bearing columns, beams and braces (through
//! the section centre), mid-surfaces of straight walls and flat slabs. Nodes are merged within
//! a tolerance, ends snap to more important elements ([`ddc_geom::analytical`]); supports come from
//! column pad footings (fixed), strip footings under walls (pinned) and rafts (fixed).
//! Adjustments (`analytical_adj`): end offsets and releases. Export is plain JSON.
//! The assembly is cached until the next file change ([`Model::analytical`]).

use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use ddc_geom::analytical::{self as an, MemberIn, PanelIn, Release, Tolerances};
use ddc_geom::member::Shape;
use ddc_geom::section_props::{self, SectionProps};
use ddc_geom::{v2, v3, V2, V3};
use rusqlite::{params, Connection, OptionalExtension};
use serde_json::{json, Value as J};

use crate::ops::Inv;
use crate::{Error, Model};

/// Elements with analytical adjustments: kind to table.
pub const ADJ_KINDS: [(&str, &str); 5] = [
    ("column", "col"),
    ("beam", "beam"),
    ("brace", "brace"),
    ("wall", "wall"),
    ("slab", "slab"),
];

#[derive(Clone, Copy, Debug, PartialEq, Default)]
pub struct AnalyticalAdj {
    /// Offset of the axis start and end (world), m.
    pub start: (f64, f64, f64),
    pub end: (f64, f64, f64),
    /// End releases; None means default (braces pinned, others fixed).
    pub release_start: Option<Release>,
    pub release_end: Option<Release>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnMember {
    pub kind: &'static str,
    pub id: i64,
    pub level: i64,
    /// Nodes along the axis from start to end.
    pub nodes: Vec<usize>,
    /// Pinned when ry and rz are released at that end (a view of `releases`).
    pub release: (Release, Release),
    pub section: Option<usize>,
    pub material: String,
    /// Stable key `m:<kind>:<id>`.
    pub key: String,
    /// frame | truss | excluded.
    pub role: &'static str,
    /// Released local DOFs per end (ux uy uz rx ry rz), true = released.
    pub releases: [[bool; 6]; 2],
    /// Rigid end offsets (global, m) from the node to the flexible end.
    pub ecc: [V3; 2],
    /// Local y and z (x runs from the first to the last node), roll included.
    pub axes: (V3, V3),
    /// Buckling data for checks: ky, kz, Ly, Lz.
    pub buckling: [Option<f64>; 4],
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnPanel {
    pub kind: &'static str,
    pub id: i64,
    pub level: i64,
    pub nodes: Vec<usize>,
    pub inner: Vec<usize>,
    pub thickness: f64,
    pub material: String,
    /// Stable key `p:<kind>:<id>`.
    pub key: String,
    /// load_only | line_support | shell | plate | membrane | excluded.
    pub role: &'static str,
    /// Openings (world), they carry no area load.
    pub holes: Vec<Vec<V3>>,
    /// Load distribution: (1 one-way | 2 two-way, span direction in degrees from +X).
    pub span: Option<(u8, f64)>,
    pub mesh: Option<f64>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnSupport {
    pub node: usize,
    /// Fixed when all six DOFs are fixed, else pinned (a view of `fixed`).
    pub kind: Release,
    /// Source: (iso_footing | strip_footing | raft | user, id).
    pub from: (&'static str, i64),
    pub key: String,
    /// Fixed global DOFs ux uy uz rx ry rz.
    pub fixed: [bool; 6],
    /// Springs on the DOFs that are not fixed, N/m and N m/rad; 0 = none.
    pub springs: [f64; 6],
}

#[derive(Clone, Debug, PartialEq)]
pub struct AnSection {
    pub name: String,
    pub shape: String,
    pub area: f64,
    pub iy: f64,
    pub iz: f64,
    /// Full section properties (J, moduli, shear areas, shear centre); `area`, `iy`, `iz`
    /// repeat its values.
    pub props: SectionProps,
}

/// Analytical issue: code (free_end, close_nodes, short, unsupported, curved, sloped), element
/// and node, if any.
#[derive(Clone, Debug, PartialEq)]
pub struct AnIssue {
    pub code: &'static str,
    pub kind: &'static str,
    pub id: i64,
    pub node: Option<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Analytical {
    pub nodes: Vec<V3>,
    pub members: Vec<AnMember>,
    pub panels: Vec<AnPanel>,
    pub supports: Vec<AnSupport>,
    pub sections: Vec<AnSection>,
    pub issues: Vec<AnIssue>,
    /// Stable key per node (design 1.3).
    pub node_keys: Vec<String>,
    /// How each `an_override` row was used.
    pub overrides: Vec<crate::analytical_keys::OverrideUse>,
}

fn adjs(db: &Connection) -> Result<HashMap<(String, i64), AnalyticalAdj>, Error> {
    if !crate::rebar::has_table(db, "analytical_adj")? {
        return Ok(HashMap::new());
    }
    let mut st = db.prepare(
        "SELECT kind, ref, dax, day, daz, dbx, dby, dbz, rel_a, rel_b FROM analytical_adj",
    )?;
    let rows = st.query_map([], |r| {
        let rel = |s: Option<String>| s.and_then(|s| Release::parse(&s));
        Ok((
            (r.get::<_, String>(0)?, r.get::<_, i64>(1)?),
            AnalyticalAdj {
                start: (r.get(2)?, r.get(3)?, r.get(4)?),
                end: (r.get(5)?, r.get(6)?, r.get(7)?),
                release_start: rel(r.get(8)?),
                release_end: rel(r.get(9)?),
            },
        ))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

/// Member type: (name, section, material).
fn member_types(db: &Connection) -> Result<HashMap<i64, (String, String, String)>, Error> {
    let mut st = db.prepare("SELECT id, name, shape, material FROM member_type")?;
    let rows = st.query_map([], |r| {
        Ok((r.get::<_, i64>(0)?, (r.get(1)?, r.get(2)?, r.get(3)?)))
    })?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn mtypes_of(db: &Connection, table: &str) -> Result<HashMap<i64, i64>, Error> {
    let mut st = db.prepare(&format!("SELECT id, mtype FROM {table}"))?;
    let rows = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(rows.collect::<Result<_, _>>()?)
}

fn t3(p: (f64, f64, f64)) -> V3 {
    v3(p.0, p.1, p.2)
}

/// Graph input and element information.
struct Src {
    kind: &'static str,
    id: i64,
    level: i64,
    mtype: Option<i64>,
    material: String,
    thickness: f64,
    /// Section roll of a member, rad; slab openings (world) of a panel.
    rot: f64,
    holes: Vec<Vec<V3>>,
}

/// Tolerance band for nodes on the bottom edge of a wall and the top of a slab.
const ON: f64 = 0.05;

fn inside(p: V2, ring: &[V2]) -> bool {
    ddc_geom::predicates::point_in_ring(p, ring) >= 0
}

pub(crate) fn build(db: &Connection, tol: Tolerances) -> Result<Analytical, Error> {
    let ovs = crate::analytical_keys::read(db)?;
    let tol = crate::analytical_keys::tolerances(&ovs, tol);
    let adj = adjs(db)?;
    let lv: HashMap<i64, f64> = crate::walls::levels(db)?.into_iter().collect();
    let mt = member_types(db)?;
    let mut issues: Vec<AnIssue> = vec![];
    let mut ms: Vec<MemberIn> = vec![];
    let mut msrc: Vec<Src> = vec![];
    let col_t = mtypes_of(db, "col")?;
    let beam_t = mtypes_of(db, "beam")?;
    for b in crate::structure::build_all(db)? {
        if !b.structural {
            continue;
        }
        let Some((a, e)) = b.m.axis_ends() else {
            issues.push(AnIssue {
                code: "curved",
                kind: b.kind,
                id: b.id,
                node: None,
            });
            continue;
        };
        // axis passes through the section centre
        let u = (e - a).normalized();
        let (lo, hi) = b.m.extents();
        let c = v2((lo.x + hi.x) / 2.0, (lo.y + hi.y) / 2.0);
        let (ey, ez) = section_axes(u, b.m.rotation);
        let off = ey * c.x + ez * c.y;
        let column = b.kind == "column";
        let mtype = (if column {
            col_t.get(&b.id)
        } else {
            beam_t.get(&b.id)
        })
        .copied();
        ms.push(MemberIn {
            a: a + off,
            b: e + off,
            prio: if column { 0 } else { 1 },
        });
        msrc.push(Src {
            kind: if column { "column" } else { "beam" },
            id: b.id,
            level: b.level,
            mtype,
            material: mtype
                .and_then(|t| mt.get(&t))
                .map(|t| t.2.clone())
                .unwrap_or_default(),
            thickness: 0.0,
            rot: b.m.rotation,
            holes: vec![],
        });
    }
    if crate::rebar::has_table(db, "brace")? {
        let mut st = db.prepare(
            "SELECT id, level, mtype, ax, ay, az, bx, by, bz, rotation FROM brace ORDER BY id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                r.get::<_, i64>(2)?,
                (
                    r.get::<_, f64>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, f64>(5)?,
                ),
                (
                    r.get::<_, f64>(6)?,
                    r.get::<_, f64>(7)?,
                    r.get::<_, f64>(8)?,
                ),
                r.get::<_, f64>(9)?,
            ))
        })?;
        for row in rows {
            let (id, level, mtype, a, b, rot) = row?;
            let e = lv.get(&level).copied().unwrap_or(0.0);
            ms.push(MemberIn {
                a: v3(a.0, a.1, a.2 + e),
                b: v3(b.0, b.1, b.2 + e),
                prio: 3,
            });
            msrc.push(Src {
                kind: "brace",
                id,
                level,
                mtype: Some(mtype),
                material: mt.get(&mtype).map(|t| t.2.clone()).unwrap_or_default(),
                thickness: 0.0,
                rot,
                holes: vec![],
            });
        }
    }
    for (m, s) in ms.iter_mut().zip(&msrc) {
        if let Some(a) = adj.get(&(s.kind.to_string(), s.id)) {
            m.a = m.a + t3(a.start);
            m.b = m.b + t3(a.end);
        }
    }
    // panels: walls (mid-thickness), flat slabs (mid-thickness)
    let mut ps: Vec<PanelIn> = vec![];
    let mut psrc: Vec<Src> = vec![];
    let mut wall_base: Vec<(i64, V2, V2, f64)> = vec![];
    {
        let mut st = db.prepare(
            "SELECT w.id, w.level, w.ax, w.ay, w.bx, w.by, w.bulge, w.base_offset, w.height, \
             w.offset0, (SELECT total(t.thickness) FROM wall_type_layer t \
             WHERE t.wtype = w.wtype), \
             (SELECT t.material FROM wall_type_layer t WHERE t.wtype = w.wtype \
              ORDER BY t.thickness DESC LIMIT 1) FROM wall w ORDER BY w.id",
        )?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, i64>(1)?,
                v2(r.get(2)?, r.get(3)?),
                v2(r.get(4)?, r.get(5)?),
                r.get::<_, f64>(6)?,
                r.get::<_, f64>(7)?,
                r.get::<_, f64>(8)?,
                r.get::<_, Option<f64>>(9)?,
                r.get::<_, f64>(10)?,
                r.get::<_, Option<String>>(11)?,
            ))
        })?;
        for row in rows {
            let (id, level, a, b, bulge, base, h, o0, t, mat) = row?;
            if bulge.abs() > 1e-9 {
                issues.push(AnIssue {
                    code: "curved",
                    kind: "wall",
                    id,
                    node: None,
                });
                continue;
            }
            let center = o0.unwrap_or(-t / 2.0) + t / 2.0;
            let (a, b) = crate::footings::shift_left(a, b, 0.0, center);
            if (b - a).len() < 1e-6 || h <= 0.0 {
                continue;
            }
            let z0 = lv.get(&level).copied().unwrap_or(0.0) + base;
            ps.push(PanelIn {
                ring: vec![
                    v3(a.x, a.y, z0),
                    v3(b.x, b.y, z0),
                    v3(b.x, b.y, z0 + h),
                    v3(a.x, a.y, z0 + h),
                ],
                prio: 0,
            });
            psrc.push(Src {
                kind: "wall",
                id,
                level,
                mtype: None,
                material: mat.unwrap_or_default(),
                thickness: t,
                rot: 0.0,
                holes: vec![],
            });
            wall_base.push((id, a, b, z0));
        }
    }
    {
        let mut st = db.prepare(
            "SELECT s.id, s.level, s.offset, s.slope, \
             (SELECT total(t.thickness) FROM slab_type_layer t WHERE t.stype = s.stype), \
             (SELECT t.material FROM slab_type_layer t WHERE t.stype = s.stype \
              ORDER BY t.thickness DESC LIMIT 1) FROM slab s ORDER BY s.id",
        )?;
        let rows: Vec<_> = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, i64>(0)?,
                    r.get::<_, i64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, Option<String>>(3)?,
                    r.get::<_, f64>(4)?,
                    r.get::<_, Option<String>>(5)?,
                ))
            })?
            .collect::<Result<_, _>>()?;
        for (id, level, off, slope, t, mat) in rows {
            if slope.is_some_and(|s| !s.trim().is_empty()) {
                issues.push(AnIssue {
                    code: "sloped",
                    kind: "slab",
                    id,
                    node: None,
                });
                continue;
            }
            let z = lv.get(&level).copied().unwrap_or(0.0) + off - t / 2.0;
            let mut rings = crate::slabs::rings(db, id)?.into_iter();
            let Some(outer) = rings.next() else {
                continue;
            };
            if outer.len() < 3 {
                continue;
            }
            let holes: Vec<Vec<V3>> = rings
                .filter(|r| r.len() >= 3)
                .map(|r| r.iter().map(|p| v3(p.0, p.1, z)).collect())
                .collect();
            ps.push(PanelIn {
                ring: outer.iter().map(|p| v3(p.0, p.1, z)).collect(),
                prio: 2,
            });
            psrc.push(Src {
                kind: "slab",
                id,
                level,
                mtype: None,
                material: mat.unwrap_or_default(),
                thickness: t,
                rot: 0.0,
                holes,
            });
        }
    }
    let g = an::build(&ms, &ps, tol);
    // supports
    let mut sup: BTreeMap<usize, AnSupport> = BTreeMap::new();
    let mut add = |node: usize, kind: Release, from: (&'static str, i64)| {
        let r = kind == Release::Fixed;
        let fixed = [true, true, true, r, r, r];
        let e = sup.entry(node).or_insert(AnSupport {
            node,
            kind,
            from,
            key: String::new(),
            fixed,
            springs: [0.0; 6],
        });
        if kind == Release::Fixed {
            e.kind = Release::Fixed;
            e.fixed = [true; 6];
        }
    };
    if crate::rebar::has_table(db, "iso_footing")? {
        let mut st = db.prepare("SELECT id, col FROM iso_footing WHERE col IS NOT NULL")?;
        let rows: Vec<(i64, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let col_at: HashMap<i64, usize> = msrc
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == "column")
            .map(|(i, s)| (s.id, i))
            .collect();
        for (fid, col) in rows {
            if let Some(&i) = col_at.get(&col) {
                add(g.members[i][0], Release::Fixed, ("iso_footing", fid));
            }
        }
        let mut st = db.prepare("SELECT id, wall FROM strip_footing WHERE wall IS NOT NULL")?;
        let rows: Vec<(i64, i64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
            .collect::<Result<_, _>>()?;
        let wall_at: HashMap<i64, usize> = psrc
            .iter()
            .enumerate()
            .filter(|(_, s)| s.kind == "wall")
            .map(|(j, s)| (s.id, j))
            .collect();
        let base_of: HashMap<i64, f64> = wall_base.iter().map(|w| (w.0, w.3)).collect();
        for (fid, wall) in rows {
            let (Some(&j), Some(&z0)) = (wall_at.get(&wall), base_of.get(&wall)) else {
                continue;
            };
            for &n in &g.panels[j] {
                if (g.nodes[n].z - z0).abs() <= ON {
                    add(n, Release::Pinned, ("strip_footing", fid));
                }
            }
        }
        let mut st = db.prepare(
            "SELECT r.id, r.top_offset, l.elevation FROM raft r JOIN level l ON l.id = r.level",
        )?;
        let rafts: Vec<(i64, f64, f64)> = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<_, _>>()?;
        for (rid, top, elev) in rafts {
            let mut vs =
                db.prepare_cached("SELECT x, y FROM raft_vertex WHERE raft=?1 ORDER BY idx")?;
            let ring: Vec<V2> = vs
                .query_map([rid], |r| Ok(v2(r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            if ring.len() < 3 {
                continue;
            }
            let z = elev + top;
            for (n, p) in g.nodes.iter().enumerate() {
                if (p.z - z).abs() <= ON + 0.5 * tol.snap && inside(v2(p.x, p.y), &ring) {
                    add(n, Release::Fixed, ("raft", rid));
                }
            }
        }
    }
    let supported: Vec<bool> = (0..g.nodes.len()).map(|n| sup.contains_key(&n)).collect();
    let rep = an::check(&g, &supported, tol);
    for (i, k) in &rep.free_ends {
        let s = &msrc[*i];
        let m = &g.members[*i];
        issues.push(AnIssue {
            code: "free_end",
            kind: s.kind,
            id: s.id,
            node: Some(if *k == 0 { m[0] } else { m[m.len() - 1] }),
        });
    }
    for i in &rep.short {
        issues.push(AnIssue {
            code: "short",
            kind: msrc[*i].kind,
            id: msrc[*i].id,
            node: None,
        });
    }
    for (a, _) in &rep.close_nodes {
        issues.push(AnIssue {
            code: "close_nodes",
            kind: "node",
            id: *a as i64 + 1,
            node: Some(*a),
        });
    }
    for (mi, pi) in &rep.unsupported {
        let (kind, id) = match (mi.first(), pi.first()) {
            (Some(i), _) => (msrc[*i].kind, msrc[*i].id),
            (None, Some(j)) => (psrc[*j].kind, psrc[*j].id),
            _ => continue,
        };
        issues.push(AnIssue {
            code: "unsupported",
            kind,
            id,
            node: None,
        });
    }
    // sections by type
    let mut sections: Vec<AnSection> = vec![];
    let mut sec_of: HashMap<i64, usize> = HashMap::new();
    let mut section = |t: i64| -> Option<usize> {
        if let Some(i) = sec_of.get(&t) {
            return Some(*i);
        }
        let (name, shape, _) = mt.get(&t)?;
        let sh = Shape::parse(shape)?;
        let props = section_props::of_shape(&sh);
        sections.push(AnSection {
            name: name.clone(),
            shape: shape.clone(),
            area: props.area,
            iy: props.iy,
            iz: props.iz,
            props,
        });
        sec_of.insert(t, sections.len() - 1);
        Some(sections.len() - 1)
    };
    let members: Vec<AnMember> = msrc
        .iter()
        .zip(&g.members)
        .map(|(s, nodes)| {
            let def = if s.kind == "brace" {
                Release::Pinned
            } else {
                Release::Fixed
            };
            let a = adj.get(&(s.kind.to_string(), s.id));
            let release = (
                a.and_then(|a| a.release_start).unwrap_or(def),
                a.and_then(|a| a.release_end).unwrap_or(def),
            );
            let mask = |r: Release| {
                if r == Release::Pinned {
                    crate::analytical_keys::PINNED
                } else {
                    crate::analytical_keys::FIXED
                }
            };
            let (p0, p1) = (g.nodes[nodes[0]], g.nodes[nodes[nodes.len() - 1]]);
            let u = (p1 - p0).normalized();
            AnMember {
                kind: s.kind,
                id: s.id,
                level: s.level,
                nodes: nodes.clone(),
                release,
                section: s.mtype.and_then(&mut section),
                material: s.material.clone(),
                key: String::new(),
                role: if s.kind == "brace" { "truss" } else { "frame" },
                releases: [mask(release.0), mask(release.1)],
                ecc: [V3::default(); 2],
                axes: section_axes(u, s.rot),
                buckling: [None; 4],
            }
        })
        .collect();
    let panels = psrc
        .iter()
        .enumerate()
        .map(|(j, s)| AnPanel {
            kind: s.kind,
            id: s.id,
            level: s.level,
            nodes: g.panels[j].clone(),
            inner: g.inner[j].clone(),
            thickness: s.thickness,
            material: s.material.clone(),
            key: String::new(),
            role: if s.kind == "wall" { "line_support" } else { "load_only" },
            holes: s.holes.clone(),
            span: None,
            mesh: None,
        })
        .collect();
    let mut a = Analytical {
        nodes: g.nodes,
        members,
        panels,
        supports: sup.into_values().collect(),
        sections,
        issues,
        node_keys: vec![],
        overrides: vec![],
    };
    crate::analytical_keys::assign(&mut a);
    crate::analytical_keys::apply(db, &mut a, &ovs, tol)?;
    Ok(a)
}

/// Section axes of the member are the same as for the element body.
fn section_axes(u: V3, rotation: f64) -> (V3, V3) {
    let z = v3(0.0, 0.0, 1.0);
    let h = z.cross(u);
    let ey = if h.len() < 1e-9 {
        v3(1.0, 0.0, 0.0)
    } else {
        h.normalized()
    };
    let ez = u.cross(ey);
    let (c, s) = (rotation.cos(), rotation.sin());
    (ey * c + ez * s, ez * c - ey * s)
}

fn r6(x: f64) -> f64 {
    (x * 1e6).round() / 1e6
}

impl Analytical {
    /// Plain JSON: nodes, members, panels, supports, sections, materials, issues (1-based numbers).
    pub fn to_json(&self) -> J {
        let mut mats: BTreeMap<String, Option<f64>> = BTreeMap::new();
        for m in self
            .members
            .iter()
            .map(|m| &m.material)
            .chain(self.panels.iter().map(|p| &p.material))
        {
            if !m.is_empty() {
                mats.insert(m.clone(), crate::struct_ext::density(m));
            }
        }
        let ids = |v: &[usize]| v.iter().map(|n| n + 1).collect::<Vec<_>>();
        let mask = |r: &[bool; 6], on: char, off: char| -> String {
            r.iter().map(|x| if *x { on } else { off }).collect()
        };
        let p3 = |p: V3| [r6(p.x), r6(p.y), r6(p.z)];
        let nk = |n: usize| self.node_keys.get(n).cloned().unwrap_or_default();
        json!({
            "format": "ddc-analytical",
            "version": 2,
            "units": { "length": "m", "area": "m2", "inertia": "m4", "modulus": "m3",
                "density": "kg/m3", "spring": "N/m, N m/rad" },
            "nodes": self.nodes.iter().enumerate().map(|(i, p)| json!({
                "id": i + 1, "key": nk(i), "x": r6(p.x), "y": r6(p.y), "z": r6(p.z)
            })).collect::<Vec<_>>(),
            "members": self.members.iter().enumerate().map(|(i, m)| json!({
                "id": i + 1, "key": m.key, "kind": m.kind, "element": m.id,
                "nodes": ids(&m.nodes), "role": m.role,
                "section": m.section.map(|s| s + 1), "material": m.material,
                "release_start": m.release.0.name(), "release_end": m.release.1.name(),
                "releases": [mask(&m.releases[0], 'P', 'F'), mask(&m.releases[1], 'P', 'F')],
                "ecc_start": p3(m.ecc[0]), "ecc_end": p3(m.ecc[1]),
                "local_y": p3(m.axes.0), "local_z": p3(m.axes.1)
            })).collect::<Vec<_>>(),
            "panels": self.panels.iter().enumerate().map(|(i, p)| json!({
                "id": i + 1, "key": p.key, "kind": p.kind, "element": p.id, "role": p.role,
                "nodes": ids(&p.nodes), "inner_nodes": ids(&p.inner),
                "thickness": r6(p.thickness), "material": p.material,
                "holes": p.holes.iter().map(|h| h.iter().map(|q| p3(*q)).collect::<Vec<_>>())
                    .collect::<Vec<_>>(),
                "span": p.span.map(|s| json!({ "way": s.0, "deg": s.1 }))
            })).collect::<Vec<_>>(),
            "supports": self.supports.iter().map(|s| json!({
                "node": s.node + 1, "key": s.key, "type": s.kind.name(), "from": s.from.0,
                "element": s.from.1, "dofs": mask(&s.fixed, 'F', 'P'), "springs": s.springs
            })).collect::<Vec<_>>(),
            "overrides": self.overrides.iter().map(|o| json!({
                "id": o.id, "target": o.target, "field": o.field, "status": o.status
            })).collect::<Vec<_>>(),
            "sections": self.sections.iter().enumerate().map(|(i, s)| json!({
                "id": i + 1, "name": s.name, "shape": s.shape,
                "area": s.area, "iy": s.iy, "iz": s.iz, "iyz": s.props.iyz, "j": s.props.j,
                "wy": s.props.wy, "wz": s.props.wz,
                "wpl_y": s.props.wpl_y, "wpl_z": s.props.wpl_z,
                "avy": s.props.avy, "avz": s.props.avz,
                "centroid": [s.props.centroid.x, s.props.centroid.y],
                "shear_center": [s.props.shear_center.x, s.props.shear_center.y]
            })).collect::<Vec<_>>(),
            "materials": mats.into_iter().map(|(n, d)| json!({ "name": n, "density": d }))
                .collect::<Vec<_>>(),
            "issues": self.issues.iter().map(|q| json!({
                "code": q.code, "kind": q.kind, "element": q.id, "node": q.node.map(|n| n + 1)
            })).collect::<Vec<_>>(),
        })
    }

    /// Unit axis of member `i` from its first to its last node.
    pub fn axis(&self, i: usize) -> V3 {
        let m = &self.members[i];
        match (m.nodes.first(), m.nodes.last()) {
            (Some(&a), Some(&b)) => (self.nodes[b] - self.nodes[a]).normalized(),
            _ => V3::default(),
        }
    }

    pub fn node_of(&self, key: &str) -> Option<usize> {
        self.node_keys.iter().position(|k| k == key)
    }

    pub fn member_of(&self, key: &str) -> Option<usize> {
        self.members.iter().position(|m| m.key == key)
    }

    pub fn panel_of(&self, key: &str) -> Option<usize> {
        self.panels.iter().position(|p| p.key == key)
    }

    /// Member axis segments (for display).
    pub fn member_lines(&self) -> Vec<(V3, V3)> {
        self.members
            .iter()
            .flat_map(|m| {
                m.nodes
                    .windows(2)
                    .map(|w| (self.nodes[w[0]], self.nodes[w[1]]))
            })
            .collect()
    }
}

impl Model {
    /// Analytical model with default tolerances; one assembly per file revision (plan views,
    /// 3D and export share it).
    pub fn analytical(&self) -> Result<Arc<Analytical>, Error> {
        let key = crate::rebar::revision(&self.db)?;
        if let Ok(g) = self.rebar_cache.analytical.lock() {
            if let Some((k, a)) = &*g {
                if *k == key {
                    return Ok(a.clone());
                }
            }
        }
        let a = Arc::new(build(&self.db, Tolerances::default())?);
        if let Ok(mut g) = self.rebar_cache.analytical.lock() {
            *g = Some((key, a.clone()));
        }
        Ok(a)
    }

    pub fn analytical_with(&self, tol: Tolerances) -> Result<Analytical, Error> {
        build(&self.db, tol)
    }

    pub fn analytical_json(&self) -> Result<String, Error> {
        serde_json::to_string_pretty(&self.analytical()?.to_json())
            .map_err(|e| Error::Invalid(e.to_string()))
    }

    pub fn analytical_adj(&self, kind: &str, id: i64) -> Result<Option<AnalyticalAdj>, Error> {
        Ok(adjs(&self.db)?.get(&(kind.to_string(), id)).copied())
    }

    /// Sets an element's analytical adjustment (end offsets, releases) in one operation; an empty one
    /// removes the adjustment.
    pub fn set_analytical_adj(
        &mut self,
        kind: &str,
        ids: &[i64],
        a: AnalyticalAdj,
    ) -> Result<(), Error> {
        let Some((kind, table)) = ADJ_KINDS.into_iter().find(|k| k.0 == kind) else {
            return Err(Error::Invalid(format!("analytical kind '{kind}'")));
        };
        let v = [a.start.0, a.start.1, a.start.2, a.end.0, a.end.1, a.end.2];
        if v.iter().any(|x| !x.is_finite() || x.abs() > 10.0) {
            return Err(Error::Invalid(
                "analytical offsets must be within 10 m".into(),
            ));
        }
        for &id in ids {
            let n: i64 = self.db.query_row(
                &format!("SELECT count(*) FROM {table} WHERE id=?1"),
                [id],
                |r| r.get(0),
            )?;
            if n == 0 {
                return Err(Error::Invalid(format!("{kind} {id} not found")));
            }
        }
        let empty = a == AnalyticalAdj::default();
        self.op("analytical_adj", |db| {
            let mut inv = Inv::default();
            for &id in ids {
                let rid: Option<i64> = db
                    .query_row(
                        "SELECT id FROM analytical_adj WHERE kind=?1 AND ref=?2",
                        params![kind, id],
                        |r| r.get(0),
                    )
                    .optional()?;
                if let Some(r) = rid {
                    inv = inv.restore(db, "analytical_adj", "id", r)?;
                    db.execute("DELETE FROM analytical_adj WHERE id=?1", [r])?;
                }
                if !empty {
                    db.execute(
                        "INSERT INTO analytical_adj(kind, ref, dax, day, daz, dbx, dby, dbz, \
                         rel_a, rel_b) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
                        params![
                            kind,
                            id,
                            v[0],
                            v[1],
                            v[2],
                            v[3],
                            v[4],
                            v[5],
                            a.release_start.map(|r| r.name()),
                            a.release_end.map(|r| r.name())
                        ],
                    )?;
                    inv = inv.delete("analytical_adj", "id", db.last_insert_rowid());
                }
            }
            Ok(((), ids.first().copied().unwrap_or(0), inv))
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{BeamJoin, BeamSpec, ColumnSpec, FootingSpec, FootingStep, Just};

    fn portal(zjust: Just) -> (Model, [i64; 3]) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        let ct = m
            .add_member_type("RC 400", &Shape::Rect { b: 0.4, h: 0.4 }, "Concrete")
            .unwrap();
        let bt = m
            .add_member_type("RC 300x500", &Shape::Rect { b: 0.3, h: 0.5 }, "Concrete")
            .unwrap();
        let col = |x: f64| ColumnSpec {
            level: l0,
            top_level: Some(l1),
            member_type: ct,
            structural: true,
            at: (x, 0.0),
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        };
        let c0 = m.add_column(&col(0.0)).unwrap();
        let c1 = m.add_column(&col(6.0)).unwrap();
        let b = m
            .add_beam(&BeamSpec {
                level: l0,
                member_type: bt,
                a: (0.0, 0.0, 3.0),
                b: (6.0, 0.0, 3.0),
                bulge: 0.0,
                yjust: Just::Center,
                zjust,
                rotation: 0.0,
                start_ext: 0.0,
                end_ext: 0.0,
                join_start: BeamJoin::Auto,
                join_end: BeamJoin::Auto,
            })
            .unwrap();
        m.add_footing(&FootingSpec {
            level: l0,
            column: Some(c0),
            at: (0.0, 0.0),
            rotation: 0.0,
            round: false,
            steps: vec![FootingStep {
                w: 1.5,
                l: 1.5,
                h: 0.5,
            }],
            top_offset: 0.0,
            material: "Concrete".into(),
        })
        .unwrap();
        (m, [c0, c1, b])
    }

    #[test]
    fn portal_nodes_merge_and_footing_supports_the_column() {
        for zj in [Just::Center, Just::Pos, Just::Neg] {
            let (m, [c0, c1, _]) = portal(zj);
            let a = m.analytical().unwrap();
            assert_eq!(a.members.len(), 3);
            assert_eq!(a.nodes.len(), 4, "{zj:?}: {:?}", a.nodes);
            let beam = a.members.iter().find(|m| m.kind == "beam").unwrap();
            let col0 = a
                .members
                .iter()
                .find(|m| m.id == c0 && m.kind == "column")
                .unwrap();
            assert_eq!(col0.nodes[1], beam.nodes[0]);
            assert_eq!(a.supports.len(), 1);
            assert_eq!(a.supports[0].node, col0.nodes[0]);
            assert_eq!(a.supports[0].kind, Release::Fixed);
            // the second column has no footing: its base is a free end
            let col1 = a.members.iter().find(|m| m.id == c1).unwrap();
            assert!(a
                .issues
                .iter()
                .any(|q| q.code == "free_end" && q.id == c1 && q.node == Some(col1.nodes[0])));
            assert!(!a.issues.iter().any(|q| q.code == "unsupported"));
            assert_eq!(a.sections.len(), 2);
            let s = &a.sections[beam.section.unwrap()];
            assert!((s.area - 0.15).abs() < 1e-9);
            assert!((s.iy - 0.3 * 0.125 / 12.0).abs() < 1e-9);
            let j = ddc_geom::section_props::rect_torsion(0.3, 0.5);
            assert!((s.props.j - j).abs() < 1e-12 * j);
            assert!((s.props.wy - 0.3 * 0.25 / 6.0).abs() < 1e-12);
            assert!((s.props.avz - 0.15 * 5.0 / 6.0).abs() < 1e-9);
            let col = &a.sections[col0.section.unwrap()];
            assert!((col.props.wpl_z - 0.4f64.powi(3) / 4.0).abs() < 1e-9);
        }
    }

    #[test]
    fn releases_and_offsets_are_one_undo_step_and_exported() {
        let (mut m, [_, _, b]) = portal(Just::Center);
        m.set_analytical_adj(
            "beam",
            &[b],
            AnalyticalAdj {
                release_start: Some(Release::Pinned),
                ..Default::default()
            },
        )
        .unwrap();
        let a = m.analytical().unwrap();
        let beam = a.members.iter().find(|m| m.kind == "beam").unwrap();
        assert_eq!(beam.release, (Release::Pinned, Release::Fixed));
        let j: J = serde_json::from_str(&m.analytical_json().unwrap()).unwrap();
        assert_eq!(j["nodes"].as_array().unwrap().len(), 4);
        assert_eq!(j["supports"][0]["type"], "fixed");
        assert!(j["members"]
            .as_array()
            .unwrap()
            .iter()
            .any(|x| x["release_start"] == "pinned"));
        assert_eq!(j["materials"][0]["name"], "Concrete");
        let sec = &j["sections"][0];
        assert!(sec["j"].as_f64().unwrap() > 0.0 && sec["avz"].as_f64().unwrap() > 0.0);
        assert_eq!(sec["shear_center"].as_array().unwrap().len(), 2);
        assert_eq!(j["units"]["modulus"], "m3");
        m.undo().unwrap();
        assert!(m.analytical_adj("beam", b).unwrap().is_none());
        // an end offset beyond the merge tolerance separates the node
        m.set_analytical_adj(
            "beam",
            &[b],
            AnalyticalAdj {
                start: (0.0, 0.0, -1.0),
                ..Default::default()
            },
        )
        .unwrap();
        let a = m.analytical().unwrap();
        let beam = a.members.iter().find(|m| m.kind == "beam").unwrap();
        assert!((a.nodes[beam.nodes[0]].z - 2.0).abs() < 1e-6);
        // the beam end now lands on the column axis: the column is split there
        assert!(a
            .members
            .iter()
            .any(|m| m.kind == "column" && m.nodes.len() == 3));
    }

    #[test]
    fn analytical_model_is_cached_until_the_file_changes() {
        let (mut m, [_, _, b]) = portal(Just::Center);
        let a = m.analytical().unwrap();
        assert!(Arc::ptr_eq(&a, &m.analytical().unwrap()));
        m.set_analytical_adj(
            "beam",
            &[b],
            AnalyticalAdj {
                release_end: Some(Release::Pinned),
                ..Default::default()
            },
        )
        .unwrap();
        let a2 = m.analytical().unwrap();
        assert!(!Arc::ptr_eq(&a, &a2));
        let beam = a2.members.iter().find(|m| m.kind == "beam").unwrap();
        assert_eq!(beam.release.1, Release::Pinned);
        assert_eq!(m.analytical_with(Tolerances::default()).unwrap(), *a2);
    }

    #[test]
    fn deleting_an_element_drops_its_adjustment() {
        let (mut m, [_, _, b]) = portal(Just::Center);
        m.set_analytical_adj(
            "beam",
            &[b],
            AnalyticalAdj {
                release_end: Some(Release::Pinned),
                ..Default::default()
            },
        )
        .unwrap();
        m.delete_beam(b).unwrap();
        assert!(m.analytical_adj("beam", b).unwrap().is_none());
        m.undo().unwrap();
        assert!(m.analytical_adj("beam", b).unwrap().is_some());
    }
}
