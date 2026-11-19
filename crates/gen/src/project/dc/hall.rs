//! The data halls: rack rows in hot and cold aisles between two walls of cooling units, the
//! rows running from one unit wall to the other so no rack is farther than `dc.crah_throw`;
//! the cooling units, their chilled water headers in the floor void joined into a ring, and
//! two cable trays over each row on their own trunks.

use ddc_geom::math::{v3, V3};

use std::f64::consts::FRAC_PI_2;

use super::why::{n2, s, Whys};
use super::{columns, count_rule, dn_for, flow_of, footprint, pipe, plan_box, port_world, svc_box, tray, types, DcRoom, DcUnit, GDc, HallFact, Kind, Lib, Net, EPS};
use super::super::rules::rule;
use super::super::svc::Side;
use super::super::GProject;

/// A run end the plant joins: its point, the run and which end.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Tap {
    pub at: V3,
    pub run: usize,
    pub start: bool,
}

/// A laid hall as the plant sees it.
pub(crate) struct LaidHall {
    pub storey: usize,
    pub name: String,
    pub it_kw: f64,
    pub heat_kw: f64,
    /// Chilled water taps per path (wall): supply, return.
    pub chw: [[Option<Tap>; 2]; 2],
    /// Tray trunk starts per power path.
    pub feed: [Option<Tap>; 2],
}

/// A rack position: plan point along and across the rows, the side its front faces, its row.
#[derive(Clone, Copy, Debug)]
struct Slot {
    u: f64,
    v: f64,
    front: f64,
    row: usize,
}

/// A hall's frame and rack positions: `u` along the rows between the unit walls.
struct Plan {
    along_x: bool,
    ua: f64,
    ub: f64,
    va: f64,
    vb: f64,
    slots: Vec<Slot>,
}

impl Plan {
    fn xy(&self, u: f64, v: f64) -> (f64, f64) {
        if self.along_x {
            (u, v)
        } else {
            (v, u)
        }
    }
    fn v_of(&self, p: V3) -> f64 {
        if self.along_x {
            p.y
        } else {
            p.x
        }
    }
    fn span(&self) -> f64 {
        self.ub - self.ua
    }
}

/// Turn of a family whose front (local -y) faces the plan direction `f`.
fn facing(f: (f64, f64)) -> f64 {
    f.0.atan2(-f.1)
}

fn plan_hall(p: &GProject, h: &DcRoom, rack: &Lib, cd: f64) -> Plan {
    let c = h.room.clear;
    let cols = columns(p, h.storey);
    let (cold, hot, end) = (rule("dc.cold_aisle"), rule("dc.hot_aisle"), rule("dc.row_end"));
    let (row_max, cross, gap) = (count_rule("dc.row_max").max(1), rule("dc.cross_aisle"), rule("dc.unit_gap"));
    let ue = cd + rule("dc.crah_clear");
    let (rw, rd) = (rack.w(), rack.d());
    let mut best: Option<(bool, usize, Plan)> = None;
    for along_x in [true, false] {
        let (ua, ub, va, vb) = if along_x { (c[0], c[2], c[1], c[3]) } else { (c[1], c[3], c[0], c[2]) };
        let mut plan = Plan { along_x, ua, ub, va, vb, slots: vec![] };
        let mut us = vec![];
        let mut x = ua + ue;
        while x + rw <= ub - ue + EPS {
            us.push(x + rw / 2.0);
            x += rw;
            if us.len() % row_max == 0 {
                x += cross;
            }
        }
        let mut v = va + end;
        let mut row = 0;
        while v + rd * 2.0 + cold <= vb - end + EPS {
            for (axis, front) in [(v + rd / 2.0, 1.0), (v + rd * 1.5 + cold, -1.0)] {
                for &u in &us {
                    let at = plan.xy(u, axis);
                    let f = plan.xy(0.0, front);
                    let fp = footprint(rack, at, facing(f), gap);
                    if cols.iter().all(|k| !svc_box::hit(&fp, k)) {
                        plan.slots.push(Slot { u, v: axis, front, row });
                    }
                }
                row += 1;
            }
            v += rd * 2.0 + cold + hot;
        }
        let ok = plan.span() / 2.0 <= rule("dc.crah_throw") + EPS;
        let better = match &best {
            None => true,
            // the throw decides only between orientations that both seat racks
            Some((bok, bn, _)) if *bn > 0 && !plan.slots.is_empty() => (ok, plan.slots.len()) > (*bok, *bn),
            Some((_, bn, _)) => plan.slots.len() > *bn,
        };
        if better {
            best = Some((ok, plan.slots.len(), plan));
        }
    }
    best.map(|b| b.2).unwrap_or(Plan { along_x: true, ua: c[0], ub: c[2], va: c[1], vb: c[3], slots: vec![] })
}

