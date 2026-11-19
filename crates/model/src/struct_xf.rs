// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Plan transforms of footings, rafts, braces, trusses and beam systems, the helper behind the
//! matching `Target`s. Elements that follow a host (column, wall, member) refuse to move alone:
//! regeneration would put them back.

use crate::modify::{ModifyError, Target};
use crate::Error;
use rusqlite::{params, Connection, OptionalExtension};
use std::collections::HashMap;

type P = (f64, f64);

fn refuse<T>(what: &'static str, t: Target) -> Result<T, Error> {
    Err(Error::Modify(ModifyError::Unsupported(what, t)))
}

/// Direction angle after the transform; a mirror reverses it so the layout side is mirrored too.
fn turned(a: f64, v: &dyn Fn(P) -> P, mir: bool) -> f64 {
    let d = v((a.cos(), a.sin()));
    let r = d.1.atan2(d.0);
    if mir {
        r + std::f64::consts::PI
    } else {
        r
    }
}

/// True when the element is carried by a host that moves in the same gesture. A brace with a
/// host at one end only is stretched: the hosted end follows the host, the free end stays.
/// A copy of such an element is re-hosted on the copied host instead (see `rehost`).
pub(crate) fn rides_along(db: &Connection, t: Target, all: &[Target]) -> Result<bool, Error> {
    Ok(match t {
        Target::Footing(id) => {
            let c: Option<i64> =
                db.query_row("SELECT col FROM iso_footing WHERE id=?1", [id], |r| r.get(0))?;
            c.is_some_and(|c| all.contains(&Target::Column(c)))
        }
        Target::StripFooting(id) => {
            let w: Option<i64> =
                db.query_row("SELECT wall FROM strip_footing WHERE id=?1", [id], |r| {
                    r.get(0)
                })?;
            w.is_some_and(|w| all.contains(&Target::Wall(w)))
        }
        Target::Brace(id) => {
            type Ends = (Option<String>, Option<i64>, Option<String>, Option<i64>);
            let (ha, ra, hb, rb): Ends = db.query_row(
                "SELECT host_a,ref_a,host_b,ref_b FROM brace WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let ends = [(ha, ra), (hb, rb)];
            let hosted: Vec<&(Option<String>, Option<i64>)> =
                ends.iter().filter(|e| e.1.is_some()).collect();
            !hosted.is_empty()
                && hosted.iter().all(|(h, r)| {
                    let id = r.unwrap_or(-1);
                    match h.as_deref() {
                        Some("column") => all.contains(&Target::Column(id)),
                        Some("beam") => all.contains(&Target::Beam(id)),
                        _ => false,
                    }
                })
        }
        _ => false,
    })
}

/// A copy of a hosted element is first placed free (host and refs cleared), so `transform`
/// moves its own coordinates; `rehost` then attaches it to the copied host.
pub(crate) fn detach(db: &Connection, orig: Target, new: i64) -> Result<(), Error> {
    let sql = match orig {
        Target::Footing(_) => "UPDATE iso_footing SET col=NULL WHERE id=?1",
        Target::StripFooting(_) => "UPDATE strip_footing SET wall=NULL WHERE id=?1",
        Target::Brace(_) => {
            "UPDATE brace SET host_a=NULL,ref_a=NULL,host_b=NULL,ref_b=NULL WHERE id=?1"
        }
        _ => return Ok(()),
    };
    db.execute(sql, [new])?;
    Ok(())
}

/// Re-hosts the copy `new` on the copies of the hosts of `orig`. A host that was not copied
/// leaves the copy free, never attached to the original.
pub(crate) fn rehost(
    db: &Connection,
    orig: Target,
    new: Target,
    walls: &HashMap<i64, i64>,
    cols: &HashMap<i64, i64>,
    beams: &HashMap<i64, i64>,
) -> Result<(), Error> {
    match (orig, new) {
        (Target::Footing(o), Target::Footing(n)) => {
            let c: Option<i64> =
                db.query_row("SELECT col FROM iso_footing WHERE id=?1", [o], |r| r.get(0))?;
            if let Some(c) = c.and_then(|c| cols.get(&c)) {
                // rotation is relative to the column, which already turned: keep the original
                db.execute(
                    "UPDATE iso_footing SET col=?2, \
                     rotation=(SELECT rotation FROM iso_footing WHERE id=?3) WHERE id=?1",
                    params![n, c, o],
                )?;
            }
        }
        (Target::StripFooting(o), Target::StripFooting(n)) => {
            let w: Option<i64> =
                db.query_row("SELECT wall FROM strip_footing WHERE id=?1", [o], |r| {
                    r.get(0)
                })?;
            if let Some(w) = w.and_then(|w| walls.get(&w)) {
                // eccentricity is relative to the wall side, as when it rides along on a move
                db.execute(
                    "UPDATE strip_footing SET wall=?2, \
                     ecc=(SELECT ecc FROM strip_footing WHERE id=?3) WHERE id=?1",
                    params![n, w, o],
                )?;
            }
        }
        (Target::Brace(o), Target::Brace(n)) => {
            type Ends = (Option<String>, Option<i64>, Option<String>, Option<i64>);
            let (ha, ra, hb, rb): Ends = db.query_row(
                "SELECT host_a,ref_a,host_b,ref_b FROM brace WHERE id=?1",
                [o],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let pick = |h: &Option<String>, r: Option<i64>| match (h.as_deref(), r) {
                (Some("column"), Some(r)) => cols.get(&r).map(|c| ("column", *c)),
                (Some("beam"), Some(r)) => beams.get(&r).map(|b| ("beam", *b)),
                _ => None,
            };
            if let Some((h, r)) = pick(&ha, ra) {
                db.execute("UPDATE brace SET host_a=?2,ref_a=?3 WHERE id=?1", params![n, h, r])?;
            }
            if let Some((h, r)) = pick(&hb, rb) {
                db.execute("UPDATE brace SET host_b=?2,ref_b=?3 WHERE id=?1", params![n, h, r])?;
            }
        }
        _ => {}
    }
    Ok(())
}

pub(crate) fn transform(
    db: &Connection,
    t: Target,
    p: &dyn Fn(P) -> P,
    v: &dyn Fn(P) -> P,
    mir: bool,
) -> Result<(), Error> {
    match t {
        Target::Footing(id) => {
            let (col, rot, x, y): (Option<i64>, f64, f64, f64) = db.query_row(
                "SELECT col, rotation, x, y FROM iso_footing WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            if col.is_some() {
                return refuse("moving a footing that follows its column", t);
            }
            let q = p((x, y));
            let nr = turned(rot, v, false);
            db.execute(
                "UPDATE iso_footing SET x=?2,y=?3,rotation=?4,frot=?4 WHERE id=?1",
                params![id, q.0, q.1, nr],
            )?;
        }
        Target::StripFooting(id) => {
            let (w, ax, ay, bx, by, bulge, ecc): (Option<i64>, f64, f64, f64, f64, f64, f64) = db
                .query_row(
                    "SELECT wall,ax,ay,bx,by,bulge,ecc FROM strip_footing WHERE id=?1",
                    [id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                            r.get(6)?,
                        ))
                    },
                )?;
            if w.is_some() {
                return refuse("moving a strip footing that follows its wall", t);
            }
            let (a, b) = (p((ax, ay)), p((bx, by)));
            let k = if mir { -1.0 } else { 1.0 };
            db.execute(
                "UPDATE strip_footing SET ax=?2,ay=?3,bx=?4,by=?5,bulge=?6,ecc=?7 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1, bulge * k, ecc * k],
            )?;
        }
        Target::Raft(id) => {
            let pts: Vec<(i64, f64, f64)> = {
                let mut st =
                    db.prepare("SELECT idx,x,y FROM raft_vertex WHERE raft=?1 ORDER BY idx")?;
                let v = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            let n = pts.len() as i64;
            for (i, x, y) in &pts {
                let q = p((*x, *y));
                // a mirror reverses the winding: keep the orientation by reversing the order
                let at = if mir { n - 1 - i } else { *i };
                db.execute(
                    "UPDATE raft_vertex SET x=?3,y=?4 WHERE raft=?1 AND idx=?2",
                    params![id, at, q.0, q.1],
                )?;
            }
        }
        Target::Brace(id) => {
            let (ra, rb, ax, ay, bx, by): (Option<i64>, Option<i64>, f64, f64, f64, f64) = db
                .query_row(
                    "SELECT ref_a,ref_b,ax,ay,bx,by FROM brace WHERE id=?1",
                    [id],
                    |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    },
                )?;
            if ra.is_some() || rb.is_some() {
                return refuse("moving a brace that follows its members", t);
            }
            let (a, b) = (p((ax, ay)), p((bx, by)));
            let k = if mir { -1.0 } else { 1.0 };
            db.execute(
                "UPDATE brace SET ax=?2,ay=?3,bx=?4,by=?5,rotation=rotation*?6 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1, k],
            )?;
        }
        Target::Truss(id) => {
            let (ax, ay, bx, by): (f64, f64, f64, f64) = db.query_row(
                "SELECT ax,ay,bx,by FROM truss WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )?;
            let (a, b) = (p((ax, ay)), p((bx, by)));
            db.execute(
                "UPDATE truss SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1],
            )?;
        }
        Target::BeamSystem(id) => {
            let pts: Vec<(i64, f64, f64)> = {
                let mut st = db.prepare("SELECT idx,x,y FROM beam_system_vertex WHERE sys=?1")?;
                let v = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            for (i, x, y) in pts {
                let q = p((x, y));
                db.execute(
                    "UPDATE beam_system_vertex SET x=?3,y=?4 WHERE sys=?1 AND idx=?2",
                    params![id, i, q.0, q.1],
                )?;
            }
            let angle: Option<f64> = db
                .query_row("SELECT angle FROM beam_system WHERE id=?1", [id], |r| {
                    r.get(0)
                })
                .optional()?;
            if let Some(a) = angle {
                db.execute(
                    "UPDATE beam_system SET angle=?2 WHERE id=?1",
                    params![id, turned(a, v, mir)],
                )?;
            }
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{FootingSpec, FootingStep, Model, RaftSpec, Spacing};

    fn model() -> (Model, i64, i64) {
        let mut m = Model::open(":memory:").unwrap();
        let level = m.add_level("L0", 0.0).unwrap();
        let f = m
            .add_footing(&FootingSpec {
                level,
                column: None,
                at: (1.0, 2.0),
                rotation: 0.0,
                round: false,
                steps: vec![FootingStep {
                    w: 1.0,
                    l: 1.0,
                    h: 0.5,
                }],
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        let r = m
            .add_raft(&RaftSpec {
                level,
                ring: vec![(0.0, 0.0), (4.0, 0.0), (4.0, 4.0), (0.0, 4.0)],
                thickness: 0.4,
                edge_width: 0.0,
                edge_depth: 0.0,
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        (m, f, r)
    }

    #[test]
    fn footing_moves_rotates_and_undoes() {
        let (mut m, f, _) = model();
        m.move_elements(&[Target::Footing(f)], (3.0, 0.0)).unwrap();
        assert_eq!(m.footing(f).unwrap().at, (4.0, 2.0));
        m.rotate_elements(&[Target::Footing(f)], (0.0, 0.0), 0.5, false)
            .unwrap();
        assert!((m.footing(f).unwrap().rotation - 0.5).abs() < 1e-9);
        m.undo().unwrap();
        m.undo().unwrap();
        assert_eq!(m.footing(f).unwrap().at, (1.0, 2.0));
    }

    #[test]
    fn raft_copy_and_array_keep_the_ring() {
        let (mut m, _, r) = model();
        let out = m.copy_elements(&[Target::Raft(r)], &[(10.0, 0.0)]).unwrap();
        let Target::Raft(n) = out[0] else { panic!() };
        assert_eq!(m.raft(n).unwrap().ring[0], (10.0, 0.0));
        let arr = m
            .array_linear(&[Target::Raft(r)], 3, Spacing::Step((0.0, 8.0)))
            .unwrap();
        assert!(arr.len() >= 2);
    }

    #[test]
    fn copying_a_column_with_its_footing_copies_and_rehosts_it() {
        use crate::ColumnSpec;
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        let t = m
            .add_member_type("R", &ddc_geom::member::Shape::Rect { b: 0.4, h: 0.4 }, "Steel")
            .unwrap();
        let c = m
            .add_column(&ColumnSpec {
                level: l0,
                top_level: Some(l1),
                member_type: t,
                structural: true,
                at: (0.0, 0.0),
                base_offset: 0.0,
                top_offset: 0.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        let f = m
            .add_footing(&FootingSpec {
                level: l0,
                column: Some(c),
                at: (0.0, 0.0),
                rotation: 0.0,
                round: false,
                steps: vec![FootingStep { w: 1.0, l: 1.0, h: 0.5 }],
                top_offset: 0.0,
                material: "Concrete".into(),
            })
            .unwrap();
        let out = m
            .copy_elements(&[Target::Column(c), Target::Footing(f)], &[(10.0, 0.0)])
            .unwrap();
        assert_eq!(out.len(), 2, "both copied");
        let (Some(Target::Column(nc)), Some(Target::Footing(nf))) = (
            out.iter().find(|t| matches!(t, Target::Column(_))).copied(),
            out.iter().find(|t| matches!(t, Target::Footing(_))).copied(),
        ) else {
            panic!("copies missing")
        };
        assert_eq!(m.footing(nf).unwrap().column, Some(nc));
        assert_eq!(m.footing(f).unwrap().column, Some(c));
        // a hosted footing stores its offset from the column, which is the one that moved
        assert!((m.column(nc).unwrap().at.0 - 10.0).abs() < 1e-6);
        assert!(m.footing(nf).unwrap().at.0.abs() < 1e-6);
        m.undo().unwrap();
        assert!(m.struct_ids("footing", None).unwrap().len() == 1, "one undo step");
    }

    #[test]
    fn brace_with_one_hosted_end_stretches_when_the_host_moves() {
        use crate::{Anchor, BraceSpec, ColumnSpec, MemberHost};
        let mut m = Model::open(":memory:").unwrap();
        let l0 = m.add_level("L0", 0.0).unwrap();
        let l1 = m.add_level("L1", 3.0).unwrap();
        let ct = m
            .add_member_type("C", &ddc_geom::member::Shape::Rect { b: 0.4, h: 0.4 }, "Steel")
            .unwrap();
        let bt = m
            .add_member_type("B", &ddc_geom::member::Shape::Rect { b: 0.1, h: 0.1 }, "Steel")
            .unwrap();
        let c = m
            .add_column(&ColumnSpec {
                level: l0,
                top_level: Some(l1),
                member_type: ct,
                structural: true,
                at: (0.0, 0.0),
                base_offset: 0.0,
                top_offset: 0.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        let b = m
            .add_brace(&BraceSpec {
                level: l0,
                member_type: bt,
                a: (0.0, 0.0, 0.3),
                b: (6.0, 0.0, 2.7),
                start: Some(Anchor { host: MemberHost::Column, id: c, t: 0.1 }),
                end: None,
                rotation: 0.0,
            })
            .unwrap();
        m.move_elements(&[Target::Column(c), Target::Brace(b)], (0.0, 2.0))
            .unwrap();
        let s = m.brace(b).unwrap();
        assert!((s.a.1 - 2.0).abs() < 1e-6, "hosted end follows");
        assert!((s.b.0 - 6.0).abs() < 1e-9 && s.b.1.abs() < 1e-9, "free end stays");
    }
}
