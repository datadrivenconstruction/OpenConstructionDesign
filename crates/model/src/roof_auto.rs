// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Wall tops by rule: a straight wall whose flat top reaches the roof and whose axis lies under
//! it gets a top following the roof underside (`wall.top_auto = 1`, `top_roof` is the roof over
//! most of the axis). Stretches under several roofs (joints, extensions) take the lowest of the
//! undersides; stretches outside the roof stay flat at `height`. Editing/deleting a roof and
//! adding/moving a wall recompute the attachment in the same operation; an attachment released
//! by hand (`top_auto = 2`) is left alone by the rule.

use ddc_geom::roof::Roof;
use ddc_geom::v2;
use rusqlite::{params, Connection};

use crate::ops::Inv;
use crate::Error;

const EPS: f64 = 1e-9;

struct Ctx {
    /// Roof `roof.id`; a shell is the negative shell id.
    id: i64,
    geom: Option<Roof>,
    shell: Option<Shell>,
    /// Thickness of the roof layers.
    t: f64,
    /// From the top to the underside of the bearing layer: the wall core reaches it.
    ts: f64,
    /// Elevation of the roof planes above the bearing line.
    base_z: f64,
}

struct Cand {
    id: i64,
    a: (f64, f64),
    b: (f64, f64),
    /// Axis curvature: 0 is straight; for an arc, profile u is the length along the axis.
    bulge: f64,
    base: f64,
    height: f64,
    auto: i64,
    top_roof: Option<i64>,
}

/// A straight axis stretch: chord `a`->`b`, starting at length `u0`, `k` is axis length per chord length.
struct Piece {
    a: (f64, f64),
    b: (f64, f64),
    u0: f64,
    k: f64,
}

impl Cand {
    fn arc(&self) -> Option<ddc_geom::arc::ArcAxis> {
        (self.bulge != 0.0)
            .then(|| ddc_geom::arc::ArcAxis::new(v2(self.a.0, self.a.1), v2(self.b.0, self.b.1), self.bulge, ddc_geom::Tol::default()).ok())
            .flatten()
    }

    /// Axis length (along the arc for an arc).
    fn len(&self) -> f64 {
        match self.arc() {
            Some(ax) => ax.length(),
            None => ((self.b.0 - self.a.0).powi(2) + (self.b.1 - self.a.1).powi(2)).sqrt(),
        }
    }

    /// The axis as straight stretches: chords of an arc are at most ~0.25 m, a straight axis is itself.
    fn pieces(&self) -> Vec<Piece> {
        let Some(ax) = self.arc() else {
            return vec![Piece { a: self.a, b: self.b, u0: 0.0, k: 1.0 }];
        };
        let len = ax.length();
        let n = ((len / 0.25).ceil() as usize).clamp(4, 800);
        let pt = |i: usize| ax.point_at(len * i as f64 / n as f64);
        (0..n)
            .filter_map(|i| {
                let (p, q) = (pt(i), pt(i + 1));
                let ch = (q - p).len();
                (ch > EPS).then(|| Piece { a: (p.x, p.y), b: (q.x, q.y), u0: len * i as f64 / n as f64, k: len / n as f64 / ch })
            })
            .collect()
    }

    /// The stretch as a straight wall: its own bounds.
    fn straight(&self, p: &Piece) -> Cand {
        Cand { id: self.id, a: p.a, b: p.b, bulge: 0.0, base: self.base, height: self.height, auto: self.auto, top_roof: self.top_roof }
    }
}

/// Project rules: how far a wall top may be below the roof, the share of the axis under it, the minimum height.
struct Rules {
    reach: f64,
    min_cover: f64,
    min_height: f64,
}

fn rules(db: &Connection) -> Rules {
    use crate::roof_rules::{value, MIN_COVER, MIN_HEIGHT, REACH};
    Rules { reach: value(db, REACH), min_cover: value(db, MIN_COVER), min_height: value(db, MIN_HEIGHT) }
}

fn context(db: &Connection, id: i64) -> Result<Option<Ctx>, Error> {
    let Ok(s) = crate::roofs::load(db, id) else { return Ok(None) };
    // with cutouts: a dormer, shaft and lantern are not roof; the wall does not extend to the slope under them
    let Ok(geom) = crate::roofs::cut_geometry(db, id, &s) else { return Ok(None) };
    let t: f64 = crate::roofs::layer_sum(db, s.slab_type)?;
    let elev: f64 = db.query_row("SELECT elevation FROM level WHERE id=?1", [s.level], |r| r.get(0))?;
    let ts = crate::layer_core::roof_structure_depth(db, s.slab_type)?.min(t);
    Ok(Some(Ctx { id, geom: Some(geom), shell: None, t, ts, base_z: elev + s.offset }))
}

