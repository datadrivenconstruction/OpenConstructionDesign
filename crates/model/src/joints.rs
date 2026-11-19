// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Assembly joints between component instances (table `asm_joint`, schema step 53).
//!
//! A joint ties a frame on instance `a` to a frame on instance `b` (or on the ground). Frames
//! are stored in each instance's own coordinates, so moving an instance carries its frames.
//! Instances connected by joints form one assembly; its degrees of freedom are the rank
//! deficit of the joint equations (`ddc_geom::joint`). An instance's orientation is its plan
//! turn times its tilt (`comp.tilt`), so hinges about any axis drive.

use std::collections::BTreeSet;

use ddc_geom::joint::{Assembly, JFrame, Joint, JointError, Pose};
pub use ddc_geom::joint::{Coord, JointKind};
use rusqlite::types::Value;
use rusqlite::{params, Connection, OptionalExtension};

use crate::components::{matrix_quat, tilt_parse, tilt_text, Placement};
use crate::ops::Inv;
use crate::{Error, Model};

const TILT: f64 = 1e-9;

const DDL: &str = "
CREATE TABLE IF NOT EXISTS asm_joint(id INTEGER PRIMARY KEY, name TEXT NOT NULL, kind TEXT NOT NULL,
    a INTEGER NOT NULL, b INTEGER, fa TEXT NOT NULL, fb TEXT NOT NULL,
    rot_lo REAL, rot_hi REAL, slide_lo REAL, slide_hi REAL);
CREATE INDEX IF NOT EXISTS asm_joint_a ON asm_joint(a);
CREATE INDEX IF NOT EXISTS asm_joint_b ON asm_joint(b);
";

pub(crate) fn migrate(db: &Connection) -> Result<(), Error> {
    db.execute_batch(DDL)?;
    // full orientation of an instance: NULL = upright, a plain turn about the vertical
    let have: i64 = db.query_row(
        "SELECT count(*) FROM pragma_table_info('comp') WHERE name='tilt'",
        [],
        |r| r.get(0),
    )?;
    if have == 0 {
        db.execute_batch("ALTER TABLE comp ADD COLUMN tilt TEXT")?;
    }
    Ok(())
}

/// A new joint. `frame` is in world coordinates at the instances' current placement:
/// origin, axis (`z`) and reference direction (`x`); the joint holds as created.
#[derive(Clone, Debug, PartialEq)]
pub struct JointSpec {
    pub name: String,
    pub kind: JointKind,
    pub a: i64,
    /// `None`: tied to the ground.
    pub b: Option<i64>,
    pub frame: JFrame,
    /// Angle limits, radians (turning kinds only).
    pub rot: Option<(f64, f64)>,
    /// Slide limits, metres (sliding kinds only).
    pub slide: Option<(f64, f64)>,
}

#[derive(Clone, Debug, PartialEq)]
pub struct JointRow {
    pub id: i64,
    pub name: String,
    pub kind: JointKind,
    pub a: i64,
    pub b: Option<i64>,
    /// Frame in `a`'s coordinates.
    pub fa: JFrame,
    /// Frame in `b`'s coordinates (world for the ground).
    pub fb: JFrame,
    pub rot: Option<(f64, f64)>,
    pub slide: Option<(f64, f64)>,
}

/// Mobility of the assembly an instance belongs to.
#[derive(Clone, Debug, PartialEq)]
pub struct AsmDof {
    pub comps: Vec<i64>,
    pub joints: Vec<i64>,
    pub dof: usize,
    pub equations: usize,
    pub rank: usize,
    /// Joints with equations that repeat others (over-constrained).
    pub redundant: Vec<i64>,
    /// Instances that can still move.
    pub moving: Vec<i64>,
    /// Largest joint equation error: above zero when instances were moved by hand.
    pub residual: f64,
}

fn bad<T>(m: impl Into<String>) -> Result<T, Error> {
    Err(Error::Invalid(m.into()))
}

fn frame_text(f: &JFrame) -> String {
    let v = [f.o, f.z, f.x].concat();
    v.iter().map(|x| x.to_string()).collect::<Vec<_>>().join(" ")
}

fn frame_parse(s: &str) -> Result<JFrame, Error> {
    let v: Vec<f64> = s
        .split_whitespace()
        .map(|x| x.parse::<f64>())
        .collect::<Result<_, _>>()
        .map_err(|_| Error::Invalid(format!("joint frame: {s}")))?;
    if v.len() != 9 || !v.iter().all(|x| x.is_finite()) {
        return bad(format!("joint frame: {s}"));
    }
    Ok(JFrame {
        o: [v[0], v[1], v[2]],
        z: [v[3], v[4], v[5]],
        x: [v[6], v[7], v[8]],
    })
}

