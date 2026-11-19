//! Columns and beams: section types, instances, joints and deductions (schema 13).
//! Beam joints (in regeneration, not in the journal): an end at a column is cut at the column face; a shared
//! end of two beams is a mitre (or butt: one runs to the side face of the other, the other is extended);
//! an end on another beam's axis runs to its side face (cope: to the web with flanges notched).
//! Deductions: a wall goes around a column (layer volume - intersection), a structural column cuts the
//! slab, a slab cuts an architectural column, a slab cuts a beam (the part of the section within the slab).
//! Inclined columns and beams and arc beams do not take part in deductions.

use std::collections::HashMap;

use ddc_geom::member::{catalog, miter, Cap, Just, Member, Path, Shape};
use ddc_geom::mesh::Mesh;
use ddc_geom::{poly, v2, v3, V2, V3};
use rusqlite::{params, Connection, OptionalExtension};

use crate::ops::Inv;
use crate::{Error, Issue, Model};

#[derive(Clone, Debug, PartialEq)]
pub struct ColumnSpec {
    pub level: i64,
    /// Top level; None means the bottom level.
    pub top_level: Option<i64>,
    pub member_type: i64,
    pub structural: bool,
    pub at: (f64, f64),
    pub base_offset: f64,
    /// Top = top level elevation + top_offset.
    pub top_offset: f64,
    /// Section rotation, rad.
    pub rotation: f64,
    /// Top point of an inclined column; None means vertical.
    pub top_at: Option<(f64, f64)>,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum BeamJoin {
    #[default]
    Auto,
    Butt,
    Miter,
    Cope,
    None,
}

impl BeamJoin {
    pub fn text(self) -> &'static str {
        match self {
            BeamJoin::Auto => "auto",
            BeamJoin::Butt => "butt",
            BeamJoin::Miter => "miter",
            BeamJoin::Cope => "cope",
            BeamJoin::None => "none",
        }
    }
    pub fn parse(s: &str) -> BeamJoin {
        match s {
            "butt" => BeamJoin::Butt,
            "miter" => BeamJoin::Miter,
            "cope" => BeamJoin::Cope,
            "none" => BeamJoin::None,
            _ => BeamJoin::Auto,
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct BeamSpec {
    pub level: i64,
    pub member_type: i64,
    /// Ends of the support line; z is from the level elevation.
    pub a: (f64, f64, f64),
    pub b: (f64, f64, f64),
    /// tan(angle/4) of the arc in plan (0 is straight); the arc is horizontal.
    pub bulge: f64,
    pub yjust: Just,
    pub zjust: Just,
    pub rotation: f64,
    /// Extension (+) / shortening (-) of the ends along the axis up to joints.
    pub start_ext: f64,
    pub end_ext: f64,
    pub join_start: BeamJoin,
    pub join_end: BeamJoin,
}

/// Member quantities: net volume = gross - deductions.
#[derive(Clone, Debug, PartialEq)]
pub struct MemberQuantity {
    pub length: f64,
    pub volume: f64,
    pub cut: f64,
    pub surface: f64,
    pub material: String,
}

fn invalid<T>(m: String) -> Result<T, Error> {
    Err(Error::Invalid(m))
}

fn exists(db: &Connection, table: &str, id: i64) -> Result<bool, Error> {
    Ok(db.query_row(
        &format!("SELECT count(*) FROM {table} WHERE id=?1"),
        [id],
        |r| r.get::<_, i64>(0),
    )? > 0)
}

fn elevation(db: &Connection, level: i64) -> Result<f64, Error> {
    db.query_row("SELECT elevation FROM level WHERE id=?1", [level], |r| {
        r.get(0)
    })
    .map_err(|_| Error::Invalid(format!("no such level {level}")))
}

pub(crate) fn check_column(db: &Connection, s: &ColumnSpec) -> Result<(), Error> {
    if !exists(db, "member_type", s.member_type)? {
        return invalid(format!("column: no member type {}", s.member_type));
    }
    let base = elevation(db, s.level)? + s.base_offset;
    let top = elevation(db, s.top_level.unwrap_or(s.level))? + s.top_offset;
    let fin = [s.at.0, s.at.1, s.base_offset, s.top_offset, s.rotation]
        .iter()
        .chain(s.top_at.iter().flat_map(|p| [&p.0, &p.1]))
        .all(|v| v.is_finite());
    if !fin || top - base < 1e-6 {
        return invalid(format!("column: top {top} must be above base {base}"));
    }
    Ok(())
}

pub(crate) fn check_beam(db: &Connection, s: &BeamSpec) -> Result<(), Error> {
    if !exists(db, "member_type", s.member_type)? {
        return invalid(format!("beam: no member type {}", s.member_type));
    }
    elevation(db, s.level)?;
    let v = [
        s.a.0,
        s.a.1,
        s.a.2,
        s.b.0,
        s.b.1,
        s.b.2,
        s.bulge,
        s.rotation,
        s.start_ext,
        s.end_ext,
    ];
    let d = ((s.b.0 - s.a.0).powi(2) + (s.b.1 - s.a.1).powi(2) + (s.b.2 - s.a.2).powi(2)).sqrt();
    if !v.iter().all(|x| x.is_finite()) || d < 1e-6 {
        return invalid("beam: zero length or non-finite".into());
    }
    if s.bulge != 0.0 && ((s.a.2 - s.b.2).abs() > 1e-9 || s.bulge.abs() > 1.0) {
        return invalid("beam arc: ends at one height, |bulge| <= 1".into());
    }
    if d + s.start_ext + s.end_ext < 1e-6 && s.bulge == 0.0 {
        return invalid("beam: extensions consume the whole length".into());
    }
    Ok(())
}

fn col_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, ColumnSpec)> {
    let tx: Option<f64> = r.get(10)?;
    let ty: Option<f64> = r.get(11)?;
    Ok((
        r.get(0)?,
        ColumnSpec {
            level: r.get(1)?,
            top_level: r.get(2)?,
            member_type: r.get(3)?,
            structural: r.get::<_, i64>(4)? != 0,
            at: (r.get(5)?, r.get(6)?),
            base_offset: r.get(7)?,
            top_offset: r.get(8)?,
            rotation: r.get(9)?,
            top_at: tx.zip(ty),
        },
    ))
}

const COL_SEL: &str =
    "SELECT id,level,top_level,mtype,structural,x,y,base_offset,top_offset,rotation,tx,ty FROM col";
const BEAM_SEL: &str = "SELECT id,level,mtype,ax,ay,az,bx,by,bz,bulge,yjust,zjust,rotation,start_ext,end_ext,join_start,join_end FROM beam";

fn beam_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, BeamSpec)> {
    Ok((
        r.get(0)?,
        BeamSpec {
            level: r.get(1)?,
            member_type: r.get(2)?,
            a: (r.get(3)?, r.get(4)?, r.get(5)?),
            b: (r.get(6)?, r.get(7)?, r.get(8)?),
            bulge: r.get(9)?,
            yjust: Just::from_i(r.get(10)?),
            zjust: Just::from_i(r.get(11)?),
            rotation: r.get(12)?,
            start_ext: r.get(13)?,
            end_ext: r.get(14)?,
            join_start: BeamJoin::parse(&r.get::<_, String>(15)?),
            join_end: BeamJoin::parse(&r.get::<_, String>(16)?),
        },
    ))
}

pub(crate) fn insert_column(
    db: &Connection,
    s: &ColumnSpec,
    id: Option<i64>,
) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO col(id,level,top_level,mtype,structural,x,y,base_offset,top_offset,rotation,tx,ty) \
         VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12)",
        params![
            id,
            s.level,
            s.top_level,
            s.member_type,
            s.structural as i64,
            s.at.0,
            s.at.1,
            s.base_offset,
            s.top_offset,
            s.rotation,
            s.top_at.map(|p| p.0),
            s.top_at.map(|p| p.1)
        ],
    )?;
    Ok(db.last_insert_rowid())
}

