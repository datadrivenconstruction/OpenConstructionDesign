//! Plan transforms of stairs, railings, grids and reference planes, the helper behind the
//! matching `Target`s. A railing on a stair rides along with it; a grid moves in place, so
//! dimensions that reference it stay attached, and only columns placed on its intersections
//! follow it (see `docs/design/ELEMENT_CONTRACT.md`).

use crate::modify::{ModifyError, Target};
use crate::ops::Snapshots;
use crate::Error;
use rusqlite::{params, Connection};
use std::collections::HashMap;

type P = (f64, f64);

/// A column closer than this to an intersection of two grids was placed on it.
const ON_POINT: f64 = 1e-4;

fn refuse<T>(what: &'static str, t: Target) -> Result<T, Error> {
    Err(Error::Modify(ModifyError::Unsupported(what, t)))
}

/// True for a railing on a stair that moves in the same gesture.
pub(crate) fn rides_along(db: &Connection, t: Target, all: &[Target]) -> Result<bool, Error> {
    Ok(match t {
        Target::Railing(id) => {
            let s: Option<i64> =
                db.query_row("SELECT stair FROM railing WHERE id=?1", [id], |r| r.get(0))?;
            s.is_some_and(|s| all.contains(&Target::Stair(s)))
        }
        _ => false,
    })
}

/// A copy of a railing on a stair is first placed free; `rehost` then puts it on the copied stair.
pub(crate) fn detach(db: &Connection, orig: Target, new: i64) -> Result<(), Error> {
    if let Target::Railing(_) = orig {
        db.execute("UPDATE railing SET stair=NULL,side=NULL WHERE id=?1", [new])?;
    }
    Ok(())
}

/// The copy of a railing goes to the copy of its stair; a mirror swaps the stair side.
pub(crate) fn rehost(
    db: &Connection,
    orig: Target,
    new: Target,
    stairs: &HashMap<i64, i64>,
    mir: bool,
) -> Result<(), Error> {
    if let (Target::Railing(o), Target::Railing(n)) = (orig, new) {
        let (s, side): (Option<i64>, Option<i64>) =
            db.query_row("SELECT stair,side FROM railing WHERE id=?1", [o], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })?;
        if let (Some(s), Some(side)) = (s.and_then(|s| stairs.get(&s)), side) {
            let side = if mir { 1 - side } else { side };
            db.execute(
                "UPDATE railing SET stair=?2,side=?3 WHERE id=?1",
                params![n, s, side],
            )?;
        }
    }
    Ok(())
}

/// Copies of these elements add rows to tables outside the batch list: undo removes them.
pub(crate) fn watermarks(db: &Connection, s: &mut Snapshots, ts: &[Target]) -> Result<(), Error> {
    for t in ts {
        let tables: &[&str] = match t {
            Target::Stair(_) => &["stair"],
            Target::Railing(_) => &["railing", "railing_vertex"],
            Target::Grid(_) => &["grid"],
            Target::RefPlane(_) => &["ref_plane"],
            _ => &[],
        };
        for tb in tables {
            s.watermark(db, tb)?;
        }
    }
    Ok(())
}

/// A column that was placed on the crossing of two grids and moves with it.
pub(crate) struct Follower {
    col: i64,
    g: i64,
    h: i64,
    from: P,
}