/// `asked` racks shared over the halls by what each holds; 0 fills them.
fn share(asked: usize, caps: &[usize]) -> Vec<usize> {
    let total: usize = caps.iter().sum();
    if asked == 0 || asked >= total {
        return caps.to_vec();
    }
    let mut out: Vec<usize> = caps.iter().map(|c| asked * c / total).collect();
    let mut left = asked - out.iter().sum::<usize>();
    for (o, c) in out.iter_mut().zip(caps) {
        let more = left.min(c - *o);
        *o += more;
        left -= more;
    }
    out
}

pub(crate) fn lay_halls(p: &GProject, halls: &[DcRoom], dc: &mut GDc, w: &mut Whys) -> Vec<LaidHall> {
    let (Some(rack), crahs) = (types(Kind::Rack).into_iter().next(), types(Kind::Crah)) else { return vec![] };
    if crahs.is_empty() {
        return vec![];
    }
    let cd = crahs.iter().map(|c| c.d()).fold(0.0, f64::max);
    let plans: Vec<Plan> = halls.iter().map(|h| plan_hall(p, h, &rack, cd)).collect();
    let caps: Vec<usize> = plans.iter().map(|q| q.slots.len()).collect();
    let asked = count_rule("dc.racks");
    let want = share(asked, &caps);
    let fit: usize = caps.iter().sum();
    if asked > fit {
        w.add("dc", "dc.racks", "dc_racks_short", vec![("asked", s(asked)), ("n", s(fit))]);
    }
    w.add("dc", "dc.raised_floor", "dc_raised", vec![("h", n2(rule("dc.raised_floor")))]);
    let mut out = vec![];
    for ((h, plan), n) in halls.iter().zip(&plans).zip(want) {
        out.push(lay_hall(p, h, plan, n, &rack, &crahs, cd, dc, w));
    }
    let trays = dc.trays();
    w.add(
        "dc",
        "dc.tray_gap",
        "dc_trays",
        vec![("n", s(trays)), ("w", n2(rule("dc.tray_w"))), ("h", n2(rule("dc.tray_h"))), ("gap", n2(rule("dc.tray_gap")))],
    );
    out
}

/// Even spacing of `n` units with their backs on `side` between `s_lo` and `s_hi`, each
/// moved within its share to clear doors and columns; `None` when one does not fit.
fn place_even(side: &Side, n: usize, l: &Lib, s_lo: f64, s_hi: f64, cols: &[svc_box::R]) -> Option<Vec<f64>> {
    let gap = rule("dc.unit_gap");
    if n == 0 {
        return Some(vec![]);
    }
    let pitch = (s_hi - s_lo) / n as f64;
    if pitch < l.w() + gap - EPS {
        return None;
    }
    let step = l.w() / 4.0;
    let lim = (pitch - l.w()) / 2.0;
    let ok = |x: f64| {
        let fp = footprint(l, side.at(x, l.hi.y), side.back_rot(), gap);
        side.free(x - l.w() / 2.0, x + l.w() / 2.0, gap, 0.0, l.h()) && cols.iter().all(|k| !svc_box::hit(&fp, k))
    };
    let mut out = vec![];
    for i in 0..n {
        let target = s_lo + (i as f64 + 0.5) * pitch;
        let mut k = 0;
        let found = loop {
            let off = k as f64 * step;
            if off > lim + EPS {
                break None;
            }
            if let Some(x) = [target + off, target - off].into_iter().find(|x| ok(*x)) {
                break Some(x);
            }
            k += 1;
        };
        out.push(found?);
    }
    Some(out)
}