fn pair(lo: Option<f64>, hi: Option<f64>) -> Option<(f64, f64)> {
    lo.zip(hi)
}

fn check_limits(kind: JointKind, rot: Option<(f64, f64)>, slide: Option<(f64, f64)>) -> Result<(), Error> {
    for (l, ok, what) in [(rot, kind.turns(), "angle"), (slide, kind.slides(), "slide")] {
        if let Some((lo, hi)) = l {
            if !ok {
                return bad(format!("a {} joint has no {what}", kind.name()));
            }
            if !(lo.is_finite() && hi.is_finite() && lo <= hi) {
                return bad(format!("{what} limits must be finite with low <= high"));
            }
        }
    }
    Ok(())
}

/// Placement of a free-standing, unmirrored instance and its level's elevation.
fn pose(db: &Connection, comp: i64) -> Result<(Pose, f64), Error> {
    let row = db
        .query_row(
            "SELECT c.x, c.y, c.z, c.rotation, c.mirror, \
             c.host_wall IS NOT NULL OR c.host_slab IS NOT NULL OR c.host_ceiling IS NOT NULL, \
             l.elevation, c.tilt FROM comp c JOIN level l ON l.id = c.level WHERE c.id = ?1",
            [comp],
            |r| {
                Ok((
                    r.get::<_, f64>(0)?,
                    r.get::<_, f64>(1)?,
                    r.get::<_, f64>(2)?,
                    r.get::<_, f64>(3)?,
                    r.get::<_, bool>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, f64>(6)?,
                    r.get::<_, Option<String>>(7)?,
                ))
            },
        )
        .optional()?;
    let Some((x, y, z, rot, mirror, hosted, el, tilt)) = row else {
        return bad(format!("no such component {comp}"));
    };
    if mirror {
        return bad(format!("component {comp} is mirrored: joints need an unmirrored instance"));
    }
    if hosted {
        return bad(format!("component {comp} is hosted: joints need a free-standing instance"));
    }
    let p = Placement {
        origin: (x, y, el + z),
        rotation: rot,
        mirror: false,
        tilt: tilt_parse(tilt),
    };
    let m = p.matrix();
    let r = [[m[0], m[1], m[2]], [m[4], m[5], m[6]], [m[8], m[9], m[10]]];
    Ok((Pose { t: [x, y, el + z], r }, el))
}

/// Plan turn and tilt of a rotation: world = Rz(turn) * tilt; the turn keeps the local x
/// axis's plan direction (local y's when x stands vertical), no tilt when upright.
fn split_rotation(r: &[[f64; 3]; 3]) -> (f64, Option<[f64; 4]>) {
    let yaw = if r[0][0].hypot(r[1][0]) > 1e-6 {
        r[1][0].atan2(r[0][0])
    } else {
        (-r[0][1]).atan2(r[1][1])
    };
    if r[2][2] > 1.0 - TILT {
        return (yaw, None);
    }
    let (s, c) = yaw.sin_cos();
    let rz_t = [[c, s, 0.0], [-s, c, 0.0], [0.0, 0.0, 1.0]];
    let mut t = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            t[i][j] = (0..3).map(|l| rz_t[i][l] * r[l][j]).sum();
        }
    }
    (yaw, Some(matrix_quat(t)))
}

fn read_row(r: &rusqlite::Row) -> rusqlite::Result<(i64, String, String, i64, Option<i64>, String, String, [Option<f64>; 4])> {
    Ok((
        r.get(0)?,
        r.get(1)?,
        r.get(2)?,
        r.get(3)?,
        r.get(4)?,
        r.get(5)?,
        r.get(6)?,
        [r.get(7)?, r.get(8)?, r.get(9)?, r.get(10)?],
    ))
}

fn all(db: &Connection) -> Result<Vec<JointRow>, Error> {
    let mut st = db.prepare_cached(
        "SELECT id, name, kind, a, b, fa, fb, rot_lo, rot_hi, slide_lo, slide_hi \
         FROM asm_joint ORDER BY id",
    )?;
    let rows = st.query_map([], read_row)?.collect::<Result<Vec<_>, _>>()?;
    rows.into_iter()
        .map(|(id, name, kind, a, b, fa, fb, l)| {
            Ok(JointRow {
                id,
                name,
                kind: JointKind::parse(&kind)
                    .ok_or_else(|| Error::Invalid(format!("joint {id}: kind {kind}")))?,
                a,
                b,
                fa: frame_parse(&fa)?,
                fb: frame_parse(&fb)?,
                rot: pair(l[0], l[1]),
                slide: pair(l[2], l[3]),
            })
        })
        .collect()
}