pub(crate) fn insert_beam(db: &Connection, s: &BeamSpec, id: Option<i64>) -> Result<i64, Error> {
    db.execute(
        "INSERT OR REPLACE INTO beam(id,level,mtype,ax,ay,az,bx,by,bz,bulge,yjust,zjust,rotation,\
         start_ext,end_ext,join_start,join_end) VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17)",
        params![
            id,
            s.level,
            s.member_type,
            s.a.0,
            s.a.1,
            s.a.2,
            s.b.0,
            s.b.1,
            s.b.2,
            s.bulge,
            s.yjust.to_i(),
            s.zjust.to_i(),
            s.rotation,
            s.start_ext,
            s.end_ext,
            s.join_start.text(),
            s.join_end.text()
        ],
    )?;
    Ok(db.last_insert_rowid())
}

/// A member after joints: kind, id, level, body, volume deduction.
pub(crate) struct Built {
    pub kind: &'static str,
    pub id: i64,
    pub level: i64,
    pub m: Member,
    pub cut: f64,
    pub structural: bool,
}

fn types(db: &Connection) -> Result<HashMap<i64, (Shape, String)>, Error> {
    let mut st = db.prepare("SELECT id, shape, material FROM member_type")?;
    let rows = st.query_map([], |r| {
        Ok((
            r.get::<_, i64>(0)?,
            r.get::<_, String>(1)?,
            r.get::<_, String>(2)?,
        ))
    })?;
    let mut out = HashMap::new();
    for r in rows {
        let (id, s, mat) = r?;
        let sh = Shape::parse(&s)
            .ok_or_else(|| Error::Geom(format!("member type {id}: bad shape '{s}'")))?;
        out.insert(id, (sh, mat));
    }
    Ok(out)
}

fn levels(db: &Connection) -> Result<HashMap<i64, f64>, Error> {
    let mut st = db.prepare("SELECT id, elevation FROM level")?;
    let r = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(r.collect::<Result<_, _>>()?)
}

fn column_member(sh: Shape, s: &ColumnSpec, lv: &HashMap<i64, f64>) -> Member {
    let z0 = lv.get(&s.level).copied().unwrap_or(0.0) + s.base_offset;
    let z1 = lv
        .get(&s.top_level.unwrap_or(s.level))
        .copied()
        .unwrap_or(0.0)
        + s.top_offset;
    let t = s.top_at.unwrap_or(s.at);
    Member {
        shape: sh,
        path: Path::Line {
            a: v3(s.at.0, s.at.1, z0),
            b: v3(t.0, t.1, z1),
        },
        rotation: s.rotation,
        yjust: Just::Center,
        zjust: Just::Center,
        column: true,
        start: None,
        end: None,
    }
}

fn beam_member(sh: Shape, s: &BeamSpec, lv: &HashMap<i64, f64>) -> Member {
    let e = lv.get(&s.level).copied().unwrap_or(0.0);
    let (mut a, mut b) = (v3(s.a.0, s.a.1, e + s.a.2), v3(s.b.0, s.b.1, e + s.b.2));
    let path = if s.bulge != 0.0 {
        Path::Arc {
            a,
            b,
            bulge: s.bulge,
        }
    } else {
        let u = (b - a).normalized();
        a = a - u * s.start_ext;
        b = b + u * s.end_ext;
        Path::Line { a, b }
    };
    Member {
        shape: sh,
        path,
        rotation: s.rotation,
        yjust: s.yjust,
        zjust: s.zjust,
        column: false,
        start: None,
        end: None,
    }
}

fn ends(m: &Member) -> (V3, V3) {
    match m.path {
        Path::Line { a, b } | Path::Arc { a, b, .. } => (a, b),
    }
}

fn flat(p: V3) -> V2 {
    v2(p.x, p.y)
}

/// The face of beam `j` on the side of point `body` (the y-face of the extents, or the web if `web`).
fn side_face(j: &Member, p: V3, body: V3, web: bool) -> Option<(Cap, f64)> {
    let u = j.axis_dir()?;
    let uh = v3(u.x, u.y, 0.0);
    if uh.len() < 1e-9 {
        return None;
    }
    let uh = uh.normalized();
    let ey = v3(-uh.y, uh.x, 0.0);
    let (lo, hi) = j.extents();
    let s = if (body - p).dot(ey) >= 0.0 { 1.0 } else { -1.0 };
    let (a, _) = ends(j);
    let q = a + uh * (p - a).dot(uh);
    let mid = (lo.x + hi.x) / 2.0;
    let half = (hi.x - lo.x) / 2.0;
    let off = if web {
        let (tw, _) = j.shape.web_flange()?;
        mid + s * tw / 2.0
    } else {
        mid + s * half
    };
    let f = v3(q.x + ey.x * off, q.y + ey.y * off, p.z);
    Some((
        Cap { p: f, n: ey },
        half - j.shape.web_flange().map_or(half, |w| w.0 / 2.0),
    ))
}

/// A flat cell grid for neighbour search (element extents).
struct Grid {
    cell: f64,
    map: HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn new(cell: f64) -> Self {
        Grid {
            cell,
            map: HashMap::new(),
        }
    }
    fn range(&self, b: [f64; 4]) -> (i64, i64, i64, i64) {
        let f = |v: f64| (v / self.cell).floor() as i64;
        (f(b[0]), f(b[1]), f(b[2]), f(b[3]))
    }
    fn insert(&mut self, b: [f64; 4], v: usize) {
        let (x0, y0, x1, y1) = self.range(b);
        for x in x0..=x1.min(x0 + 64) {
            for y in y0..=y1.min(y0 + 64) {
                self.map.entry((x, y)).or_default().push(v);
            }
        }
    }
    fn near(&self, p: V2) -> &[usize] {
        let (x, y, _, _) = self.range([p.x, p.y, p.x, p.y]);
        self.map.get(&(x, y)).map_or(&[], |v| v.as_slice())
    }
}