/// Pieces of the axis line `a..b` (along u at a fixed v, or along v) clear of the columns
/// grown by `r`: each piece and whether its start and end were cut.
fn clear_pieces(a: (f64, f64), b: (f64, f64), r: f64, cols: &[svc_box::R]) -> Vec<((f64, f64), (f64, f64), bool, bool)> {
    let horiz = (a.1 - b.1).abs() < EPS;
    let (t0, t1) = if horiz { (a.0, b.0) } else { (a.1, b.1) };
    let (lo, hi, rev) = if t0 <= t1 { (t0, t1, false) } else { (t1, t0, true) };
    let line = if horiz { a.1 } else { a.0 };
    let mut cuts: Vec<(f64, f64)> = cols
        .iter()
        .filter(|k| if horiz { k[1] - r < line && line < k[3] + r } else { k[0] - r < line && line < k[2] + r })
        .map(|k| if horiz { (k[0] - r, k[2] + r) } else { (k[1] - r, k[3] + r) })
        .filter(|c| c.1 > lo && c.0 < hi)
        .collect();
    cuts.sort_by(|x, y| x.0.total_cmp(&y.0));
    let mut out = vec![];
    let (mut x, mut cut_start) = (lo, false);
    for c in cuts {
        if c.0 > x + EPS {
            out.push((x, c.0, cut_start, true));
        }
        x = x.max(c.1);
        cut_start = true;
    }
    if hi > x + EPS {
        out.push((x, hi, cut_start, false));
    }
    let pt = |t: f64| if horiz { (t, line) } else { (line, t) };
    let mut v: Vec<_> = out.into_iter().map(|(x0, x1, c0, c1)| (pt(x0), pt(x1), c0, c1)).collect();
    if rev {
        v = v.into_iter().rev().map(|(p, q, c0, c1)| (q, p, c1, c0)).collect();
    }
    v
}

/// The axis line `a..b` clears every column grown by `r`.
fn clear(a: (f64, f64), b: (f64, f64), r: f64, cols: &[svc_box::R]) -> bool {
    let bx = [a.0.min(b.0) - r, a.1.min(b.1) - r, a.0.max(b.0) + r, a.1.max(b.1) + r];
    cols.iter().all(|k| !svc_box::hit(&bx, k))
}