/// Columns standing on an intersection of two grids of which at least one is moved.
pub(crate) fn followers(db: &Connection, ts: &[Target]) -> Result<Vec<Follower>, Error> {
    let moved: Vec<i64> = ts
        .iter()
        .filter_map(|t| match t {
            Target::Grid(g) => Some(*g),
            _ => None,
        })
        .collect();
    if moved.is_empty() {
        return Ok(vec![]);
    }
    let pts: Vec<_> = crate::datum::pair_points(db)?
        .into_iter()
        .filter(|x| moved.contains(&x.0) || moved.contains(&x.1))
        .collect();
    let cols: Vec<(i64, f64, f64)> = {
        let mut st = db.prepare("SELECT id,x,y FROM col")?;
        let v = st
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
            .collect::<Result<Vec<_>, _>>()?;
        v
    };
    let mut out: Vec<Follower> = vec![];
    for (c, x, y) in cols {
        if ts.contains(&Target::Column(c)) || out.iter().any(|f| f.col == c) {
            continue;
        }
        if let Some(p) = pts
            .iter()
            .find(|p| (p.2 .0 - x).hypot(p.2 .1 - y) < ON_POINT)
        {
            out.push(Follower {
                col: c,
                g: p.0,
                h: p.1,
                from: p.2,
            });
        }
    }
    Ok(out)
}

/// After the transform: the followers go to the new crossing, a railing that rode along with a
/// mirrored stair changes its side.
pub(crate) fn settle(
    db: &Connection,
    s: &mut Snapshots,
    ts: &[Target],
    fol: &[Follower],
    mir: bool,
    copy: bool,
) -> Result<(), Error> {
    if copy {
        return Ok(());
    }
    let now = if fol.is_empty() {
        vec![]
    } else {
        crate::datum::pair_points(db)?
    };
    for f in fol {
        let d = |p: &P| (p.0 - f.from.0).hypot(p.1 - f.from.1);
        let to = now
            .iter()
            .filter(|p| p.0 == f.g && p.1 == f.h)
            .map(|p| p.2)
            .min_by(|a, b| d(a).total_cmp(&d(b)));
        let Some(to) = to else { continue };
        let (dx, dy) = (to.0 - f.from.0, to.1 - f.from.1);
        s.save(db, "col", "id", f.col)?;
        db.execute(
            "UPDATE col SET x=x+?2,y=y+?3,tx=tx+?2,ty=ty+?3 WHERE id=?1",
            params![f.col, dx, dy],
        )?;
    }
    if mir {
        for t in ts {
            if let Target::Railing(r) = t {
                if rides_along(db, *t, ts)? {
                    s.save(db, "railing", "id", *r)?;
                    db.execute("UPDATE railing SET side=1-side WHERE id=?1", [r])?;
                }
            }
        }
    }
    Ok(())
}

fn turned(a: f64, v: &dyn Fn(P) -> P) -> f64 {
    let d = v((a.cos(), a.sin()));
    d.1.atan2(d.0)
}