/// Beam joints: ends against columns and each other; flange notch deduction.
fn resolve_joins(built: &mut [Built], beams: &[(i64, BeamSpec)]) {
    let cols: Vec<(Vec<V2>, f64, f64)> = built
        .iter()
        .filter(|b| b.kind == "column")
        .filter_map(|b| {
            let (a, t) = ends(&b.m);
            if flat(a) != flat(t) {
                return None;
            }
            let r = b.m.plan_section((a.z + t.z) / 2.0)?;
            Some((r, a.z, t.z))
        })
        .collect();
    let mut cgrid = Grid::new(4.0);
    for (i, c) in cols.iter().enumerate() {
        cgrid.insert(bbox(&c.0), i);
    }
    let first_beam = built
        .iter()
        .position(|b| b.kind == "beam")
        .unwrap_or(built.len());
    let snapshot: Vec<Member> = built[first_beam..].iter().map(|b| b.m.clone()).collect();
    let tol = 1e-6;
    let key = |p: V3| {
        (
            (p.x / tol).round() as i64,
            (p.y / tol).round() as i64,
            (p.z / tol).round() as i64,
        )
    };
    let mut at_end: HashMap<(i64, i64, i64), Vec<(usize, bool)>> = HashMap::new();
    let mut bgrid = Grid::new(4.0);
    for (j, om) in snapshot.iter().enumerate() {
        if om.axis_dir().is_none() {
            continue;
        }
        let (oa, ob) = ends(om);
        at_end.entry(key(oa)).or_default().push((j, true));
        at_end.entry(key(ob)).or_default().push((j, false));
        let (w, _) = om.shape.size();
        let b = bbox(&[flat(oa), flat(ob)]);
        bgrid.insert([b[0] - w, b[1] - w, b[2] + w, b[3] + w], j);
    }
    for (i, (_, spec)) in beams.iter().enumerate() {
        let me = &snapshot[i];
        let Some(u) = me.axis_dir() else { continue };
        let (a, b) = ends(me);
        let mut cut = 0.0;
        let mut caps: [Option<Cap>; 2] = [None, None];
        for (k, (p, body, mode)) in [(a, b, spec.join_start), (b, a, spec.join_end)]
            .into_iter()
            .enumerate()
        {
            if mode == BeamJoin::None {
                continue;
            }
            // column
            let host = cgrid
                .near(flat(p))
                .iter()
                .map(|&c| &cols[c])
                .find(|(r, z0, z1)| {
                    p.z >= z0 - tol
                        && p.z <= z1 + tol
                        && ddc_geom::predicates::point_in_ring(flat(p), r) >= 0
                });
            if let Some((ring, _, _)) = host {
                let iv = poly::inside_intervals(flat(a), flat(b), std::slice::from_ref(ring));
                let t = if k == 0 {
                    iv.iter().find(|w| w.0 <= 1e-9).map(|w| w.1)
                } else {
                    iv.iter().find(|w| w.1 >= 1.0 - 1e-9).map(|w| w.0)
                };
                if let Some(t) = t {
                    let q = a + (b - a) * t;
                    caps[k] = Some(Cap {
                        p: q,
                        n: if k == 0 { -u } else { u },
                    });
                    continue;
                }
            }
            // shared end of another beam
            let fwd = if k == 0 { -u } else { u }; // towards the joint
            let other_end = at_end
                .get(&key(p))
                .and_then(|v| v.iter().find(|(j, _)| *j != i))
                .copied();
            if let Some((j, at_start)) = other_end {
                let om = &snapshot[j];
                let Some(ou) = om.axis_dir() else {
                    continue;
                };
                let omode = if at_start {
                    beams[j].1.join_start
                } else {
                    beams[j].1.join_end
                };
                let o_away = if at_start { ou } else { -ou };
                caps[k] = if mode == BeamJoin::Butt
                    || (mode == BeamJoin::Cope && omode != BeamJoin::Butt)
                {
                    side_face(om, p, body, false).map(|c| c.0)
                } else if omode == BeamJoin::Butt || omode == BeamJoin::Cope {
                    let (w, _) = om.shape.size();
                    Some(Cap {
                        p: p + fwd * (w / 2.0),
                        n: fwd,
                    })
                } else {
                    Some(miter(p, fwd, o_away))
                };
                continue;
            }
            // end on another beam's axis (T)
            for &j in bgrid.near(flat(p)) {
                if j == i {
                    continue;
                }
                let om = &snapshot[j];
                let (Some(ou), (oa, ob)) = (om.axis_dir(), ends(om)) else {
                    continue;
                };
                let d = p - oa;
                let along = d.dot(ou);
                let perp = d - ou * along;
                let (lo, hi) = om.extents();
                if along <= tol
                    || along >= (ob - oa).len() - tol
                    || v3(perp.x, perp.y, 0.0).len() > tol
                    || perp.z < lo.y - tol
                    || perp.z > hi.y + tol
                {
                    continue;
                }
                let web = mode == BeamJoin::Cope && om.shape.web_flange().is_some();
                if let Some((cap, flange)) = side_face(om, p, body, web) {
                    caps[k] = Some(cap);
                    if web {
                        let (_, tf) = om.shape.web_flange().unwrap();
                        let (z0, z1) = (oa.z + lo.y + tf, oa.z + hi.y - tf);
                        if let Some(inside) = me.band_area(z0, z1) {
                            cut += flange * (me.shape.area() - inside);
                        }
                    }
                }
                break;
            }
        }
        let bm = &mut built[first_beam + i];
        bm.m.start = caps[0].or(bm.m.start);
        bm.m.end = caps[1].or(bm.m.end);
        bm.cut += cut;
    }
}

/// All members with joints (columns, then beams by id).
pub(crate) fn build_all(db: &Connection) -> Result<Vec<Built>, Error> {
    let ty = types(db)?;
    let lv = levels(db)?;
    let mut out = vec![];
    let mut st = db.prepare(&format!("{COL_SEL} ORDER BY id"))?;
    let cols: Vec<(i64, ColumnSpec)> = st.query_map([], col_row)?.collect::<Result<_, _>>()?;
    for (id, s) in cols {
        let (sh, _) = ty
            .get(&s.member_type)
            .cloned()
            .ok_or_else(|| Error::Geom(format!("column {id}: no type")))?;
        out.push(Built {
            kind: "column",
            id,
            level: s.level,
            m: column_member(sh, &s, &lv),
            cut: 0.0,
            structural: s.structural,
        });
    }
    let mut st = db.prepare(&format!("{BEAM_SEL} ORDER BY id"))?;
    let beams: Vec<(i64, BeamSpec)> = st.query_map([], beam_row)?.collect::<Result<_, _>>()?;
    for (id, s) in &beams {
        let (sh, _) = ty
            .get(&s.member_type)
            .cloned()
            .ok_or_else(|| Error::Geom(format!("beam {id}: no type")))?;
        out.push(Built {
            kind: "beam",
            id: *id,
            level: s.level,
            m: beam_member(sh, s, &lv),
            cut: 0.0,
            structural: true,
        });
    }
    resolve_joins(&mut out, &beams);
    Ok(out)
}