#[allow(clippy::too_many_arguments)]
fn lay_hall(p: &GProject, h: &DcRoom, plan: &Plan, n: usize, rack: &Lib, crahs: &[Lib], cd: f64, dc: &mut GDc, w: &mut Whys) -> LaidHall {
    let st = &p.storeys[h.storey];
    let elev = st.elevation;
    let cols = columns(p, h.storey);
    let raised = rule("dc.raised_floor");
    let taken: Vec<Slot> = plan.slots.iter().copied().take(n).collect();
    for sl in &taken {
        let at = plan.xy(sl.u, sl.v);
        let rot = facing(plan.xy(0.0, sl.front));
        dc.units.push(DcUnit { kind: Kind::Rack, family: rack.family.clone(), ty: rack.ty.clone(), storey: h.storey, at, z: raised, rot, path: 0 });
    }
    let mut rows: Vec<usize> = taken.iter().map(|x| x.row).collect();
    rows.dedup();
    let it_kw = taken.len() as f64 * rule("dc.rack_kw");
    let heat = it_kw * (1.0 + rule("dc.cooling_margin"));
    let name = h.name.clone();
    let (span, throw) = (n2(plan.span()), n2(rule("dc.crah_throw")));
    if plan.span() / 2.0 <= rule("dc.crah_throw") + EPS {
        w.add(&name, "dc.crah_throw", "dc_orient", vec![("hall", name.clone()), ("span", span), ("throw", throw)]);
    } else {
        w.add(&name, "dc.crah_throw", "dc_throw_over", vec![("hall", name.clone()), ("span", span), ("throw", throw)]);
    }
    w.add(
        &name,
        "dc.cold_aisle",
        "dc_rows",
        vec![
            ("hall", name.clone()),
            ("n", s(taken.len())),
            ("rows", s(rows.len())),
            ("cold", n2(rule("dc.cold_aisle"))),
            ("hot", n2(rule("dc.hot_aisle"))),
        ],
    );
    let mut laid = LaidHall { storey: h.storey, name: name.clone(), it_kw, heat_kw: heat, chw: [[None; 2]; 2], feed: [None; 2] };
    if taken.is_empty() {
        dc.halls.push(HallFact { name, storey: h.storey, racks: 0, rows: 0, it_kw, crah: 0 });
        return laid;
    }
    // cooling units: walls at u = ua (path A) and u = ub (path B)
    let sides = &h.room.sides;
    let walls: [&Side; 2] = if plan.along_x { [&sides[3], &sides[1]] } else { [&sides[0], &sides[2]] };
    let end = rule("dc.row_end");
    // the side's s runs along v whatever u the point has
    let s_range = |side: &Side| {
        let (a, b) = (side.s_of(plan.xy(plan.ua, plan.va + end)), side.s_of(plan.xy(plan.ua, plan.vb - end)));
        (a.min(b), a.max(b))
    };
    let spare = count_rule("dc.crah_spare");
    let mut cands: Vec<&Lib> = crahs.iter().filter(|l| l.cap > 0.0).collect();
    let need_of = |l: &Lib| (heat / l.cap).ceil() as usize + spare;
    cands.sort_by(|a, b| {
        let (na, nb) = (need_of(a), need_of(b));
        (na as f64 * a.cap).total_cmp(&(nb as f64 * b.cap)).then(na.cmp(&nb))
    });
    let mut chosen: Option<(&Lib, [Vec<f64>; 2])> = None;
    for l in &cands {
        let total = need_of(l);
        let n1 = total.div_ceil(2);
        let r1 = s_range(walls[0]);
        let r2 = s_range(walls[1]);
        if let (Some(a), Some(b)) = (place_even(walls[0], n1, l, r1.0, r1.1, &cols), place_even(walls[1], total - n1, l, r2.0, r2.1, &cols)) {
            chosen = Some((l, [a, b]));
            break;
        }
    }
    let (crah, at_s) = match chosen {
        Some(c) => c,
        None => {
            // the largest unit, as many as each wall takes
            let Some(l) = cands.iter().max_by(|a, b| a.cap.total_cmp(&b.cap)).copied() else { return laid };
            let total = need_of(l);
            let mut got: [Vec<f64>; 2] = [vec![], vec![]];
            for (i, side) in walls.iter().enumerate() {
                let r = s_range(side);
                let mut k = total.div_ceil(2);
                while k > 0 {
                    if let Some(v) = place_even(side, k, l, r.0, r.1, &cols) {
                        got[i] = v;
                        break;
                    }
                    k -= 1;
                }
            }
            let placed = got[0].len() + got[1].len();
            w.add(
                &name,
                "dc.unit_gap",
                "dc_unplaced_crah",
                vec![("hall", name.clone()), ("n", s(placed)), ("need", s(total))],
            );
            (l, got)
        }
    };
    let ncrah = at_s[0].len() + at_s[1].len();
    w.red.push(("crah", need_of(crah).saturating_sub(spare), ncrah));
    w.add(
        &name,
        "dc.crah_spare",
        "dc_crah",
        vec![("hall", name.clone()), ("n", s(ncrah)), ("kw", n2(crah.cap)), ("heat", n2(heat)), ("spare", s(spare))],
    );
    let ports = super::data()["ports"].clone();
    let (sup, ret) = (ports["supply"].as_str().unwrap_or(""), ports["return"].as_str().unwrap_or(""));
    let (pitch, off) = (rule("dc.pipe_pitch"), rule("dc.pipe_off"));
    let (hdn, hv) = dn_for(flow_of(heat));
    let port_mm = crah.port(sup).map_or(0.0, |x| x.3);
    let bdn = branch_dn(flow_of(crah.cap), port_mm);
    w.add(&name, "dc.chw_v", "dc_dn_hall", vec![("hall", name.clone()), ("dn", s(hdn)), ("flow", n2(flow_of(heat) * 1000.0)), ("v", n2(hv))]);
    let hod = ddc_geom::mep::PipeMaterial::Steel.by_dn(hdn).map_or(0.0, |x| x.od);
    let hr = hod / 2.0 + rule("dc.unit_gap");
    let (va0, vb0) = (plan.va + end / 2.0, plan.vb - end / 2.0);
    // header lines: supply at d, return one pitch farther; moved off the columns
    let u_at = |wall: usize, d: f64| if wall == 0 { plan.ua + d } else { plan.ub - d };
    let mut ends: [Option<[(f64, f64); 2]>; 2] = [None, None];
    let mut last: [[Option<usize>; 2]; 2] = [[None; 2]; 2];
    let mut zs = (0.0, 0.0);
    for (wall, side) in walls.iter().enumerate() {
        let mut units = vec![];
        for &x in &at_s[wall] {
            let u = DcUnit {
                kind: Kind::Crah,
                family: crah.family.clone(),
                ty: crah.ty.clone(),
                storey: h.storey,
                at: side.at(x, crah.hi.y),
                z: 0.0,
                rot: side.back_rot(),
                path: wall as u8 + 1,
            };
            dc.units.push(u);
            units.push(dc.units.len() - 1);
        }
        let mut d = crah.d() + off;
        while d < crah.d() + rule("dc.crah_clear")
            && !(clear(plan.xy(u_at(wall, d), va0), plan.xy(u_at(wall, d), vb0), hr, &cols)
                && clear(plan.xy(u_at(wall, d + pitch), va0), plan.xy(u_at(wall, d + pitch), vb0), hr, &cols))
        {
            d += pitch;
        }
        for (k, (pname, net, line_d)) in [(sup, Net::ChwSupply, d), (ret, Net::ChwReturn, d + pitch)].into_iter().enumerate() {
            let uh = u_at(wall, line_d);
            let mut tees: Vec<(f64, usize, V3)> = vec![];
            for &ui in &units {
                let Some((pw, _)) = port_world(&dc.units[ui], crah, pname, elev) else { continue };
                tees.push((plan.v_of(pw), ui, pw));
            }
            let Some(z) = tees.first().map(|t| t.2.z) else { continue };
            if k == 0 {
                zs.0 = z;
            } else {
                zs.1 = z;
            }
            let at = |v: f64| {
                let q = plan.xy(uh, v);
                v3(q.0, q.1, z)
            };
            for &(v, ui, pw) in &tees {
                dc.runs.push(pipe(net, h.storey, bdn, &[pw, at(v)]));
                dc.links.push(super::DcLink { unit: ui, connector: pname.to_string(), net });
            }
            let mut vs: Vec<f64> = tees.iter().map(|t| t.0).collect();
            vs.push(va0);
            vs.push(vb0);
            vs.sort_by(f64::total_cmp);
            vs.dedup_by(|a, b| (*a - *b).abs() < EPS);
            let first = dc.runs.len();
            for pair in vs.windows(2) {
                dc.runs.push(pipe(net, h.storey, hdn, &[at(pair[0]), at(pair[1])]));
            }
            laid.chw[wall][k] = Some(Tap { at: at(va0), run: first, start: true });
            last[wall][k] = Some(dc.runs.len() - 1);
            let e = ends[k].get_or_insert([(0.0, 0.0); 2]);
            e[wall] = plan.xy(uh, vb0);
        }
    }
    // ring: the header ends of the two walls joined across the far end of the hall, else capped
    for (k, net) in [(0, Net::ChwSupply), (1, Net::ChwReturn)] {
        let both = laid.chw[0][k].is_some() && laid.chw[1][k].is_some();
        match ends[k] {
            Some(e) if both && clear(e[0], e[1], hr, &cols) => {
                let z = if k == 0 { zs.0 } else { zs.1 };
                dc.runs.push(pipe(net, h.storey, hdn, &[v3(e[0].0, e[0].1, z), v3(e[1].0, e[1].1, z)]));
            }
            _ => {
                for i in last.iter().filter_map(|x| x[k]) {
                    dc.runs[i].caps.1 = true;
                }
            }
        }
    }
    lay_trays(plan, h.storey, elev, &taken, rack, cd, &cols, dc, &mut laid);
    lay_ends(plan, h.storey, elev, &taken, rack, &cols, dc, w, &name);
    dc.halls.push(HallFact { name, storey: h.storey, racks: taken.len(), rows: rows.len(), it_kw, crah: ncrah });
    laid
}