pub(crate) fn transform(
    db: &Connection,
    t: Target,
    p: &dyn Fn(P) -> P,
    v: &dyn Fn(P) -> P,
    mir: bool,
) -> Result<(), Error> {
    match t {
        Target::Stair(id) => {
            let (x, y, ang, kind, turn): (f64, f64, f64, String, i64) = db.query_row(
                "SELECT x,y,angle,kind,turn FROM stair WHERE id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?)),
            )?;
            let q = p((x, y));
            // the origin is the middle of the first riser: a mirror swaps the turn side only
            let turn = if mir && kind != "straight" {
                1 - turn
            } else {
                turn
            };
            db.execute(
                "UPDATE stair SET x=?2,y=?3,angle=?4,turn=?5 WHERE id=?1",
                params![id, q.0, q.1, turned(ang, v), turn],
            )?;
        }
        Target::Railing(id) => {
            let host: Option<i64> =
                db.query_row("SELECT stair FROM railing WHERE id=?1", [id], |r| r.get(0))?;
            if host.is_some() {
                return refuse("moving a railing that follows its stair", t);
            }
            let pts: Vec<(i64, f64, f64)> = {
                let mut st = db.prepare("SELECT idx,x,y FROM railing_vertex WHERE railing=?1")?;
                let v = st
                    .query_map([id], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)))?
                    .collect::<Result<Vec<_>, _>>()?;
                v
            };
            for (i, x, y) in pts {
                let q = p((x, y));
                db.execute(
                    "UPDATE railing_vertex SET x=?3,y=?4 WHERE railing=?1 AND idx=?2",
                    params![id, i, q.0, q.1],
                )?;
            }
        }
        Target::Grid(id) => {
            let (a, b, bulge): (P, P, f64) = db.query_row(
                "SELECT ax,ay,bx,by,bulge FROM grid WHERE id=?1",
                [id],
                |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?), r.get(4)?)),
            )?;
            let (a, b) = (p(a), p(b));
            let k = if mir { -1.0 } else { 1.0 };
            db.execute(
                "UPDATE grid SET ax=?2,ay=?3,bx=?4,by=?5,bulge=?6 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1, bulge * k],
            )?;
        }
        Target::RefPlane(id) => {
            let (a, b): (P, P) = db.query_row(
                "SELECT ax,ay,bx,by FROM ref_plane WHERE id=?1",
                [id],
                |r| Ok(((r.get(0)?, r.get(1)?), (r.get(2)?, r.get(3)?))),
            )?;
            let (a, b) = (p(a), p(b));
            db.execute(
                "UPDATE ref_plane SET ax=?2,ay=?3,bx=?4,by=?5 WHERE id=?1",
                params![id, a.0, a.1, b.0, b.1],
            )?;
        }
        _ => {}
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use crate::views::tests::room_6x4;
    use crate::{
        ColumnSpec, DimGeom, DimKind, DimRef, DimSpec, GridSpec, MemberShape, Model, RailingHost,
        RailingSpec, StairSpec, Target,
    };

    fn ids(m: &Model, sql: &str) -> Vec<i64> {
        let mut st = m.sql().prepare(sql).unwrap();
        st.query_map([], |r| r.get(0))
            .unwrap()
            .map(|r| r.unwrap())
            .collect()
    }

    #[test]
    fn grid_move_keeps_hosted_dims() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let wall = ids(&m, "SELECT id FROM wall ORDER BY id")[0];
        let g1 = m.add_grid(&GridSpec::line("A", (1.0, -1.0), (1.0, 5.0))).unwrap();
        let g2 = m.add_grid(&GridSpec::line("B", (4.0, -1.0), (4.0, 5.0))).unwrap();
        let spec = DimSpec {
            level: lv,
            kind: DimKind::Linear,
            refs: vec![
                DimRef::GridAt { grid: g1, wall },
                DimRef::GridAt { grid: g2, wall },
            ],
            dir: (0.0, 0.0),
            side: 1.0,
            offset: 0.5,
            tier: 0,
            auto: false,
        };
        m.add_dim(&spec).unwrap();
        let len = |m: &Model| {
            let d = &m.dims(lv).unwrap()[0];
            assert!(!d.orphan, "the dimension stays attached");
            match &d.geom {
                DimGeom::Linear(l) => (l.b - l.a).len(),
                g => panic!("{g:?}"),
            }
        };
        assert!((len(&m) - 3.0).abs() < 1e-9);
        m.move_elements(&[Target::Grid(g1)], (0.5, 0.0)).unwrap();
        assert!((len(&m) - 2.5).abs() < 1e-9, "the dimension follows the grid");
        assert_eq!(m.dims(lv).unwrap().len(), 1);
        m.undo().unwrap();
        assert!((len(&m) - 3.0).abs() < 1e-9);
        m.assert_integrity();
    }

    #[test]
    fn column_follows_grid_only_when_placed_on_the_crossing() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let g1 = m.add_grid(&GridSpec::line("A", (1.0, -1.0), (1.0, 5.0))).unwrap();
        m.add_grid(&GridSpec::line("B", (-1.0, 2.0), (7.0, 2.0))).unwrap();
        let mt = m
            .add_member_type("C", &MemberShape::Rect { b: 0.3, h: 0.3 }, "Concrete")
            .unwrap();
        for at in [(1.0, 2.0), (3.0, 3.0)] {
            m.add_column(&ColumnSpec {
                level: lv,
                top_level: None,
                member_type: mt,
                structural: true,
                at,
                base_offset: 0.0,
                top_offset: 3.0,
                rotation: 0.0,
                top_at: None,
            })
            .unwrap();
        }
        let at = |m: &Model| -> Vec<(f64, f64)> {
            let mut st = m.sql().prepare("SELECT x,y FROM col ORDER BY id").unwrap();
            st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
                .unwrap()
                .map(|r| r.unwrap())
                .collect()
        };
        assert_eq!(at(&m).len(), 2);
        m.move_elements(&[Target::Grid(g1)], (0.5, 0.0)).unwrap();
        let now = at(&m);
        assert!((now[0].0 - 1.5).abs() < 1e-9 && (now[0].1 - 2.0).abs() < 1e-9);
        assert_eq!(now[1], (3.0, 3.0), "a column off the crossing stays");
        m.undo().unwrap();
        assert_eq!(at(&m)[0], (1.0, 2.0));
    }

    #[test]
    fn stair_move_carries_railing() {
        let (mut m, lv) = room_6x4(&[("Brick", 0.2)]);
        let top = m.add_level("L2", 3.0).unwrap();
        let st = m
            .add_stair(&StairSpec::straight(lv, top, (1.0, 1.0), 0.0))
            .unwrap();
        let r = m
            .add_railing(&RailingSpec::new(
                lv,
                RailingHost::Stair { stair: st, side: 0 },
            ))
            .unwrap();
        let start = |m: &Model| m.stair(st).unwrap().origin;
        let rail_len = |m: &Model| m.railing_quantities(r).unwrap().length;
        let l0 = rail_len(&m);
        m.move_elements(&[Target::Stair(st), Target::Railing(r)], (2.0, 1.0))
            .unwrap();
        assert_eq!(start(&m), (3.0, 2.0));
        assert!((rail_len(&m) - l0).abs() < 1e-9);
        // alone, a railing on a stair does not move
        assert!(m.move_elements(&[Target::Railing(r)], (1.0, 0.0)).is_err());
        // a copy of both is a stair with its own railing on it
        let out = m
            .copy_elements(&[Target::Stair(st), Target::Railing(r)], &[(0.0, 5.0)])
            .unwrap();
        let (Target::Stair(s2), Target::Railing(r2)) = (out[0], out[1]) else {
            panic!("{out:?}")
        };
        assert_eq!(m.stair(s2).unwrap().origin, (3.0, 7.0));
        assert_eq!(
            m.railing(r2).unwrap().host,
            RailingHost::Stair { stair: s2, side: 0 }
        );
        m.undo().unwrap();
        assert!(m.stair(s2).is_err());
        m.undo().unwrap();
        assert_eq!(start(&m), (1.0, 1.0));
        m.assert_integrity();
    }

    #[test]
    fn ref_plane_mirror_one_undo() {
        let mut m = Model::open(":memory:").unwrap();
        let id = m.add_ref_plane("R", (1.0, 0.0), (1.0, 4.0)).unwrap();
        let before = m.ref_planes().unwrap();
        m.mirror_elements(&[Target::RefPlane(id)], (0.0, 0.0), (0.0, 1.0), false)
            .unwrap();
        assert_eq!(m.ref_planes().unwrap()[0].1, (-1.0, 0.0));
        m.undo().unwrap();
        assert_eq!(m.ref_planes().unwrap(), before);
        let out = m
            .copy_elements(&[Target::RefPlane(id)], &[(2.0, 0.0)])
            .unwrap();
        assert_eq!(m.ref_planes().unwrap().len(), 2);
        assert_eq!(out.len(), 1);
        m.undo().unwrap();
        assert_eq!(m.ref_planes().unwrap(), before);
        m.assert_integrity();
    }
}