/// A curved shell roof: the underside follows the surface, sampled along the wall axis.
struct Shell {
    surf: ddc_geom::nurbs::Surface,
    trim: ddc_geom::nurbs::Trim,
    /// Level elevation.
    z0: f64,
    /// Stack bounds along the surface normal (signed).
    lo_n: f64,
    hi_n: f64,
    /// Surface extent in plan: [x0, y0, x1, y1].
    bb: [f64; 4],
}

/// A shell roof (dome, free form): `id` is the id in `shell_elem`.
fn shell_context(db: &Connection, id: i64) -> Result<Option<Ctx>, Error> {
    let Ok(s) = crate::shell_elem::read(db, id) else { return Ok(None) };
    if s.kind != crate::ShellKind::Roof {
        return Ok(None);
    }
    let Ok(trim) = s.full_trim() else { return Ok(None) };
    let z0: f64 = db.query_row("SELECT elevation FROM level WHERE id=?1", [s.level], |r| r.get(0))?;
    let offs = s.offsets();
    let lo_n = offs.iter().fold(f64::MAX, |m, o| m.min(o.0));
    let hi_n = offs.iter().fold(f64::MIN, |m, o| m.max(o.1));
    let (mut x0, mut y0, mut x1, mut y1) = (f64::MAX, f64::MAX, f64::MIN, f64::MIN);
    for p in s.surface.ctrl() {
        (x0, y0, x1, y1) = (x0.min(p.x), y0.min(p.y), x1.max(p.x), y1.max(p.y));
    }
    let t = s.thickness();
    let low = s.surface.ctrl().iter().fold(f64::MAX, |m, p| m.min(p.z));
    let sh = Shell { surf: s.surface, trim, z0, lo_n, hi_n, bb: [x0, y0, x1, y1] };
    Ok(Some(Ctx { id: -id, geom: None, shell: Some(sh), t, ts: t, base_z: z0 + low }))
}

fn context_of(db: &Connection, id: i64) -> Result<Option<Ctx>, Error> {
    if id < 0 {
        shell_context(db, -id)
    } else {
        context(db, id)
    }
}

/// Parameters of the surface point above (x, y): Newton from the previous solution, otherwise a global search.
fn locate(sf: &ddc_geom::nurbs::Surface, x: f64, y: f64, prev: Option<[f64; 2]>) -> Option<[f64; 2]> {
    let (u0, u1) = sf.domain_u();
    let (v0, v1) = sf.domain_v();
    let newton = |[mut u, mut v]: [f64; 2], steps: usize| -> Option<[f64; 2]> {
        for _ in 0..steps {
            let d = sf.derivs(u, v, 1);
            let (fx, fy) = (d[0][0].x - x, d[0][0].y - y);
            if (fx * fx + fy * fy).sqrt() <= 1e-9 {
                return Some([u, v]);
            }
            let (a, b, c, e) = (d[1][0].x, d[0][1].x, d[1][0].y, d[0][1].y);
            let det = a * e - b * c;
            if !(det.abs() > 1e-12) {
                return None;
            }
            let (nu, nv) = ((u - (e * fx - b * fy) / det).clamp(u0, u1), (v - (a * fy - c * fx) / det).clamp(v0, v1));
            if (nu - u).abs() + (nv - v).abs() < 1e-15 {
                return None;
            }
            (u, v) = (nu, nv);
        }
        None
    };
    if let Some(uv) = prev.and_then(|p| newton(p, 12)) {
        return Some(uv);
    }
    if let Some(uv) = sf.at_plan(x, y, 1e-6) {
        return Some(uv);
    }
    // next to a pole (the apex of a dome) one plan step turns the parameters half round and the
    // searches stall on the collapsed edge: start afresh from a grid of seeds, only while a run
    // is under way (an edge of the surface is met once per run, not at every sample past it)
    prev?;
    const SEEDS: usize = 8;
    (0..=SEEDS)
        .flat_map(|i| (0..=SEEDS).map(move |j| (i, j)))
        .map(|(i, j)| [u0 + (u1 - u0) * i as f64 / SEEDS as f64, v0 + (v1 - v0) * j as f64 / SEEDS as f64])
        .find_map(|s| newton(s, 30))
}