/// The branch DN: the flow's, at least the connector's size.
pub(crate) fn branch_dn(flow: f64, port: f64) -> u32 {
    let (dn, _) = dn_for(flow);
    let sizes = ddc_geom::mep::PipeMaterial::Steel.sizes();
    let min = sizes.iter().map(|x| x.dn).find(|d| f64::from(*d) >= port * 1000.0 - EPS).unwrap_or(dn);
    dn.max(min)
}

/// Two trays over each row, A on the side of wall A and fed from its trunk there, B likewise;
/// the trays stop short of the columns with caps.
#[allow(clippy::too_many_arguments)]
fn lay_trays(plan: &Plan, storey: usize, elev: f64, taken: &[Slot], rack: &Lib, cd: f64, cols: &[svc_box::R], dc: &mut GDc, laid: &mut LaidHall) {
    let (tw, pitch, gap) = (rule("dc.tray_w"), rule("dc.pipe_pitch"), rule("dc.unit_gap"));
    let z = elev + rule("dc.raised_floor") + rack.h() + rule("dc.tray_gap") + rule("dc.tray_h") / 2.0;
    let r = tw / 2.0 + gap;
    let mut rows: Vec<(f64, f64, f64)> = vec![];
    for sl in taken {
        match rows.iter_mut().find(|x| (x.0 - sl.v).abs() < EPS) {
            Some(x) => {
                x.1 = x.1.min(sl.u);
                x.2 = x.2.max(sl.u);
            }
            None => rows.push((sl.v, sl.u, sl.u)),
        }
    }
    let half = rack.w() / 2.0;
    for (path, net) in [(0usize, Net::PowerA), (1, Net::PowerB)] {
        let side = if path == 0 { -1.0 } else { 1.0 };
        let base = cd + rule("dc.crah_clear") / 2.0;
        let mut ut = if path == 0 { plan.ua + base } else { plan.ub - base };
        let vs: Vec<f64> = rows.iter().map(|x| x.0 + side * pitch / 2.0).collect();
        let (v0, v1) = vs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &v| (a.0.min(v), a.1.max(v)));
        let mut moved = 0.0;
        while !clear(plan.xy(ut, v0), plan.xy(ut, v1), r, cols) && moved < rule("dc.crah_clear") / 2.0 {
            ut += if path == 0 { -tw } else { tw };
            moved += tw;
        }
        let at = |u: f64, v: f64| {
            let q = plan.xy(u, v);
            v3(q.0, q.1, z)
        };
        for (row, &v) in rows.iter().zip(&vs) {
            let far = if path == 0 { row.2 + half } else { row.1 - half };
            for (a, b, c0, c1) in clear_pieces(plan.xy(ut, v), plan.xy(far, v), r, cols) {
                let caps = (c0, c1 || (b.0 - plan.xy(far, v).0).abs() + (b.1 - plan.xy(far, v).1).abs() < EPS);
                dc.runs.push(tray(net, storey, &[v3(a.0, a.1, z), v3(b.0, b.1, z)], caps));
            }
        }
        let mut sorted = vs.clone();
        sorted.sort_by(f64::total_cmp);
        sorted.dedup_by(|a, b| (*a - *b).abs() < EPS);
        let first = dc.runs.len();
        for pair in sorted.windows(2) {
            let last = (pair[1] - sorted[sorted.len() - 1]).abs() < EPS;
            dc.runs.push(tray(net, storey, &[at(ut, pair[0]), at(ut, pair[1])], (false, last)));
        }
        if sorted.len() > 1 {
            laid.feed[path] = Some(Tap { at: at(ut, sorted[0]), run: first, start: true });
        } else if let Some(&v) = sorted.first() {
            // one row: its tray is the trunk
            let i = dc.runs.iter().rposition(|x| x.net == net && x.storey == storey && {
                let p0 = x.points[0];
                (p0.0 - at(ut, v).x).abs() + (p0.1 - at(ut, v).y).abs() < EPS
            });
            if let Some(i) = i {
                laid.feed[path] = Some(Tap { at: at(ut, v), run: i, start: true });
            }
        }
    }
}