/// Slab: id, rings, top, layer thicknesses (top to bottom); inclined ones are skipped. `only`
/// restricts to these slabs (by ascending id).
pub(crate) fn flat_slabs(
    db: &Connection,
    only: Option<&std::collections::BTreeSet<i64>>,
) -> Result<Vec<FlatSlab>, Error> {
    let rows: Vec<(i64, f64)> = match only {
        None => {
            let mut st = db.prepare(
                "SELECT s.id, l.elevation + s.offset FROM slab s JOIN level l ON l.id = s.level \
                 WHERE s.slope IS NULL ORDER BY s.id",
            )?;
            let v = st
                .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?
                .collect::<Result<_, _>>()?;
            v
        }
        Some(ids) => {
            let mut st = db.prepare_cached(
                "SELECT l.elevation + s.offset FROM slab s JOIN level l ON l.id = s.level \
                 WHERE s.slope IS NULL AND s.id=?1",
            )?;
            let mut v = vec![];
            for &id in ids {
                if let Some(top) = st.query_row([id], |r| r.get::<_, f64>(0)).optional()? {
                    v.push((id, top));
                }
            }
            v
        }
    };
    let mut out = vec![];
    for (id, top) in rows {
        let rings: Vec<Vec<V2>> = crate::slabs::rings(db, id)?
            .into_iter()
            .map(|r| r.into_iter().map(|p| v2(p.0, p.1)).collect())
            .collect();
        let mut ls = db.prepare_cached(
            "SELECT l.thickness FROM slab s JOIN slab_type_layer l ON l.stype = s.stype WHERE s.id=?1 ORDER BY l.pos",
        )?;
        let th: Vec<f64> = ls
            .query_map([id], |r| r.get(0))?
            .collect::<Result<_, _>>()?;
        out.push((id, rings, top, th));
    }
    Ok(out)
}

fn z_overlap(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.1.min(b.1) - a.0.max(b.0)).max(0.0)
}

pub(crate) fn bbox(r: &[V2]) -> [f64; 4] {
    r.iter()
        .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, p| {
            [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)]
        })
}

pub(crate) fn boxes_meet(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// A vertical column: convex section pieces, area correction (circle), extents, z.
pub(crate) fn column_plan(m: &Member) -> Option<ColPlan> {
    let (a, b) = ends(m);
    if flat(a) != flat(b) {
        return None;
    }
    let sec = m.plan_section((a.z + b.z) / 2.0)?;
    // circle/pipe: the outer-outline polygon is scaled to the exact section area
    let ratio = m.shape.area() / poly::area(&sec).abs().max(1e-300);
    Some((poly::convex_pieces(&sec), ratio, bbox(&sec), (a.z, b.z)))
}

/// Vertical columns: (plan section, bottom, top, load-bearing), a deduction for voids.
pub(crate) fn column_prisms(db: &Connection) -> Result<Vec<(Vec<V2>, f64, f64, bool)>, Error> {
    let n: i64 = db.query_row("SELECT count(*) FROM col", [], |r| r.get(0))?;
    if n == 0 {
        return Ok(vec![]);
    }
    let built = match build_all(db) {
        Ok(b) => b,
        Err(Error::Geom(_)) => return Ok(vec![]),
        Err(e) => return Err(e),
    };
    Ok(built
        .iter()
        .filter(|b| b.kind == "column")
        .filter_map(|b| {
            let (a, e) = ends(&b.m);
            if flat(a) != flat(e) {
                return None;
            }
            let sec = b.m.plan_section((a.z + e.z) / 2.0)?;
            Some((sec, a.z.min(e.z), a.z.max(e.z), b.structural))
        })
        .collect())
}

fn region_overlap(rings: &[Vec<V2>], pieces: &[Vec<V2>]) -> f64 {
    let pb =
        pieces
            .iter()
            .map(|p| bbox(p))
            .fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, b| {
                [
                    a[0].min(b[0]),
                    a[1].min(b[1]),
                    a[2].max(b[2]),
                    a[3].max(b[3]),
                ]
            });
    // rings that miss the section box add nothing; a section box inside the outer ring and clear
    // of its edges (no hole near) overlaps whole — no clipping (hundreds of members per edit)
    let near: Vec<usize> = (0..rings.len())
        .filter(|&i| boxes_meet(bbox(&rings[i]), pb))
        .collect();
    if near == [0]
        && !ring_hits_box(&rings[0], pb)
        && poly::contains(
            &rings[..1],
            v2((pb[0] + pb[2]) / 2.0, (pb[1] + pb[3]) / 2.0),
        )
    {
        return pieces.iter().map(|p| poly::area(p)).sum::<f64>().abs();
    }
    near.iter()
        .map(|&i| poly::overlap_area(&rings[i], pieces))
        .sum::<f64>()
        .abs()
}

/// Some edge of `r` touches the box (Liang–Barsky clip of each edge).
fn ring_hits_box(r: &[V2], b: [f64; 4]) -> bool {
    (0..r.len()).any(|i| {
        let (p, q) = (r[i], r[(i + 1) % r.len()]);
        let d = q - p;
        let (mut t0, mut t1) = (0.0f64, 1.0f64);
        for (den, num) in [
            (-d.x, p.x - b[0]),
            (d.x, b[2] - p.x),
            (-d.y, p.y - b[1]),
            (d.y, b[3] - p.y),
        ] {
            if den == 0.0 {
                if num < 0.0 {
                    return false;
                }
            } else {
                let t = num / den;
                if den < 0.0 {
                    t0 = t0.max(t);
                } else {
                    t1 = t1.min(t);
                }
            }
        }
        t0 <= t1
    })
}

/// Plan section of a vertical column: convex pieces, area correction, extents, z.
pub(crate) type ColPlan = (Vec<Vec<V2>>, f64, [f64; 4], (f64, f64));
/// A flat slab: id, rings, top, layer thicknesses (top to bottom).
pub(crate) type FlatSlab = (i64, Vec<Vec<V2>>, f64, Vec<f64>);
/// Extents of the slab's outer outline, and the bottom and top of its thickness.
pub(crate) type SlabBox = ([f64; 4], f64, f64);