/// The shell underside along the wall axis as continuous stretches: sampled at a step of at most
/// 0.1 m; gaps outside the surface, outside the trim and on steep spots (near-horizontal normal) break a stretch.
fn shell_runs(sh: &Shell, c: &Cand) -> Vec<Vec<(f64, f64)>> {
    let (a, b) = (v2(c.a.0, c.a.1), v2(c.b.0, c.b.1));
    debug_assert!(c.bulge == 0.0);
    let len = (b - a).len();
    let n = ((len / 0.1).ceil() as usize).clamp(2, 600);
    let (u0, u1) = sh.surf.domain_u();
    let (v0, v1) = sh.surf.domain_v();
    let mut runs: Vec<Vec<(f64, f64)>> = vec![];
    let mut run: Vec<(f64, f64)> = vec![];
    let mut prev = None;
    for i in 0..=n {
        let f = i as f64 / n as f64;
        let p = a + (b - a) * f;
        let inside = p.x >= sh.bb[0] - 1e-6 && p.x <= sh.bb[2] + 1e-6 && p.y >= sh.bb[1] - 1e-6 && p.y <= sh.bb[3] + 1e-6;
        let hit = if inside { locate(&sh.surf, p.x, p.y, prev) } else { None };
        let z = hit.and_then(|uv| {
            let q = [(uv[0] - u0) / (u1 - u0), (uv[1] - v0) / (v1 - v0)];
            if !sh.trim.keeps(q) {
                return None;
            }
            let nrm = sh.surf.normal(uv[0], uv[1])?;
            if nrm.z.abs() < 0.2 {
                return None;
            }
            // stack underside vertically above the point: the offset along the normal divided by its vertical component
            let dz = if nrm.z > 0.0 { sh.lo_n.min(0.0) / nrm.z } else { sh.hi_n.max(0.0) / nrm.z };
            Some(sh.z0 + sh.surf.point(uv[0], uv[1]).z + dz)
        });
        match (hit, z) {
            (Some(uv), Some(z)) => {
                prev = Some(uv);
                run.push((f * len, z));
            }
            _ => {
                prev = None;
                if run.len() > 1 {
                    runs.push(std::mem::take(&mut run));
                } else {
                    run.clear();
                }
            }
        }
    }
    if run.len() > 1 {
        runs.push(run);
    }
    runs
}