/// A run passes through the box `b` between heights `z0` and `z1`, by its outer size.
fn runs_pass(dc: &GDc, storey: usize, b: &svc_box::R, z0: f64, z1: f64) -> bool {
    let step = rule("dc.tray_h") / 2.0;
    dc.runs.iter().filter(|r| r.storey == storey).any(|r| {
        let m = r.half();
        r.points.windows(2).any(|w| {
            let (dx, dy, dz) = (w[1].0 - w[0].0, w[1].1 - w[0].1, w[1].2 - w[0].2);
            let n = (((dx * dx + dy * dy + dz * dz).sqrt() / step).ceil() as usize).max(1);
            (0..=n).any(|i| {
                let f = i as f64 / n as f64;
                let q = (w[0].0 + dx * f, w[0].1 + dy * f, w[0].2 + dz * f);
                q.0 > b[0] - m && q.0 < b[2] + m && q.1 > b[1] - m && q.1 < b[3] + m && q.2 > z0 - m && q.2 < z1 + m
            })
        })
    })
}

/// The box is free of the columns, the units already placed and every run.
fn free_box(dc: &GDc, storey: usize, b: &svc_box::R, z0: f64, z1: f64, cols: &[svc_box::R]) -> bool {
    cols.iter().all(|k| !svc_box::hit(b, k))
        && dc.units.iter().filter(|u| u.storey == storey).filter_map(plan_box).all(|o| !svc_box::hit(b, &o))
        && !runs_pass(dc, storey, b, z0, z1)
}