pub(crate) fn slab_box(s: &FlatSlab) -> Option<SlabBox> {
    let bb = bbox(s.1.first()?);
    Some((bb, s.2 - s.3.iter().sum::<f64>(), s.2))
}

/// Member quantities; returns the built columns and beams (for schema 26 structure).
/// `walls` are wall layers, `rewritten` are slabs with just-recomputed quantities:
/// the column deduction goes only into fresh rows (a repeated recompute does not accumulate it). The second
/// value is joint deductions per member (before slabs), the third is flat slab extents, if they were read
/// (for the partial-recompute index).
pub(crate) fn regenerate(
    db: &Connection,
    walls: &[&[crate::walls::WallLayer]],
    rewritten: &std::collections::HashSet<i64>,
) -> Result<(Vec<Built>, Vec<f64>, Option<Vec<(i64, SlabBox)>>), Error> {
    db.execute("DELETE FROM member_qty", [])?;
    let has: i64 = db.query_row(
        "SELECT (SELECT count(*) FROM col) + (SELECT count(*) FROM beam)",
        [],
        |r| r.get(0),
    )?;
    if has == 0 {
        return Ok((vec![], vec![], None));
    }
    let mut built = build_all(db)?;
    let joins: Vec<f64> = built.iter().map(|b| b.cut).collect();
    let slabs = flat_slabs(db, None)?;
    let plans: Vec<(usize, ColPlan)> = built
        .iter()
        .enumerate()
        .filter(|(_, b)| b.kind == "column")
        .filter_map(|(i, b)| column_plan(&b.m).map(|p| (i, p)))
        .collect();
    let plans: Vec<(usize, &ColPlan)> = plans.iter().map(|(i, p)| (*i, p)).collect();
    let sub: Vec<&FlatSlab> = slabs.iter().filter(|s| rewritten.contains(&s.0)).collect();
    let all: Vec<&FlatSlab> = slabs.iter().collect();
    apply(db, &mut built, &plans, walls, &sub, &all, None)?;
    let boxes = slabs
        .iter()
        .filter_map(|s| slab_box(s).map(|b| (s.0, b)))
        .collect();
    Ok((built, joins, Some(boxes)))
}

/// Deductions and `member_qty` over the recompute area. `plans` are all vertical columns (index into
/// `built`), `walls` are wall layers with fresh quantities, `sub` are slabs with fresh
/// quantities (load-bearing column deduction), `cut` are slabs for deductions of `members` (None means
/// all members; their rows are rewritten). Traversal order is as in a full recompute (by id).
pub(crate) fn apply(
    db: &Connection,
    built: &mut [Built],
    plans: &[(usize, &ColPlan)],
    walls: &[&[crate::walls::WallLayer]],
    sub: &[&FlatSlab],
    cut: &[&FlatSlab],
    members: Option<&std::collections::HashSet<(&'static str, i64)>>,
) -> Result<(), Error> {
    let redo = |b: &Built| members.map_or(true, |s| s.contains(&(b.kind, b.id)));
    // columns and slabs
    for &(i, (pieces, ratio, bb, zr)) in plans {
        let structural = built[i].structural;
        let list = if structural {
            sub
        } else if redo(&built[i]) {
            cut
        } else {
            continue;
        };
        for s in list {
            let (sid, rings, top, th) = (s.0, &s.1, s.2, &s.3);
            let total: f64 = th.iter().sum();
            if z_overlap(*zr, (top - total, top)) <= 0.0 || !boxes_meet(*bb, bbox(&rings[0])) {
                continue;
            }
            let area = region_overlap(rings, pieces) * ratio;
            if area <= 0.0 {
                continue;
            }
            let mut z = top;
            for (pos, t) in th.iter().enumerate() {
                let v = area * z_overlap(*zr, (z - t, z));
                z -= t;
                if v <= 0.0 {
                    continue;
                }
                if structural {
                    db.prepare_cached(
                        "UPDATE slab_qty SET volume = volume - ?3 WHERE slab=?1 AND pos=?2",
                    )?
                    .execute(params![sid, pos as i64, v])?;
                } else {
                    built[i].cut += v;
                }
            }
        }
    }
    // walls go around columns
    if !plans.is_empty() {
        for (wall, pos, rings, z0, z1) in walls.iter().copied().flatten() {
            let wb = rings.iter().map(|r| bbox(r)).fold(
                [f64::MAX, f64::MAX, f64::MIN, f64::MIN],
                |a, b| {
                    [
                        a[0].min(b[0]),
                        a[1].min(b[1]),
                        a[2].max(b[2]),
                        a[3].max(b[3]),
                    ]
                },
            );
            for &(_, (pieces, ratio, bb, zr)) in plans {
                if !boxes_meet(wb, *bb) {
                    continue;
                }
                let v = region_overlap(rings, pieces) * ratio * z_overlap(*zr, (*z0, *z1));
                if v > 0.0 {
                    db.prepare_cached(
                        "UPDATE wall_qty SET volume = volume - ?3 WHERE wall=?1 AND pos=?2",
                    )?
                    .execute(params![wall, pos, v])?;
                }
            }
        }
    }
    // beams within slab thickness
    for b in built.iter_mut().filter(|b| b.kind == "beam") {
        if !redo(&*b) {
            continue;
        }
        let Some((p, q)) = b.m.axis_ends() else {
            continue;
        };
        let l = (q - p).len();
        let seg_bb = bbox(&[flat(p), flat(q)]);
        for s in cut {
            let (rings, top, th) = (&s.1, s.2, &s.3);
            if !boxes_meet(seg_bb, bbox(&rings[0])) {
                continue;
            }
            let t: f64 = th.iter().sum();
            let Some(band) = b.m.band_area(top - t, top) else {
                continue;
            };
            if band <= 0.0 {
                continue;
            }
            let inside: f64 = poly::inside_intervals(flat(p), flat(q), rings)
                .iter()
                .map(|w| (w.1 - w.0) * l)
                .sum();
            b.cut += band * inside;
        }
    }
    let mut ins = db.prepare_cached("INSERT INTO member_qty VALUES (?1,?2,?3,?4,?5,?6)")?;
    for b in built.iter() {
        if !redo(b) {
            continue;
        }
        if members.is_some() {
            db.prepare_cached("DELETE FROM member_qty WHERE kind=?1 AND ref=?2")?
                .execute(params![b.kind, b.id])?;
        }
        let q = b.m.quantities();
        // overlapping slabs or columns deduct the same stretch twice: never more than the member has
        let cut = b.cut.min(q.volume.max(0.0));
        ins.execute(params![b.kind, b.id, q.length, q.volume - cut, q.surface, cut])?;
    }
    Ok(())
}

/// Plan extents of a beam axis (to find slabs for deduction).
pub(crate) fn beam_box(m: &Member) -> Option<[f64; 4]> {
    let (p, q) = m.axis_ends()?;
    Some(bbox(&[flat(p), flat(q)]))
}

pub(crate) fn issues(db: &Connection) -> Result<Vec<Issue>, Error> {
    let mut out = vec![];
    for (table, kind) in [("col", "column"), ("beam", "beam")] {
        let mut st = db.prepare(&format!(
            "SELECT id FROM {table} WHERE id NOT IN (SELECT ref FROM member_qty WHERE kind='{kind}') ORDER BY id"
        ))?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            out.push(Issue {
                table: table.into(),
                id: id?,
                problem: format!("{kind} without quantities"),
            });
        }
        let mut st = db.prepare(&format!(
            "SELECT rowid FROM member_qty WHERE kind='{kind}' AND ref NOT IN (SELECT id FROM {table})"
        ))?;
        for id in st.query_map([], |r| r.get::<_, i64>(0))? {
            out.push(Issue {
                table: "member_qty".into(),
                id: id?,
                problem: format!("ref -> missing {table}"),
            });
        }
    }
    let mut st = db.prepare("SELECT id, shape FROM member_type ORDER BY id")?;
    for r in st.query_map([], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, String>(1)?)))? {
        let (id, s) = r?;
        if Shape::parse(&s)
            .map(|s| s.check())
            .is_none_or(|c| c.is_err())
        {
            out.push(Issue {
                table: "member_type".into(),
                id,
                problem: format!("bad shape '{s}'"),
            });
        }
    }
    out.extend(crate::roofs::issues(db)?);
    Ok(out)
}