/// Roofs (id, plane elevation) without building shapes.
fn bases(db: &Connection) -> Result<Vec<(i64, f64)>, Error> {
    let mut st = db.prepare("SELECT r.id, l.elevation + r.offset FROM roof r JOIN level l ON l.id = r.level ORDER BY r.id")?;
    let mut v: Vec<(i64, f64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut sh = db.prepare("SELECT id FROM shell_elem WHERE kind='roof' ORDER BY id")?;
    let ids: Vec<i64> = sh.query_map([], |r| r.get(0))?.collect::<Result<_, _>>()?;
    for id in ids {
        if let Ok(Some(c)) = shell_context(db, id) {
            v.push((-id, c.base_z));
        }
    }
    Ok(v)
}

/// The wall row snapshot for undo is recorded first: the journal is replayed forward, so the
/// pre-operation state must come first, however many times and in whatever order the wall changed.
fn snap(db: &Connection, inv: Inv, wall: i64) -> Result<Inv, Error> {
    Ok(Inv::default().restore(db, "wall", "id", wall)?.then(inv))
}

const CAND_SQL: &str = "SELECT w.id, w.ax, w.ay, w.bx, w.by, l.elevation + w.base_offset, w.height, w.top_auto, w.top_roof, w.bulge      FROM wall w JOIN level l ON l.id = w.level      WHERE (w.top_auto = 1 OR (w.top_auto = 0 AND w.top_profile IS NULL AND w.top_roof IS NULL))        AND w.id NOT IN (SELECT wall FROM wall_attach WHERE side='top')        AND w.id NOT IN (SELECT wall FROM wall_attach_level)        AND w.id NOT IN (SELECT wall FROM wall_profile)";

/// Free walls (neither attached yet nor released) and walls driven by the rule.
fn candidates(db: &Connection, only: Option<&[i64]>) -> Result<Vec<Cand>, Error> {
    let row = |r: &rusqlite::Row| {
        Ok(Cand {
            id: r.get(0)?,
            a: (r.get(1)?, r.get(2)?),
            b: (r.get(3)?, r.get(4)?),
            base: r.get(5)?,
            height: r.get(6)?,
            auto: r.get(7)?,
            top_roof: r.get(8)?,
            bulge: r.get(9)?,
        })
    };
    let mut out = vec![];
    match only {
        Some(ids) => {
            let mut st = db.prepare_cached(&format!("{CAND_SQL} AND w.id=?1"))?;
            for id in ids {
                for c in st.query_map([id], row)? {
                    out.push(c?);
                }
            }
        }
        None => {
            let mut st = db.prepare_cached(&format!("{CAND_SQL} ORDER BY w.id"))?;
            for c in st.query_map([], row)? {
                out.push(c?);
            }
        }
    }
    Ok(out)
}

/// Slabs lying above the wall top (within reach): (u0, u1, elevation) of the axis stretches under
/// them. A lower-storey wall does not rise above its own slab, and where there is no slab above
/// it (top storey, a one-storey part of the house) it extends all the way to the roof.
fn floor_caps(floors: &[Floor], c: &Cand, rl: &Rules) -> Vec<(f64, f64, f64)> {
    let mut out = vec![];
    for pc in c.pieces() {
        let sub = c.straight(&pc);
        out.extend(floor_caps_line(floors, &sub, rl).into_iter().map(|(u0, u1, z)| (pc.u0 + u0 * pc.k, pc.u0 + u1 * pc.k, z)));
    }
    out
}

fn floor_caps_line(floors: &[Floor], c: &Cand, rl: &Rules) -> Vec<(f64, f64, f64)> {
    let flat = c.base + c.height;
    let (a, b) = (v2(c.a.0, c.a.1), v2(c.b.0, c.b.1));
    let (d, len) = (b - a, (b - a).len());
    let mut out = vec![];
    for fl in floors.iter().filter(|f| (flat - rl.reach..=flat + rl.reach).contains(&f.z)) {
        let mut ts = vec![0.0, 1.0];
        ts.extend(ddc_geom::poly::crossings(a, b, &fl.rings));
        // a wall lying along a slab edge has no crossings: split at the ring vertices it passes
        let d2 = d.dot(d);
        if d2 > 1e-18 {
            for &v in fl.rings.iter().flatten() {
                let s = (v - a).dot(d) / d2;
                if s > 1e-9 && s < 1.0 - 1e-9 && (a + d * s - v).len() < 1e-9 {
                    ts.push(s);
                }
            }
        }
        ts.sort_by(f64::total_cmp);
        for w in ts.windows(2) {
            if w[1] - w[0] < 1e-9 {
                continue;
            }
            let m = a + d * ((w[0] + w[1]) / 2.0);
            // the edge belongs to the slab; a hole's edge too
            let in_outer = fl.rings.first().is_some_and(|r| ddc_geom::predicates::point_in_ring(m, r) >= 0);
            let in_hole = fl.rings.iter().skip(1).any(|r| ddc_geom::predicates::point_in_ring(m, r) > 0);
            if in_outer && !in_hole {
                out.push((w[0] * len, w[1] * len, fl.z));
            }
        }
    }
    out
}

/// A slab: top elevation and outlines (outer, holes); read once per `sync`.
struct Floor {
    z: f64,
    rings: Vec<Vec<ddc_geom::V2>>,
}

fn floors(db: &Connection) -> Result<Vec<Floor>, Error> {
    let mut st = db.prepare_cached("SELECT s.id, l.elevation + s.offset FROM slab s JOIN level l ON l.id = s.level")?;
    let slabs: Vec<(i64, f64)> = st.query_map([], |r| Ok((r.get(0)?, r.get(1)?)))?.collect::<Result<_, _>>()?;
    let mut out = vec![];
    for (id, z) in slabs {
        let rings = crate::slabs::rings(db, id)?.iter().map(|r| r.iter().map(|p| v2(p.0, p.1)).collect()).collect();
        out.push(Floor { z, rings });
    }
    Ok(out)
}

/// Top `pts` (u, h), on [u0, u1] no higher than `hc`; intersections with the level are vertices.
fn clamp_top(pts: &[(f64, f64)], u0: f64, u1: f64, hc: f64) -> Vec<(f64, f64)> {
    let mut out: Vec<(f64, f64)> = vec![];
    let mut push = |p: (f64, f64)| {
        if out.last().map_or(true, |l| (l.0 - p.0).abs() > EPS || (l.1 - p.1).abs() > EPS) {
            out.push(p);
        }
    };
    for w in pts.windows(2) {
        let (p, q) = (w[0], w[1]);
        let du = q.0 - p.0;
        if du <= EPS {
            let inside = p.0 >= u0 - EPS && p.0 <= u1 + EPS;
            push(if inside { (p.0, p.1.min(hc)) } else { p });
            push(if inside { (q.0, q.1.min(hc)) } else { q });
            continue;
        }
        let at = |t: f64| (p.0 + du * t, p.1 + (q.1 - p.1) * t);
        let mut cuts = vec![0.0, 1.0];
        for u in [u0, u1] {
            let t = (u - p.0) / du;
            if t > 1e-9 && t < 1.0 - 1e-9 {
                cuts.push(t);
            }
        }
        if (q.1 - p.1).abs() > EPS {
            let t = (hc - p.1) / (q.1 - p.1);
            if t > 1e-9 && t < 1.0 - 1e-9 {
                cuts.push(t);
            }
        }
        cuts.sort_by(f64::total_cmp);
        for s in cuts.windows(2) {
            let (m, x0, x1) = (at((s[0] + s[1]) / 2.0), at(s[0]), at(s[1]));
            let inside = m.0 >= u0 && m.0 <= u1;
            for x in [x0, x1] {
                push(if inside { (x.0, x.1.min(hc)) } else { x });
            }
        }
    }
    if out.is_empty() {
        pts.to_vec()
    } else {
        out
    }
}

/// Segments (u0, z0, u1, z1) of the roof underside along the axis.
fn segments(r: &Ctx, c: &Cand, core: bool) -> Vec<[f64; 4]> {
    // continuous roof stretches separately: the top does not extend across a cutout or void
    let mut runs: Vec<Vec<(f64, f64)>> = vec![];
    for pc in c.pieces() {
        let sub = c.straight(&pc);
        let part = match (&r.geom, &r.shell) {
            (Some(g), _) => g.underside_runs(v2(sub.a.0, sub.a.1), v2(sub.b.0, sub.b.1), if core { r.ts } else { r.t }),
            (None, Some(sh)) => shell_runs(sh, &sub),
            _ => vec![],
        };
        // stretch chord -> length along the axis
        runs.extend(part.into_iter().map(|run| run.into_iter().map(|(u, z)| (pc.u0 + u * pc.k, z)).collect()));
    }
    runs.iter()
        .flat_map(|run| run.windows(2))
        .filter(|w| w[1].0 - w[0].0 > EPS)
        .map(|w| [w[0].0, w[0].1, w[1].0, w[1].1])
        .collect()
}

fn z_at(s: &[f64; 4], u: f64) -> f64 {
    s[1] + (s[3] - s[1]) * (u - s[0]) / (s[2] - s[0])
}

/// Top profile (u, h above the bottom) by the lowest of the roofs; None means the wall is not under a roof.
fn profile(c: &Cand, roofs: &[&Ctx], core: bool, rl: &Rules, caps: &[(f64, f64, f64)]) -> Option<(Vec<(f64, f64)>, i64)> {
    let len = c.len();
    if len < EPS {
        return None;
    }
    let segs: Vec<(i64, [f64; 4])> = roofs
        .iter()
        .flat_map(|r| segments(r, c, core).into_iter().map(|s| (r.id, s)))
        .collect();
    if segs.is_empty() {
        return None;
    }
    let mut us = vec![0.0, len];
    for (_, s) in &segs {
        us.push(s[0]);
        us.push(s[2]);
    }
    // intersections of slopes of different roofs within a shared stretch
    for (i, (ri, a)) in segs.iter().enumerate() {
        for (rj, b) in &segs[i + 1..] {
            let (lo, hi) = (a[0].max(b[0]), a[2].min(b[2]));
            if ri == rj || hi - lo < EPS {
                continue;
            }
            let (da, db_) = (z_at(a, lo) - z_at(b, lo), z_at(a, hi) - z_at(b, hi));
            if da * db_ < 0.0 {
                us.push(lo + (hi - lo) * da / (da - db_));
            }
        }
    }
    us.retain(|u| *u >= -EPS && *u <= len + EPS);
    us.sort_by(f64::total_cmp);
    us.dedup_by(|x, y| (*x - *y).abs() < EPS);
    let flat = c.base + c.height;
    let mut pts: Vec<(f64, f64)> = vec![];
    let mut cover = 0.0;
    let mut by_roof: Vec<(i64, f64)> = vec![];
    for w in us.windows(2) {
        let (ua, ub) = (w[0], w[1]);
        let m = (ua + ub) / 2.0;
        let best = segs
            .iter()
            .filter(|(_, s)| s[0] - EPS <= m && m <= s[2] + EPS)
            .min_by(|x, y| z_at(&x.1, m).total_cmp(&z_at(&y.1, m)));
        let (za, zb) = match best {
            Some((r, s)) => {
                cover += ub - ua;
                match by_roof.iter_mut().find(|e| e.0 == *r) {
                    Some(e) => e.1 += ub - ua,
                    None => by_roof.push((*r, ub - ua)),
                }
                (z_at(s, ua), z_at(s, ub))
            }
            None => (flat, flat),
        };
        for (u, z) in [(ua, za), (ub, zb)] {
            let h = z - c.base;
            match pts.last() {
                Some(l) if (l.0 - u).abs() < EPS && (l.1 - h).abs() < EPS => {}
                _ => pts.push((u, h)),
            }
        }
    }
    // the wall is below the minimum height everywhere (the roof is under its base): not under this roof
    if cover < rl.min_cover * len || pts.iter().all(|p| p.1 < rl.min_height - EPS) {
        return None;
    }
    // the next storey's slab above the wall: the top is no higher than it, above it are that storey's walls;
    // when it hits the base, the wall stays at the minimum height instead of losing its trim
    for &(u0, u1, z) in caps {
        pts = clamp_top(&pts, u0, u1, z - c.base);
    }
    for p in pts.iter_mut() {
        // a profile needs a positive height: a zero minimum still leaves a hair
        p.1 = p.1.max(rl.min_height).max(1e-6);
    }
    // collinear points
    let mut i = 1;
    while i + 1 < pts.len() {
        let (p, q, r) = (pts[i - 1], pts[i], pts[i + 1]);
        if ((q.0 - p.0) * (r.1 - p.1) - (q.1 - p.1) * (r.0 - p.0)).abs() < 1e-12 && q.0 > p.0 && r.0 > q.0 {
            pts.remove(i);
        } else {
            i += 1;
        }
    }
    let primary = by_roof.iter().max_by(|x, y| x.1.total_cmp(&y.1))?.0;
    Some((pts, primary))
}

fn release(db: &Connection, inv: Inv, w: i64) -> Result<Inv, Error> {
    let inv = snap(db, inv, w)?;
    db.execute(
        "UPDATE wall SET top_profile=NULL, top_core=NULL, top_roof=NULL, top_auto=0 WHERE id=?1",
        [w],
    )?;
    Ok(inv)
}

/// From this many walls in one sync the roofs are found through a grid of their plan boxes.
const GRID_FROM: usize = 32;
/// Side of a grid cell, m.
const CELL: f64 = 20.0;

thread_local! {
    /// (walls measured against a roof, wall-roof pairs measured) since the last [`take_counts`].
    static COUNTS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// The work counters of this thread since the last call, reset: a guard against a sync that
/// measures walls against roofs far from them.
pub(crate) fn take_counts() -> (usize, usize) {
    COUNTS.with(|n| n.replace((0, 0)))
}

/// Plan box of a roof: its faces, or the plan box of a shell.
fn plan_box(c: &Ctx) -> Option<[f64; 4]> {
    if let Some(sh) = &c.shell {
        return Some(sh.bb);
    }
    let g = c.geom.as_ref()?;
    let mut b = [f64::INFINITY, f64::INFINITY, f64::NEG_INFINITY, f64::NEG_INFINITY];
    for p in g.faces.iter().flat_map(|f| &f.pts) {
        b = [b[0].min(p.x), b[1].min(p.y), b[2].max(p.x), b[3].max(p.y)];
    }
    (b[0] <= b[2]).then_some(b)
}

/// Plan box of a wall axis; an arc bulges out of its chord box by at most |bulge| * chord / 2.
fn wall_box(c: &Cand) -> [f64; 4] {
    let s = c.bulge.abs() * ((c.b.0 - c.a.0).abs() + (c.b.1 - c.a.1).abs()) / 2.0 + EPS;
    [c.a.0.min(c.b.0) - s, c.a.1.min(c.b.1) - s, c.a.0.max(c.b.0) + s, c.a.1.max(c.b.1) + s]
}

fn overlaps(a: [f64; 4], b: [f64; 4]) -> bool {
    a[0] <= b[2] && b[0] <= a[2] && a[1] <= b[3] && b[1] <= a[3]
}

/// Uniform grid of roof plan boxes: cell -> indices of the roofs over it.
struct Grid {
    cells: std::collections::HashMap<(i64, i64), Vec<usize>>,
}

impl Grid {
    fn span(b: [f64; 4]) -> ((i64, i64), (i64, i64)) {
        let c = |v: f64| (v / CELL).floor() as i64;
        ((c(b[0]), c(b[1])), (c(b[2]), c(b[3])))
    }

    fn new(boxes: &[Option<[f64; 4]>]) -> Self {
        let mut cells: std::collections::HashMap<(i64, i64), Vec<usize>> = Default::default();
        for (k, b) in boxes.iter().enumerate() {
            let Some(b) = b else { continue };
            let ((x0, y0), (x1, y1)) = Self::span(*b);
            for x in x0..=x1 {
                for y in y0..=y1 {
                    cells.entry((x, y)).or_default().push(k);
                }
            }
        }
        Grid { cells }
    }

    /// Roofs over any cell of `b`, each once, in their order.
    fn query(&self, b: [f64; 4]) -> Vec<usize> {
        let ((x0, y0), (x1, y1)) = Self::span(b);
        let mut out: Vec<usize> = (x0..=x1)
            .flat_map(|x| (y0..=y1).map(move |y| (x, y)))
            .filter_map(|k| self.cells.get(&k))
            .flatten()
            .copied()
            .collect();
        out.sort_unstable();
        out.dedup();
        out
    }
}

/// Lowest point of a roof's underside (its contour's bottom); `None` for a shell, which is
/// measured from its plane height.
fn bottom(c: &Ctx) -> Option<f64> {
    let g = c.geom.as_ref()?;
    let lo = g.faces.iter().flat_map(|f| &f.pts).map(|p| p.z).fold(f64::INFINITY, f64::min);
    lo.is_finite().then(|| lo - c.t)
}

/// Attaches by the rule and recomputes the walls driven by it (all or `only`).
pub(crate) fn sync(db: &Connection, mut inv: Inv, only: Option<&[i64]>) -> Result<Inv, Error> {
    let roofs = bases(db)?;
    let cands = candidates(db, only)?;
    if cands.is_empty() {
        return Ok(inv);
    }
    let rl = rules(db);
    let mut cache: std::collections::HashMap<i64, Option<Ctx>> = Default::default();
    let mut floor_cache: Option<Vec<Floor>> = None;
    // many walls at once: every roof's plan box goes into a grid, so a wall meets only the roofs
    // over its own cells; a few walls look at each roof (the shapes are built for the near ones)
    let grid = if cands.len() >= GRID_FROM {
        for &(id, _) in &roofs {
            if !cache.contains_key(&id) {
                cache.insert(id, context_of(db, id)?);
            }
        }
        Some(Grid::new(&roofs.iter().map(|r| cache[&r.0].as_ref().and_then(plan_box)).collect::<Vec<_>>()))
    } else {
        None
    };
    for c in cands {
        let flat = c.base + c.height;
        let wb = wall_box(&c);
        let pool: Vec<usize> = match &grid {
            Some(g) => g.query(wb),
            None => (0..roofs.len()).collect(),
        };
        // roofs the wall top reaches: shapes are built only for them and once per call.
        // The plane height is the eave only for a plain roof: a profile or a slope hanging below
        // it (a lifted piece) reaches lower, so a roof above the wall is measured by its contour
        let mut near_ids = vec![];
        for k in pool {
            let (id, base) = roofs[k];
            let by_plane = flat >= base - rl.reach;
            if !cache.contains_key(&id) && (by_plane || grid.is_none()) {
                cache.insert(id, context_of(db, id)?);
            }
            let Some(ctx) = cache.get(&id).and_then(|x| x.as_ref()) else { continue };
            if !plan_box(ctx).is_some_and(|b| overlaps(b, wb)) {
                continue;
            }
            // the contour's lowest point (an overhang edge) is only a bound: the underside over
            // this wall's own axis must come within reach of its top
            let reaches = by_plane
                || (bottom(ctx).is_some_and(|lo| flat >= lo - rl.reach)
                    && segments(ctx, &c, false).iter().any(|s| flat >= s[1].min(s[3]) - rl.reach));
            if reaches {
                near_ids.push(id);
            }
        }
        if !near_ids.is_empty() {
            COUNTS.with(|n| {
                let (w, p) = n.get();
                n.set((w + 1, p + near_ids.len()));
            });
        }
        if near_ids.is_empty() && c.auto == 0 {
            continue;
        }
        if c.auto == 0 && (!crate::walls::stored_lean(db, c.id)?.is_plain() || crate::wall_spline::refuse_piece(db, c.id).is_err()) {
            continue;
        }
        for id in &near_ids {
            if !cache.contains_key(id) {
                cache.insert(*id, context_of(db, *id)?);
            }
        }
        let near: Vec<&Ctx> = near_ids.iter().filter_map(|id| cache[id].as_ref()).collect();
        let caps = if near.is_empty() {
            vec![]
        } else {
            if floor_cache.is_none() {
                floor_cache = Some(floors(db)?);
            }
            floor_caps(floor_cache.as_deref().unwrap_or(&[]), &c, &rl)
        };
        let got = if near.is_empty() { None } else { profile(&c, &near, false, &rl, &caps) };
        match got {
            Some((top, primary)) => {
                let text = crate::walls::top_profile_text(&top);
                // the wall core follows the underside of the roof's bearing layer; if it matches the finish, there is no separate one
                let core = profile(&c, &near, true, &rl, &caps)
                    .map(|p| crate::walls::top_profile_text(&p.0))
                    .filter(|t| *t != text);
                if c.auto == 1 && c.top_roof == (primary > 0).then_some(primary) && same_top(db, c.id, &text, core.as_deref())? {
                    continue;
                }
                inv = snap(db, inv, c.id)?;
                db.execute(
                    "UPDATE wall SET top_profile=?2, top_roof=?3, top_auto=1, top_core=?4 WHERE id=?1",
                    params![c.id, text, (primary > 0).then_some(primary), core],
                )?;
            }
            None if c.auto == 1 => inv = release(db, inv, c.id)?,
            None => {}
        }
    }
    Ok(inv)
}

fn same_top(db: &Connection, wall: i64, text: &str, core: Option<&str>) -> Result<bool, Error> {
    let old: (Option<String>, Option<String>) =
        db.query_row("SELECT top_profile, top_core FROM wall WHERE id=?1", [wall], |r| Ok((r.get(0)?, r.get(1)?)))?;
    Ok(old.0.as_deref() == Some(text) && old.1.as_deref() == core)
}

/// `sync` for a batch edit that wrote wall rows itself (the editor's drag path).
/// After a plan edit (move, turn, mirror, scale, copy) of `walls` and `roofs`: retrims the
/// moved walls, the walls the moved roofs drove and the walls under their new extent. Nothing
/// runs when the model has no roof, or when no moved wall is driven or reaches a roof.
pub(crate) fn retrim_after_xf(db: &Connection, snaps: &mut crate::Snapshots, walls: &[i64], roofs: &[i64]) -> Result<(), Error> {
    let any_roof: bool = db.query_row(
        "SELECT EXISTS(SELECT 1 FROM roof) OR EXISTS(SELECT 1 FROM shell_elem WHERE kind='roof')",
        [],
        |r| r.get(0),
    )?;
    if !any_roof || (walls.is_empty() && roofs.is_empty()) {
        return Ok(());
    }
    let mut ids = walls.to_vec();
    for &r in roofs {
        let mut st = db.prepare_cached("SELECT id FROM wall WHERE top_roof=?1")?;
        ids.extend(st.query_map([r], |q| q.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?);
        let Some(Ctx { geom: Some(g), .. }) = context(db, r)? else { continue };
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        for p in g.faces.iter().flat_map(|f| &f.pts) {
            lo = (lo.0.min(p.x), lo.1.min(p.y));
            hi = (hi.0.max(p.x), hi.1.max(p.y));
        }
        if lo.0 > hi.0 {
            continue;
        }
        // an arc wall bulges out of its chord box by at most |bulge| * chord / 2
        let mut st = db.prepare_cached(
            "SELECT id FROM wall WHERE max(ax,bx) + abs(bulge)*(abs(bx-ax)+abs(by-ay))/2 >= ?1 \
             AND min(ax,bx) - abs(bulge)*(abs(bx-ax)+abs(by-ay))/2 <= ?2 \
             AND max(ay,by) + abs(bulge)*(abs(bx-ax)+abs(by-ay))/2 >= ?3 \
             AND min(ay,by) - abs(bulge)*(abs(bx-ax)+abs(by-ay))/2 <= ?4",
        )?;
        ids.extend(st.query_map(rusqlite::params![lo.0, hi.0, lo.1, hi.1], |q| q.get::<_, i64>(0))?.collect::<Result<Vec<_>, _>>()?);
    }
    ids.sort_unstable();
    ids.dedup();
    sync_roof_walls(db, snaps, &ids)
}

pub fn sync_roof_walls(db: &Connection, snaps: &mut crate::Snapshots, ids: &[i64]) -> Result<(), Error> {
    let inv = sync(db, Inv::default(), Some(ids))?;
    snaps.push_first(inv);
    Ok(())
}

impl crate::Model {
    /// The roof rule leads this wall's top (it follows the roof by itself).
    pub fn wall_top_auto(&self, wall: i64) -> bool {
        self.db
            .query_row("SELECT top_auto=1 FROM wall WHERE id=?1", [wall], |r| r.get(0))
            .unwrap_or(false)
    }
}