/// Instances tied to `comp` through joints (the ground does not connect), and their joints.
fn group(db: &Connection, comp: i64) -> Result<(Vec<i64>, Vec<JointRow>), Error> {
    let js = all(db)?;
    let mut comps = BTreeSet::from([comp]);
    loop {
        let n = comps.len();
        for j in &js {
            if let Some(b) = j.b {
                if comps.contains(&j.a) || comps.contains(&b) {
                    comps.insert(j.a);
                    comps.insert(b);
                }
            }
        }
        if comps.len() == n {
            break;
        }
    }
    let joints = js
        .into_iter()
        .filter(|j| comps.contains(&j.a))
        .collect();
    Ok((comps.into_iter().collect(), joints))
}

struct Built {
    comps: Vec<i64>,
    joints: Vec<JointRow>,
    elev: Vec<f64>,
    asm: Assembly,
}

fn built(db: &Connection, comp: i64) -> Result<Built, Error> {
    let (comps, joints) = group(db, comp)?;
    let mut poses = vec![];
    let mut elev = vec![];
    for c in &comps {
        let (p, e) = pose(db, *c)?;
        poses.push(p);
        elev.push(e);
    }
    let at = |c: i64| comps.iter().position(|x| *x == c).expect("instance of the group");
    let asm = Assembly {
        grounded: vec![false; comps.len()],
        poses,
        joints: joints
            .iter()
            .map(|j| Joint {
                kind: j.kind,
                a: at(j.a),
                b: j.b.map(at),
                fa: j.fa,
                fb: j.fb,
                rot: j.rot,
                slide: j.slide,
            })
            .collect(),
    };
    Ok(Built {
        comps,
        joints,
        elev,
        asm,
    })
}

impl Built {
    fn err(&self, e: JointError) -> Error {
        let name = |i: usize| self.joints.get(i).map_or(i.to_string(), |j| j.name.clone());
        Error::Invalid(match e {
            JointError::Limit { joint, value } => {
                format!("joint {} would leave its limits ({value:.4})", name(joint))
            }
            JointError::NoPart(p) => format!("no such component {}", self.comps.get(p).copied().unwrap_or(-1)),
            e => format!("joint: {e}"),
        })
    }

    fn index(&self, id: i64) -> Result<usize, Error> {
        self.joints
            .iter()
            .position(|j| j.id == id)
            .ok_or_else(|| Error::Invalid(format!("no such joint {id}")))
    }
}

/// Deletes the joints of an instance being deleted; the returned undo puts them back.
pub(crate) fn detach(db: &Connection, comp: i64) -> Result<Inv, Error> {
    let inv = Inv::default().restore_where(
        db,
        "asm_joint",
        "a=?1 OR b=?1",
        &[Value::Integer(comp)],
    )?;
    db.execute("DELETE FROM asm_joint WHERE a=?1 OR b=?1", [comp])?;
    Ok(inv)
}

impl Model {
    /// World origin of an instance that can take joints (refuses mirrored and hosted ones).
    pub fn joint_anchor(&self, comp: i64) -> Result<[f64; 3], Error> {
        Ok(pose(&self.db, comp)?.0.t)
    }

    pub fn joints(&self) -> Result<Vec<JointRow>, Error> {
        all(&self.db)
    }

    /// Joints of the assembly `comp` belongs to.
    pub fn joints_of(&self, comp: i64) -> Result<Vec<JointRow>, Error> {
        Ok(group(&self.db, comp)?.1)
    }

    pub fn joint(&self, id: i64) -> Result<JointRow, Error> {
        all(&self.db)?
            .into_iter()
            .find(|j| j.id == id)
            .ok_or_else(|| Error::Invalid(format!("no such joint {id}")))
    }

    pub fn joint_add(&mut self, s: &JointSpec) -> Result<i64, Error> {
        if s.b == Some(s.a) {
            return bad("a joint needs two different components");
        }
        check_limits(s.kind, s.rot, s.slide)?;
        let f = JFrame::new(s.frame.o, s.frame.z, s.frame.x)
            .ok_or_else(|| Error::Invalid("joint frame: zero axis".into()))?;
        self.op("joint_add", |db| {
            let (pa, _) = pose(db, s.a)?;
            let fa = f.moved(&Pose::IDENTITY, &pa);
            let fb = match s.b {
                Some(b) => f.moved(&Pose::IDENTITY, &pose(db, b)?.0),
                None => f,
            };
            let n: i64 = db.query_row("SELECT count(*) FROM asm_joint", [], |r| r.get(0))?;
            let name = match s.name.trim() {
                "" => format!("{} {}", s.kind.name(), n + 1),
                t => t.to_string(),
            };
            db.execute(
                "INSERT INTO asm_joint(name, kind, a, b, fa, fb, rot_lo, rot_hi, slide_lo, slide_hi) \
                 VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10)",
                params![
                    name,
                    s.kind.name(),
                    s.a,
                    s.b,
                    frame_text(&fa),
                    frame_text(&fb),
                    s.rot.map(|l| l.0),
                    s.rot.map(|l| l.1),
                    s.slide.map(|l| l.0),
                    s.slide.map(|l| l.1),
                ],
            )?;
            let id = db.last_insert_rowid();
            Ok((id, id, Inv::default().delete("asm_joint", "id", id)))
        })
    }