impl Model {
    /// A member type: section + material.
    pub fn add_member_type(
        &mut self,
        name: &str,
        shape: &Shape,
        material: &str,
    ) -> Result<i64, Error> {
        shape.check().map_err(Error::Invalid)?;
        let text = shape.text();
        self.op("member_type_add", |db| {
            db.execute(
                "INSERT INTO member_type(name, shape, material) VALUES (?1,?2,?3)",
                params![name, text, material],
            )?;
            let id = db.last_insert_rowid();
            let mut inv = Inv::default().delete("member_type", "id", id);
            // the row that open would add from the text: a saved and reopened file stays row for row
            if !material.trim().is_empty() && crate::materials::row_id(db, material)?.is_none() {
                let spec = crate::materials::material_defaults(material);
                crate::materials::insert_spec_ignore(db, &spec)?;
                inv = inv.delete("material", "id", db.last_insert_rowid());
            }
            Ok((id, id, inv))
        })
    }

    /// A type from the size catalog (`ddc_geom::member::catalog`), material is steel.
    pub fn add_catalog_member_type(&mut self, name: &str) -> Result<i64, Error> {
        let found = catalog().into_iter().find(|c| c.0 == name).map(|c| c.1);
        let Some(s) = found.or_else(|| ddc_geom::steel::sections::find(name)) else {
            return invalid(format!("no catalog section '{name}'"));
        };
        self.add_member_type(name, &s, "Steel")
    }

