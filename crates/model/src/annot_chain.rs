// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Dimension chain along a wall run: one wall is taken together with collinear neighbours that abut it;
//! several walls give one chain per straight line. Tiers: ends, joints and openings; axes; overall size.

use ddc_geom::{dim, v2, V2};
use rusqlite::Connection;

use super::tags::{exterior_face, replace_linear};
use super::{Ctx, DimKind, DimRef, DimSpec, W};
use crate::{Error, Model};

const LINE_TOL: f64 = 1e-3;

#[derive(Clone, Copy)]
struct Seg {
    id: i64,
    a: V2,
    b: V2,
}

impl Seg {
    fn dir(&self) -> V2 {
        let d = self.b - self.a;
        d * (1.0 / d.len())
    }
    fn on_line(&self, o: &Seg) -> bool {
        let d = self.dir();
        let off = |p: V2| {
            let r = p - self.a;
            (r.x * d.y - r.y * d.x).abs()
        };
        off(o.a) < LINE_TOL && off(o.b) < LINE_TOL
    }
    fn touches(&self, o: &Seg) -> bool {
        [(self.a, o.a), (self.a, o.b), (self.b, o.a), (self.b, o.b)]
            .iter()
            .any(|(p, q)| (*p - *q).len() < LINE_TOL)
    }
}

fn seg(cx: &mut Ctx, id: i64) -> Result<Option<Seg>, Error> {
    let Some(w) = cx.wall(id)? else {
        return Ok(None);
    };
    if w.arc.is_some() || w.len() < 1e-6 {
        return Ok(None);
    }
    Ok(Some(Seg {
        id,
        a: v2(w.a.0, w.a.1),
        b: v2(w.b.0, w.b.1),
    }))
}

fn ids_of(db: &Connection, sql: &str, key: i64) -> Result<Vec<i64>, Error> {
    let mut st = db.prepare(sql)?;
    let v = st
        .query_map([key], |r| r.get(0))?
        .collect::<Result<_, _>>()?;
    Ok(v)
}

fn runs(cx: &mut Ctx, level: i64, walls: &[i64]) -> Result<Vec<Vec<Seg>>, Error> {
    let mut given: Vec<Seg> = vec![];
    for &id in walls {
        if given.iter().any(|s| s.id == id) {
            continue;
        }
        if let Some(s) = seg(cx, id)? {
            given.push(s);
        }
    }
    if given.len() == 1 {
        let mut others = vec![];
        for id in ids_of(
            cx.db,
            "SELECT id FROM wall WHERE level=?1 ORDER BY id",
            level,
        )? {
            if id != given[0].id {
                others.extend(seg(cx, id)?);
            }
        }
        loop {
            let before = given.len();
            others.retain(|o: &Seg| {
                if given.iter().any(|r| r.on_line(o) && r.touches(o)) {
                    given.push(*o);
                    false
                } else {
                    true
                }
            });
            if given.len() == before {
                return Ok(vec![given]);
            }
        }
    }
    // collinear but with a gap means separate chains: otherwise a dimension would span the empty gap
    let mut out: Vec<Vec<Seg>> = vec![];
    for s in given {
        let hit = |r: &Vec<Seg>| r[0].on_line(&s) && r.iter().any(|q| q.touches(&s));
        let mut run = vec![s];
        let mut i = 0;
        while i < out.len() {
            if hit(&out[i]) {
                run.append(&mut out.remove(i));
            } else {
                i += 1;
            }
        }
        out.push(run);
    }
    Ok(out)
}

/// Wall face on the chosen side of the chain (`left` is left of the wall's own axis).
fn face_on(w: &W, left: bool) -> u8 {
    let hi = if w.face(0) >= w.face(1) { 0u8 } else { 1 };
    if left {
        hi
    } else {
        1 - hi
    }
}

struct Run {
    level: i64,
    dir: V2,
    side: f64,
    base: DimRef,
    tier: i32,
}

impl Run {
    fn tier(&mut self, cx: &mut Ctx, refs: &[DimRef], out: &mut Vec<DimSpec>) -> Result<(), Error> {
        let mut pts = vec![];
        for r in refs {
            if let Some(p) = cx.point(r)? {
                pts.push((*r, p.2));
            }
        }
        let vs: Vec<V2> = pts.iter().map(|p| p.1).collect();
        // same tolerance as a wall joint: a sub-millimetre miss does not produce a "0" dimension
        let pairs = dim::chain(&vs, self.dir, LINE_TOL);
        if pairs.is_empty() {
            return Ok(());
        }
        for (i, j) in pairs {
            out.push(DimSpec {
                level: self.level,
                kind: DimKind::Linear,
                refs: vec![pts[i].0, pts[j].0, self.base],
                dir: (self.dir.x, self.dir.y),
                side: self.side,
                offset: 0.7 + 0.6 * f64::from(self.tier),
                tier: -1,
                auto: false,
            });
        }
        self.tier += 1;
        Ok(())
    }
}