/// Containment doors closing each cold aisle at both row ends, and the PDUs standing in line
/// with each row at its ends, A at the first end and B at the last. A spot that meets a column,
/// a unit or a run is left out.
#[allow(clippy::too_many_arguments)]
fn lay_ends(plan: &Plan, storey: usize, elev: f64, taken: &[Slot], rack: &Lib, cols: &[svc_box::R], dc: &mut GDc, w: &mut Whys, name: &str) {
    let z = elev + rule("dc.raised_floor");
    let half = rack.w() / 2.0;
    // per row: v, first and last u, the side its fronts face
    let mut rows: Vec<(usize, f64, f64, f64, f64)> = vec![];
    for sl in taken {
        match rows.iter_mut().find(|x| x.0 == sl.row) {
            Some(x) => {
                x.2 = x.2.min(sl.u);
                x.3 = x.3.max(sl.u);
            }
            None => rows.push((sl.row, sl.v, sl.u, sl.u, sl.front)),
        }
    }
    let cold = rule("dc.cold_aisle");
    let mut doors = 0;
    let mut types_d = types(Kind::Door);
    types_d.retain(|l| l.w() + EPS >= cold);
    types_d.sort_by(|a, b| a.w().total_cmp(&b.w()));
    if count_rule("dc.contain") > 0 {
        if let Some(door) = types_d.first() {
            let last_row = rows.iter().map(|x| x.0).max().unwrap_or(0);
            for k in 0..=last_row / 2 {
                let (Some(a), Some(b)) = (rows.iter().find(|x| x.0 == 2 * k), rows.iter().find(|x| x.0 == 2 * k + 1)) else { continue };
                let v = (a.1 + b.1) / 2.0;
                let (lo, hi) = (a.2.min(b.2) - half - door.d() / 2.0, a.3.max(b.3) + half + door.d() / 2.0);
                for u in [lo, hi] {
                    let at = plan.xy(u, v);
                    let rot = if plan.along_x { FRAC_PI_2 } else { 0.0 };
                    let fp = footprint(door, at, rot, 0.0);
                    if free_box(dc, storey, &fp, z, z + door.h(), cols) {
                        dc.units.push(DcUnit { kind: Kind::Door, family: door.family.clone(), ty: door.ty.clone(), storey, at, z: rule("dc.raised_floor"), rot, path: 0 });
                        doors += 1;
                    }
                }
            }
        }
    }
    if doors > 0 {
        let wd = types_d.first().map_or(0.0, |l| l.w());
        w.add(name, "dc.contain", "dc_contain", vec![("hall", name.to_string()), ("n", s(doors)), ("w", n2(wd))]);
    }
    let ends = count_rule("dc.pdu_ends").min(count_rule("dc.ups_paths").max(1));
    let mut pdus = 0;
    if let Some(pdu) = types(Kind::Pdu).first() {
        for &(_, v, first, last, front) in &rows {
            for (path, at_first) in [(0usize, true), (1, false)].into_iter().filter(|x| x.0 < ends) {
                let u = if at_first { first - half - pdu.w() / 2.0 } else { last + half + pdu.w() / 2.0 };
                let at = plan.xy(u, v);
                let rot = facing(plan.xy(0.0, front));
                let fp = footprint(pdu, at, rot, 0.0);
                if free_box(dc, storey, &fp, z, z + pdu.h(), cols) {
                    dc.units.push(DcUnit { kind: Kind::Pdu, family: pdu.family.clone(), ty: pdu.ty.clone(), storey, at, z: rule("dc.raised_floor"), rot, path: path as u8 + 1 });
                    pdus += 1;
                }
            }
        }
    }
    if pdus > 0 {
        w.add(name, "dc.pdu_ends", "dc_pdus", vec![("hall", name.to_string()), ("n", s(pdus)), ("rows", s(rows.len()))]);
    }
}