    /// New name and limits (`None`: free).
    pub fn joint_edit(
        &mut self,
        id: i64,
        name: &str,
        rot: Option<(f64, f64)>,
        slide: Option<(f64, f64)>,
    ) -> Result<(), Error> {
        let j = self.joint(id)?;
        check_limits(j.kind, rot, slide)?;
        if name.trim().is_empty() {
            return bad("a joint needs a name");
        }
        self.op("joint_edit", |db| {
            let inv = Inv::default().restore(db, "asm_joint", "id", id)?;
            db.execute(
                "UPDATE asm_joint SET name=?2, rot_lo=?3, rot_hi=?4, slide_lo=?5, slide_hi=?6 WHERE id=?1",
                params![
                    id,
                    name.trim(),
                    rot.map(|l| l.0),
                    rot.map(|l| l.1),
                    slide.map(|l| l.0),
                    slide.map(|l| l.1),
                ],
            )?;
            Ok(((), id, inv))
        })
    }

    pub fn joint_delete(&mut self, id: i64) -> Result<(), Error> {
        self.op("joint_delete", |db| {
            let inv = Inv::default().restore(db, "asm_joint", "id", id)?;
            if db.execute("DELETE FROM asm_joint WHERE id=?1", [id])? == 0 {
                return bad(format!("no such joint {id}"));
            }
            Ok(((), id, inv))
        })
    }

    /// Degrees of freedom of the assembly `comp` belongs to.
    pub fn assembly_dof(&self, comp: i64) -> Result<AsmDof, Error> {
        let b = built(&self.db, comp)?;
        let d = b.asm.dof().map_err(|e| b.err(e))?;
        Ok(AsmDof {
            comps: b.comps.clone(),
            joints: b.joints.iter().map(|j| j.id).collect(),
            dof: d.dof,
            equations: d.equations,
            rank: d.rank,
            redundant: d.redundant.iter().map(|i| b.joints[*i].id).collect(),
            moving: d.moving.iter().map(|i| b.comps[*i]).collect(),
            residual: b.asm.residual(),
        })
    }

    /// Current angle (radians) or slide (metres) of a joint; `None` if it has no such coordinate.
    pub fn joint_value(&self, id: i64, c: Coord) -> Result<Option<f64>, Error> {
        let j = self.joint(id)?;
        let b = built(&self.db, j.a)?;
        Ok(b.asm.coordinate(b.index(id)?, c))
    }

    /// Moves the joint's coordinate to `value` (clamped to its limits) and places the
    /// instances of its assembly to match, in one undo step. Returns the value applied.
    pub fn joint_drive(&mut self, id: i64, c: Coord, value: f64) -> Result<f64, Error> {
        let j = self.joint(id)?;
        let b = built(&self.db, j.a)?;
        let d = b.asm.drive(b.index(id)?, c, value, &[]).map_err(|e| b.err(e))?;
        let mut moves = vec![];
        for (i, p) in d.poses.iter().enumerate() {
            let old = &b.asm.poses[i];
            let same = (0..3).all(|k| (p.t[k] - old.t[k]).abs() < 1e-12)
                && (0..3).all(|r| (0..3).all(|k| (p.r[r][k] - old.r[r][k]).abs() < 1e-12));
            if same {
                continue;
            }
            let (yaw, tilt) = split_rotation(&p.r);
            moves.push((b.comps[i], p.t[0], p.t[1], p.t[2] - b.elev[i], yaw, tilt_text(tilt)));
        }
        self.op("joint_drive", |db| {
            let mut inv = Inv::default();
            for (comp, x, y, z, yaw, tilt) in &moves {
                inv = inv.restore(db, "comp", "id", *comp)?;
                db.execute(
                    "UPDATE comp SET x=?2, y=?3, z=?4, rotation=?5, tilt=?6 WHERE id=?1",
                    params![comp, x, y, z, yaw, tilt],
                )?;
            }
            Ok((d.value, id, inv))
        })
    }
}

#[cfg(test)]
#[path = "joints_tests.rs"]
mod tests;