fn run_specs(
    cx: &mut Ctx,
    level: i64,
    run: &mut [Seg],
    grids: &[i64],
    out: &mut Vec<DimSpec>,
) -> Result<(), Error> {
    let dir = run[0].dir();
    let along = |s: &Seg| s.a.dot(dir).min(s.b.dot(dir));
    run.sort_by(|p, q| along(p).total_cmp(&along(q)));
    // the exterior side is decided by the majority of walls: a wall in the middle of an interior row may have none
    let mut vote = 0i32;
    for s in run.iter() {
        let f = exterior_face(cx, level, s.id)?;
        let w = cx.wall(s.id)?.expect("wall cached");
        let left = w.face(f) >= w.face(1 - f);
        vote += if left == (s.dir().dot(dir) > 0.0) {
            1
        } else {
            -1
        };
    }
    let side = if vote >= 0 { 1.0 } else { -1.0 };
    let mut ends = vec![];
    let mut all = vec![];
    let mut grefs = vec![];
    let mut openings = false;
    for s in run.iter() {
        let w = cx.wall(s.id)?.expect("wall cached");
        let len = w.len();
        let face = face_on(w, (side > 0.0) == (s.dir().dot(dir) > 0.0));
        for end in [0u8, 1] {
            ends.push(DimRef::WallEnd {
                wall: s.id,
                face,
                end,
            });
        }
        let ops = ids_of(
            cx.db,
            "SELECT id FROM opening WHERE wall=?1 ORDER BY along",
            s.id,
        )?;
        openings |= !ops.is_empty();
        for o in ops {
            for edge in [0u8, 1] {
                all.push(DimRef::OpeningEdge {
                    opening: o,
                    edge,
                    face,
                });
            }
        }
        for &g in grids {
            let r = DimRef::GridAt {
                grid: g,
                wall: s.id,
            };
            if let Some((_, t, _)) = cx.point(&r)? {
                if t > -1e-6 && t < len + 1e-6 {
                    grefs.push(r);
                }
            }
        }
    }
    let (mut lo, mut hi) = (ends[0], ends[0]);
    let (mut lo_t, mut hi_t) = (f64::INFINITY, f64::NEG_INFINITY);
    for r in &ends {
        let t = cx.point(r)?.map_or(f64::NAN, |p| p.2.dot(dir));
        if t < lo_t {
            (lo, lo_t) = (*r, t);
        }
        if t > hi_t {
            (hi, hi_t) = (*r, t);
        }
    }
    let mut st = Run {
        level,
        dir,
        side,
        base: lo,
        tier: 0,
    };
    if openings || run.len() > 1 {
        let mut refs = ends.clone();
        refs.append(&mut all);
        st.tier(cx, &refs, out)?;
    }
    if !grefs.is_empty() {
        let mut refs = vec![lo, hi];
        refs.append(&mut grefs);
        st.tier(cx, &refs, out)?;
    }
    st.tier(cx, &[lo, hi], out)
}

fn chain_specs(db: &Connection, level: i64, walls: &[i64]) -> Result<Vec<DimSpec>, Error> {
    let mut cx = Ctx {
        db,
        walls: Default::default(),
    };
    let grids = {
        let mut st = db.prepare("SELECT id FROM grid WHERE bulge=0 ORDER BY id")?;
        let v: Vec<i64> = st.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
        v
    };
    let mut out = vec![];
    for mut run in runs(&mut cx, level, walls)? {
        run_specs(&mut cx, level, &mut run, &grids, &mut out)?;
    }
    Ok(out)
}

impl Model {
    /// How many of the given walls the chain will not take: arc and degenerate ones.
    pub fn chain_skipped(&self, walls: &[i64]) -> Result<usize, Error> {
        let mut cx = Ctx {
            db: &self.db,
            walls: Default::default(),
        };
        let mut n = 0;
        for &id in walls {
            if seg(&mut cx, id)?.is_none() {
                n += 1;
            }
        }
        Ok(n)
    }

    /// Dimensions along a wall run on the exterior side; identical manual ones are replaced. One operation.
    pub fn auto_dimension_chain(&mut self, level: i64, walls: &[i64]) -> Result<Vec<i64>, Error> {
        let specs = chain_specs(&self.db, level, walls)?;
        if specs.is_empty() {
            return Err(Error::Invalid("no straight walls to dimension".into()));
        }
        self.op("dim_auto_walls", |db| {
            let (ids, inv) = replace_linear(db, level, &specs)?;
            Ok((ids, level, inv))
        })
    }
}