/// Racks a clear rectangle takes by the rules, columns ignored, for each row direction:
/// all racks and those in one row.
pub(crate) fn capacity(w: f64, d: f64) -> [(usize, usize); 2] {
    let (Some(rack), crahs) = (types(Kind::Rack).into_iter().next(), types(Kind::Crah)) else { return [(0, 0); 2] };
    let cd = crahs.iter().map(|c| c.d()).fold(0.0, f64::max);
    let (cold, hot, end) = (rule("dc.cold_aisle"), rule("dc.hot_aisle"), rule("dc.row_end"));
    let (row_max, cross) = (count_rule("dc.row_max").max(1), rule("dc.cross_aisle"));
    let ue = cd + rule("dc.crah_clear");
    let one = |along: f64, across: f64| {
        let (mut per, mut x) = (0, ue);
        while x + rack.w() <= along - ue + EPS {
            per += 1;
            x += rack.w();
            if per % row_max == 0 {
                x += cross;
            }
        }
        let (mut rows, mut v) = (0, end);
        while v + rack.d() * 2.0 + cold <= across - end + EPS {
            rows += 2;
            v += rack.d() * 2.0 + cold + hot;
        }
        (rows * per, per)
    };
    [one(w, d), one(d, w)]
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value as J};

    use super::super::super::{build_project, parse_spec};
    use super::super::{design, rooms_by_role};
    use super::*;

    fn hall_of(area: f64, racks: usize) -> (GDc, [f64; 4]) {
        let v = json!({
            "typology": "office", "floors": 1, "floor_height": 5.0, "band_depth": 24.0,
            "footprint": {"width": 72.0},
            "rooms": [
                {"name": "Data hall", "kind": "technical", "area": area, "count": 1},
                {"name": "UPS room", "kind": "technical", "area": 150.0, "count": 2},
                {"name": "Battery room", "kind": "technical", "area": 100.0, "count": 2},
                {"name": "Chiller plant", "kind": "technical", "area": 120.0}
            ],
            "rules": {"dc.racks": racks}
        });
        let s = parse_spec(&v).unwrap_or_else(|e| panic!("{e}"));
        let p = build_project(&s).unwrap_or_else(|e| panic!("{e}"));
        let c = rooms_by_role(&s, &p)["hall"][0].room.clear;
        (design(&s, &p), c)
    }

    /// depth.band stays at daylight depth (12 m); only a brief with a data hall goes deeper.
    #[test]
    fn only_a_data_hall_brief_takes_a_band_past_daylight_depth() {
        let brief = |band: f64, room: J| {
            json!({ "typology": "office", "floors": 1, "band_depth": band,
                    "footprint": {"width": 72.0}, "rooms": [room] })
        };
        let office = json!({"name": "Open office", "kind": "office", "area": 300.0});
        let refused = |v: &J| parse_spec(v).err().is_some_and(|e| e.0.iter().any(|i| i.path == "band_depth"));
        assert!(refused(&brief(12.5, office.clone())), "an office band past 12 m is refused");
        assert!(refused(&brief(24.0, office.clone())));
        assert!(parse_spec(&brief(12.0, office)).is_ok());
        let named = json!({"name": "Data hall", "kind": "technical", "area": 600.0});
        let tagged = json!({"name": "Hall A", "kind": "technical", "area": 600.0, "tags": ["data_hall"]});
        for room in [named.clone(), tagged] {
            let s = parse_spec(&brief(24.0, room)).unwrap_or_else(|e| panic!("{e}"));
            assert_eq!(s.band_depth, 24.0);
        }
        assert!(refused(&brief(31.0, named)), "dc.hall_band caps the hall band at 30 m");
    }

    #[test]
    fn a_hall_gets_the_rack_count_of_the_rules_within_a_row() {
        for area in [300.0, 500.0, 720.0] {
            let (d, c) = hall_of(area, 0);
            let got = d.count(Kind::Rack);
            let caps = capacity(c[2] - c[0], c[3] - c[1]);
            // columns and the cooling units take rows' ends: never more than the rules' capacity
            // (up to one row over), at most one row pair under it
            assert!(
                caps.iter().any(|k| got <= k.0 + k.1 && got + 2 * k.1 >= k.0),
                "{area} m2: {got} racks, rules say {caps:?}"
            );
        }
    }

    #[test]
    fn the_cooling_units_cover_the_it_load() {
        for area in [300.0, 720.0] {
            let (d, _) = hall_of(area, 0);
            let heat = d.count(Kind::Rack) as f64 * rule("dc.rack_kw") * (1.0 + rule("dc.cooling_margin"));
            let all = types(Kind::Crah);
            let cap: f64 = d
                .units
                .iter()
                .filter(|u| u.kind == Kind::Crah)
                .map(|u| all.iter().find(|l| l.family == u.family && l.ty == u.ty).map_or(0.0, |l| l.cap))
                .sum();
            assert!(cap >= heat, "{area} m2: cooling {cap} kW for {heat} kW");
        }
    }

    #[test]
    fn a_hall_sized_from_a_rack_count_seats_it() {
        let racks = 200;
        let (d, _) = hall_of(super::super::hall_area(racks), racks);
        assert_eq!(d.count(Kind::Rack), racks);
    }

    #[test]
    fn doors_close_the_cold_aisles_and_pdus_stand_at_the_row_ends() {
        let (d, _) = hall_of(720.0, 0);
        let (racks, rows) = (d.count(Kind::Rack), d.halls[0].rows);
        assert!(racks > 0 && rows >= 2);
        let (doors, pdus) = (d.count(Kind::Door), d.count(Kind::Pdu));
        assert!(doors >= 2 && doors <= rows, "{doors} doors for {rows} rows");
        assert!(pdus > 0 && pdus <= rows * 2, "{pdus} PDUs for {rows} rows");
        assert!(d.units.iter().filter(|u| u.kind == Kind::Pdu).all(|u| u.path == 1 || u.path == 2));
    }
}