    pub fn member_types(&self) -> Result<Vec<(i64, String, Shape, String)>, Error> {
        let mut st = self
            .db
            .prepare("SELECT id, name, shape, material FROM member_type ORDER BY id")?;
        let rows = st.query_map([], |r| {
            Ok((
                r.get::<_, i64>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })?;
        let mut out = vec![];
        for r in rows {
            let (id, n, s, m) = r?;
            if let Some(sh) = Shape::parse(&s) {
                out.push((id, n, sh, m));
            }
        }
        Ok(out)
    }

    pub fn delete_member_type(&mut self, id: i64) -> Result<(), Error> {
        self.delete_ref("member_type", "member_type_delete", id, &[])
    }

    pub fn add_column(&mut self, s: &ColumnSpec) -> Result<i64, Error> {
        Ok(self.add_columns(std::slice::from_ref(s))?[0])
    }

    /// Several columns in one operation (one undo).
    pub fn add_columns(&mut self, ss: &[ColumnSpec]) -> Result<Vec<i64>, Error> {
        for s in ss {
            check_column(&self.db, s)?;
        }
        self.op("col_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert_column(db, s, None)?;
                inv = inv.delete("col", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    pub fn update_column(&mut self, id: i64, s: &ColumnSpec) -> Result<(), Error> {
        check_column(&self.db, s)?;
        self.op("col_edit", |db| {
            if !exists(db, "col", id)? {
                return invalid(format!("no such column {id}"));
            }
            let inv = Inv::default().restore(db, "col", "id", id)?;
            insert_column(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_column(&mut self, id: i64) -> Result<(), Error> {
        self.op("col_delete", |db| {
            let inv = Inv::default().restore(db, "col", "id", id)?;
            if db.execute("DELETE FROM col WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such column {id}"));
            }
            Ok(((), id, inv))
        })
    }

    pub fn column(&self, id: i64) -> Result<ColumnSpec, Error> {
        Ok(self
            .db
            .query_row(&format!("{COL_SEL} WHERE id=?1"), [id], col_row)?
            .1)
    }

    pub fn add_beam(&mut self, s: &BeamSpec) -> Result<i64, Error> {
        Ok(self.add_beams(std::slice::from_ref(s))?[0])
    }

    pub fn add_beams(&mut self, ss: &[BeamSpec]) -> Result<Vec<i64>, Error> {
        for s in ss {
            check_beam(&self.db, s)?;
        }
        self.op("beam_add", |db| {
            let mut ids = vec![];
            let mut inv = Inv::default();
            for s in ss {
                let id = insert_beam(db, s, None)?;
                inv = inv.delete("beam", "id", id);
                ids.push(id);
            }
            let first = ids.first().copied().unwrap_or(0);
            Ok((ids, first, inv))
        })
    }

    pub fn update_beam(&mut self, id: i64, s: &BeamSpec) -> Result<(), Error> {
        check_beam(&self.db, s)?;
        self.op("beam_edit", |db| {
            if !exists(db, "beam", id)? {
                return invalid(format!("no such beam {id}"));
            }
            let inv = Inv::default().restore(db, "beam", "id", id)?;
            insert_beam(db, s, Some(id))?;
            Ok(((), id, inv))
        })
    }

    pub fn delete_beam(&mut self, id: i64) -> Result<(), Error> {
        self.op("beam_delete", |db| {
            let inv = Inv::default().restore(db, "beam", "id", id)?;
            if db.execute("DELETE FROM beam WHERE id=?1", [id])? == 0 {
                return invalid(format!("no such beam {id}"));
            }
            Ok(((), id, inv))
        })
    }

    /// Beams of one level with their ids.
    pub fn beams_on(&self, level: i64) -> Result<Vec<(i64, BeamSpec)>, Error> {
        let mut st = self.db.prepare(&format!("{BEAM_SEL} ORDER BY id"))?;
        let all = st.query_map([], beam_row)?.collect::<Result<Vec<_>, _>>()?;
        Ok(all.into_iter().filter(|b| b.1.level == level).collect())
    }

    pub fn beam(&self, id: i64) -> Result<BeamSpec, Error> {
        Ok(self
            .db
            .query_row(&format!("{BEAM_SEL} WHERE id=?1"), [id], beam_row)?
            .1)
    }

    fn member_qty(&self, kind: &str, id: i64) -> Result<MemberQuantity, Error> {
        let (table, tcol) = if kind == "column" {
            ("col", "mtype")
        } else {
            ("beam", "mtype")
        };
        Ok(self.db.query_row(
            &format!(
                "SELECT q.length, q.volume, q.cut, q.surface, t.material FROM member_qty q \
                 JOIN {table} e ON e.id = q.ref JOIN member_type t ON t.id = e.{tcol} \
                 WHERE q.kind=?1 AND q.ref=?2"
            ),
            params![kind, id],
            |r| {
                Ok(MemberQuantity {
                    length: r.get(0)?,
                    volume: r.get(1)?,
                    cut: r.get(2)?,
                    surface: r.get(3)?,
                    material: r.get(4)?,
                })
            },
        )?)
    }

    pub fn column_quantities(&self, id: i64) -> Result<MemberQuantity, Error> {
        self.member_qty("column", id)
    }

    pub fn beam_quantities(&self, id: i64) -> Result<MemberQuantity, Error> {
        self.member_qty("beam", id)
    }

    /// Bodies of all members after joints: (column | beam, id, level, mesh).
    pub fn member_meshes(&self) -> Result<Vec<(&'static str, i64, i64, Mesh)>, Error> {
        Ok(build_all(&self.db)?
            .into_iter()
            .map(|b| (b.kind, b.id, b.level, b.m.mesh()))
            .collect())
    }

    /// A member body after joints (`kind` is column | beam).
    pub fn member_mesh(&self, kind: &str, id: i64) -> Result<Mesh, Error> {
        member_mesh_db(&self.db, kind, id)
    }
}

pub(crate) fn member_mesh_db(db: &Connection, kind: &str, id: i64) -> Result<Mesh, Error> {
    build_all(db)?
        .into_iter()
        .find(|b| b.kind == kind && b.id == id)
        .map(|b| b.m.mesh())
        .ok_or_else(|| Error::Invalid(format!("no such {kind} {id}")))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{LayerSpec, SlabSpec, WallSpec};

    fn wall_vol(m: &Model, w: i64) -> f64 {
        m.sql()
            .query_row("SELECT sum(volume) FROM wall_qty WHERE wall=?1", [w], |r| {
                r.get(0)
            })
            .unwrap()
    }

    fn near(a: f64, b: f64, what: &str) {
        assert!((a - b).abs() < 1e-9, "{what}: {a} vs {b}");
    }

    fn setup() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        (m, l0, l1)
    }

    fn col(level: i64, top: i64, t: i64, at: (f64, f64)) -> ColumnSpec {
        ColumnSpec {
            level,
            top_level: Some(top),
            member_type: t,
            structural: true,
            at,
            base_offset: 0.0,
            top_offset: 0.0,
            rotation: 0.0,
            top_at: None,
        }
    }

    fn beam(level: i64, t: i64, a: (f64, f64, f64), b: (f64, f64, f64)) -> BeamSpec {
        BeamSpec {
            level,
            member_type: t,
            a,
            b,
            bulge: 0.0,
            yjust: Just::Center,
            zjust: Just::Pos,
            rotation: 0.0,
            start_ext: 0.0,
            end_ext: 0.0,
            join_start: BeamJoin::Auto,
            join_end: BeamJoin::Auto,
        }
    }

    #[test]
    fn column_wrapped_by_wall_and_undo() {
        let (mut m, l0, l1) = setup();
        let wt = m
            .add_wall_type(
                "W200",
                &[LayerSpec {
                    material: "Brick".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        let w = m
            .add_wall(&WallSpec {
                a: (0.0, 0.0),
                b: (6.0, 0.0),
                level: l0,
                base_offset: 0.0,
                height: 3.0,
                wall_type: wt,
                offset0: None,
            })
            .unwrap();
        let before: f64 = wall_vol(&m, w);
        near(before, 6.0 * 0.2 * 3.0, "wall");
        let t = m.add_catalog_member_type("R 400x400").unwrap();
        // 0.4x0.4 column on the wall axis: overlap 0.4 x 0.2 over the full height
        let c = m.add_column(&col(l0, l1, t, (3.0, 0.0))).unwrap();
        let after: f64 = wall_vol(&m, w);
        near(after, 3.6 - 0.4 * 0.2 * 3.0, "wall wraps column");
        let q = m.column_quantities(c).unwrap();
        near(q.volume, 0.16 * 3.0, "column");
        near(q.surface, 1.6 * 3.0 + 0.32, "column surface");
        near(q.length, 3.0, "column length");
        m.assert_integrity();
        m.undo().unwrap();
        let back: f64 = wall_vol(&m, w);
        near(back, 3.6, "undo");
        m.assert_integrity();
    }

    #[test]
    fn i_beam_between_columns_trimmed_and_cut_by_slab() {
        let (mut m, l0, l1) = setup();
        let ct = m.add_catalog_member_type("HEB 300").unwrap();
        let bt = m.add_catalog_member_type("IPE 300").unwrap();
        // 90 degree rotation: flanges across the beam, column face at +-0.15
        let c = ColumnSpec {
            rotation: std::f64::consts::FRAC_PI_2,
            ..col(l0, l1, ct, (0.0, 0.0))
        };
        m.add_column(&c).unwrap();
        m.add_column(&ColumnSpec {
            at: (6.0, 0.0),
            ..c
        })
        .unwrap();
        let b = m
            .add_beam(&beam(l1, bt, (0.0, 0.0, 0.0), (6.0, 0.0, 0.0)))
            .unwrap();
        let q = m.beam_quantities(b).unwrap();
        let a = 2.0 * 0.15 * 0.0107 + (0.3 - 2.0 * 0.0107) * 0.0071;
        near(q.length, 5.7, "clear span");
        near(q.volume, a * 5.7, "beam volume");
        near(q.cut, 0.0, "no cut");
        // 0.2 slab over the beam: beam top = slab top, so a flange + part of the web is within the slab
        let st = m
            .add_slab_type(
                "S200",
                &[LayerSpec {
                    material: "Concrete".into(),
                    thickness: 0.2,
                }],
            )
            .unwrap();
        m.add_slab(&SlabSpec {
            level: l1,
            slab_type: st,
            offset: 0.0,
            outer: vec![(-1.0, -3.0), (7.0, -3.0), (7.0, 3.0), (-1.0, 3.0)],
            holes: vec![],
        })
        .unwrap();
        let q = m.beam_quantities(b).unwrap();
        let band = 0.15 * 0.0107 + (0.2 - 0.0107) * 0.0071;
        near(q.cut, band * 5.7, "slab cuts beam");
        near(q.volume, (a - band) * 5.7, "net");
        // structural columns (up to 3.0) cut the 2.8...3.0 slab: 2 x (HEB 300 section x 0.2)
        let heb = 2.0 * 0.3 * 0.019 + (0.3 - 0.038) * 0.011;
        let sq = m.slab_quantities(1).unwrap();
        near(
            sq[0].volume,
            48.0 * 0.2 - 2.0 * heb * 0.2,
            "columns cut slab",
        );
        m.assert_integrity();
    }

    #[test]
    fn beams_miter_butt_and_cope() {
        let (mut m, l0, _) = setup();
        let rt = m
            .add_member_type("R", &Shape::Rect { b: 0.2, h: 0.4 }, "Timber")
            .unwrap();
        let a = m
            .add_beam(&beam(l0, rt, (0.0, 0.0, 3.0), (4.0, 0.0, 3.0)))
            .unwrap();
        let b = m
            .add_beam(&beam(l0, rt, (4.0, 0.0, 3.0), (4.0, 3.0, 3.0)))
            .unwrap();
        near(m.beam_quantities(a).unwrap().volume, 0.08 * 4.0, "miter a");
        near(m.beam_quantities(b).unwrap().volume, 0.08 * 3.0, "miter b");
        let mut s = m.beam(a).unwrap();
        s.join_end = BeamJoin::Butt;
        m.update_beam(a, &s).unwrap();
        // a runs to the face of b (x = 3.9), b is extended by 0.1
        near(m.beam_quantities(a).unwrap().length, 3.9, "butt a");
        near(m.beam_quantities(b).unwrap().length, 3.1, "extended b");
        m.undo().unwrap();
        near(m.beam_quantities(a).unwrap().length, 4.0, "undo");
        // T: an IPE 200 beam abuts an IPE 300 at mid-span, notched to the web
        let main = m.add_catalog_member_type("IPE 300").unwrap();
        let sec = m.add_catalog_member_type("IPE 200").unwrap();
        m.add_beam(&beam(l0, main, (10.0, 0.0, 3.0), (10.0, 6.0, 3.0)))
            .unwrap();
        let mut t = beam(l0, sec, (14.0, 3.0, 3.0), (10.0, 3.0, 3.0));
        let butt = m.add_beam(&t).unwrap();
        near(
            m.beam_quantities(butt).unwrap().length,
            4.0 - 0.075,
            "butt to flange",
        );
        t.join_end = BeamJoin::Cope;
        m.update_beam(butt, &t).unwrap();
        let q = m.beam_quantities(butt).unwrap();
        near(q.length, 4.0 - 0.00355, "cope to web");
        // cut: (0.075 - 0.00355) x (IPE 200 area outside the clearance 0.3 - 2*0.0107 of the main beam)
        let a200 = 2.0 * 0.1 * 0.0085 + (0.2 - 0.017) * 0.0056;
        // IPE 200 top at 3.0, main beam clearance: 2.7107...2.9893, so outside: the top 0.0107 of the flange
        let out = 0.1 * 0.0085 + 0.0056 * (0.0107 - 0.0085);
        near(q.cut, (0.075 - 0.00355) * out, "cope cut");
        near(q.volume, a200 * q.length - q.cut, "cope net");
        m.assert_integrity();
    }

    #[test]
    fn slanted_column_and_schema_13() {
        let (mut m, l0, l1) = setup();
        assert_eq!(m.schema_version().unwrap(), crate::schema::VERSION);
        let t = m
            .add_member_type("D", &Shape::Circle { d: 0.4 }, "Concrete")
            .unwrap();
        let mut s = col(l0, l1, t, (0.0, 0.0));
        s.top_at = Some((4.0, 0.0)); // 3-4-5 axis
        s.top_offset = 0.0;
        let id = m.add_column(&s).unwrap();
        let q = m.column_quantities(id).unwrap();
        near(
            q.volume,
            std::f64::consts::PI * 0.04 * 5.0,
            "slanted volume",
        );
        assert!(m.member_mesh("column", id).unwrap().is_watertight());
        assert!(m
            .add_column(&ColumnSpec {
                top_offset: -3.0,
                ..s.clone()
            })
            .is_err());
        assert!(m.delete_member_type(t).is_err());
        m.delete_column(id).unwrap();
        m.delete_member_type(t).unwrap();
        m.undo().unwrap();
        m.undo().unwrap();
        near(m.column_quantities(id).unwrap().length, 5.0, "restored");
        m.assert_integrity();
    }

    #[test]
    fn bulk_edits_members_one_undo() {
        use crate::Edit;
        let (mut m, l0, l1) = setup();
        let t = m.add_catalog_member_type("R 300x500").unwrap();
        let c = m.add_column(&col(l0, l1, t, (0.0, 0.0))).unwrap();
        let mut s = m.column(c).unwrap();
        s.top_offset = 1.0;
        m.apply_edits(&[
            Edit::Column { id: c, spec: s },
            Edit::NewColumn(col(l0, l1, t, (5.0, 0.0))),
            Edit::NewBeam(beam(l1, t, (0.0, 0.0, 0.0), (5.0, 0.0, 0.0))),
        ])
        .unwrap();
        near(m.column_quantities(c).unwrap().length, 4.0, "edited");
        assert_eq!(
            m.sql()
                .query_row("SELECT count(*) FROM beam", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        m.assert_integrity();
        m.undo().unwrap();
        near(m.column_quantities(c).unwrap().length, 3.0, "undo bulk");
        assert_eq!(
            m.sql()
                .query_row("SELECT count(*) FROM col", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            1
        );
        m.assert_integrity();
    }

    #[test]
    #[ignore]
    fn bench_10k_members_build() {
        let (mut m, l0, l1) = setup();
        let t = m.add_catalog_member_type("HEA 200").unwrap();
        let cs: Vec<ColumnSpec> = (0..5000)
            .map(|i| col(l0, l1, t, ((i % 100) as f64 * 6.0, (i / 100) as f64 * 6.0)))
            .collect();
        let bs: Vec<BeamSpec> = (0..5000)
            .map(|i| {
                let (x, y) = ((i % 100) as f64 * 6.0, (i / 100) as f64 * 6.0);
                beam(l1, t, (x, y, 0.0), (x + 6.0, y, 0.0))
            })
            .collect();
        let t0 = web_time::Instant::now();
        m.add_columns(&cs).unwrap();
        m.add_beams(&bs).unwrap();
        let dt = t0.elapsed();
        eprintln!("10k members via model: {dt:?}");
    }
}
