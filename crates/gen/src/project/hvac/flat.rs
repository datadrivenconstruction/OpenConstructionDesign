// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! A heat-recovery unit in each flat instead of the central system: the unit hangs under the
//! slab of the flat's storage room or hall, a manifold over each port feeds one duct to each
//! terminal of the flat, and its outdoor and exhaust ducts run to the flat's own facade.

use super::*;

type P = (f64, f64);

/// A run in plan at one duct level: the segments and their half widths.
struct Level {
    segs: Vec<(P, P, f64)>,
    /// Vertical ducts that pass through the level: point and half width.
    posts: Vec<(P, f64)>,
}

impl Level {
    fn free(&self, a: P, b: P, r: f64, clear: f64, skip: P) -> bool {
        let near = |q: P| (q.0 - skip.0).abs() < 1e-6 && (q.1 - skip.1).abs() < 1e-6;
        self.segs.iter().all(|&(p, q, s)| near(p) || seg_seg(a, b, p, q) >= r + s + clear)
            && self.posts.iter().all(|&(p, s)| near(p) || seg_dist(p, a, b) >= r + s + clear)
    }
}

fn seg_seg(a: P, b: P, c: P, d: P) -> f64 {
    let cross = |o: P, p: P, q: P| (p.0 - o.0) * (q.1 - o.1) - (p.1 - o.1) * (q.0 - o.0);
    let (d1, d2, d3, d4) = (cross(c, d, a), cross(c, d, b), cross(a, b, c), cross(a, b, d));
    if d1 * d2 < 0.0 && d3 * d4 < 0.0 {
        return 0.0;
    }
    seg_dist(a, c, d).min(seg_dist(b, c, d)).min(seg_dist(c, a, b)).min(seg_dist(d, a, b))
}

/// Plan routes from `a` to `b`: straight, then the two single-elbow ones.
fn routes(a: P, b: P) -> [Vec<P>; 3] {
    [vec![a, b], vec![a, (b.0, a.1), b], vec![a, (a.0, b.1), b]]
}

fn in_flat(q: P, rings: &[&[P]]) -> bool {
    rings.iter().any(|r| crate::check::inside(r, q))
}

/// The first route from `a` to `b` that keeps inside the flat and clear of the level's runs;
/// `None` when none does.
fn route(lv: &Level, a: P, b: P, r: f64, clear: f64, rings: &[&[P]]) -> Option<Vec<P>> {
    routes(a, b).into_iter().find(|pts| {
        let inside = pts.windows(2).all(|w| (0..=10).all(|i| {
            let t = i as f64 / 10.0;
            in_flat((w[0].0 + (w[1].0 - w[0].0) * t, w[0].1 + (w[1].1 - w[0].1) * t), rings)
        }));
        let short = pts.windows(2).all(|w| ((w[1].0 - w[0].0).hypot(w[1].1 - w[0].1)) > 1e-6);
        inside && short && pts.windows(2).all(|w| lv.free(w[0], w[1], r, clear, a))
    })
}

/// Rooms of one flat on one storey.
struct Flat {
    storey: usize,
    rooms: Vec<usize>,
}

fn flats(p: &GProject) -> Vec<Flat> {
    let mut out: Vec<Flat> = vec![];
    for (k, st) in p.storeys.iter().enumerate() {
        let mut ids: Vec<u32> = st.layout.rooms.iter().filter_map(|r| r.apartment).collect();
        ids.sort_unstable();
        ids.dedup();
        for a in ids {
            let rooms = (0..st.layout.rooms.len()).filter(|&i| st.layout.rooms[i].apartment == Some(a)).collect();
            out.push(Flat { storey: k, rooms });
        }
    }
    out
}

/// Designs a unit per flat into `p`; `false`, with `p` untouched, when the project has no
/// flats or more than the plant slots hold.
pub(super) fn per_flat(spec: &ProjectSpec, p: &mut GProject) -> Result<bool, GenError> {
    let all = flats(p);
    if all.is_empty() || all.len() > super::super::slots::PLANT_MAX as usize {
        return Ok(false);
    }
    let dn = Dn::read(true);
    let air = super::super::mech::AirPlan::new(spec, p);
    let clear = rule("services.hvac.branch_clear");
    let gap = rule("services.hvac.flat_port_gap");
    let roof = p.roof.elevation;
    let mut hv = GHvac::default();
    let mut fixtures: Vec<Vec<GFixture>> = vec![vec![]; p.storeys.len()];
    let (mut placed, mut terminals, mut lost, mut crossings, mut metres) = ([0.0; 2], 0usize, 0usize, 0usize, 0.0);
    let n = p.storeys.len();
    let mut net = 0u8;
    let mut facade: Vec<usize> = vec![];
    for fl in &all {
        let st = &p.storeys[fl.storey];
        let room_clear = st.height - svc::slab();
        let rings: Vec<&[P]> = fl.rooms.iter().map(|&i| st.layout.rooms[i].polygon.as_slice()).collect();
        // the air of each room of the flat, supply and extract raised to the larger
        let mut q: Vec<(usize, [f64; 2])> = vec![];
        for &i in &fl.rooms {
            let (pr, gr) = (&st.rooms[i], &st.layout.rooms[i]);
            let r = bbox(&gr.polygon);
            let area = (r.2 - r.0) * (r.3 - r.1);
            let tags = spec.room_tags.of(pr.kind, &gr.name);
            let (qs, qe) = air.room(fl.storey, i, demand(pr.kind, area, room_clear, true, tags));
            if qs + qe > 0.0 {
                q.push((i, [qs, qe]));
            }
        }
        let tot = [0, 1].map(|s| q.iter().map(|r| r.1[s]).sum::<f64>());
        if tot[0] <= 0.0 && tot[1] <= 0.0 {
            continue;
        }
        let small = usize::from(tot[0] >= tot[1]);
        if tot[small] > 0.0 {
            let k = tot[1 - small] / tot[small];
            q.iter_mut().for_each(|r| r.1[small] = up5(r.1[small] * k));
        }
        let tot = [0, 1].map(|s| q.iter().map(|r| r.1[s]).sum::<f64>());
        // the unit's room: storage, else the hall, else the smallest room of the flat
        let area = |i: usize| {
            let r = bbox(&st.layout.rooms[i].polygon);
            (r.2 - r.0) * (r.3 - r.1)
        };
        let pick = |k: RoomKind| fl.rooms.iter().copied().filter(|&i| st.rooms[i].kind == k).max_by(|a, b| area(*a).total_cmp(&area(*b)));
        let Some(home) = pick(RoomKind::Storage).or_else(|| pick(RoomKind::Hall)).or_else(|| fl.rooms.iter().copied().min_by(|a, b| area(*a).total_cmp(&area(*b)))) else {
            continue;
        };
        let hr = bbox(&st.layout.rooms[home].polygon);
        let unit = pick_unit(tot[0].max(tot[1]));
        let rot = if hr.2 - hr.0 >= hr.3 - hr.1 { 0.0 } else { PI / 2.0 };
        let mut at = ((hr.0 + hr.2) / 2.0, (hr.1 + hr.3) / 2.0);
        if !crate::check::inside(&st.layout.rooms[home].polygon, at) {
            at = st.layout.rooms[home].point;
        }
        // duct levels under the slab: supply, extract, then the unit below them
        let d = [0, 1].map(|s| q.iter().filter(|r| r.1[s] > 0.0).map(|r| round(&dn, r.1[s], dn.v_branch).max(term_for(s, r.1[s]).neck_d)).fold(0.0, f64::max));
        let d_unit = [0, 1].map(|s| round(&dn, tot[s], dn.v_branch));
        let band = [0, 1].map(|s| d[s].max(d_unit[s]));
        let top = st.elevation + st.height - svc::slab() - dn.top_gap;
        let z = [top - band[0] / 2.0, top - band[0] - dn.layer_gap - band[1] / 2.0];
        let base = z[1] - band[1] / 2.0 - dn.layer_gap - dn.bend * band[0].max(band[1]) - unit.h;
        let face = base - st.elevation - dn.drop_gap - dn.drop_bend * band[0].max(band[1]);
        let face = face.min(z[1] - st.elevation - band[1] / 2.0 - dn.drop_bend * d[0].max(d[1]) - dn.drop_gap - 0.2);
        let plant_ix = hv.plant.len();
        hv.plant.push(GFixture { scale: 1.0, min_clear: 0.0, family: UNIT_FAMILY, ty: unit.ty, at, rotation: rot, wall: None, z: base - roof });
        let ports: Vec<(f64, f64, f64)> = air_ports(hv.plant.last().expect("a unit")).into_iter().map(|(_, q)| (q.0, q.1, q.2 + roof)).collect();
        let mut lv = [Level { segs: vec![], posts: vec![] }, Level { segs: vec![], posts: vec![] }];
        // the supply climb passes the extract level
        lv[1].posts.push(((ports[0].0, ports[0].1), band[0] / 2.0));
        for s in 0..2 {
            if tot[s] <= 0.0 {
                continue;
            }
            let port = ports[s];
            let hub = (port.0, port.1, z[s]);
            hv.ducts.push(GDuct { net, sys: SYS[s], storey: fl.storey, points: vec![port, hub], size: (d_unit[s], 0.0), flow: tot[s], bend: None });
            hv.links.push(GAirLink { storey: n, fixture: plant_ix, connector: if s == 0 { "Supply" } else { "Return" }, sys: SYS[s], net });
            placed[s] += tot[s];
            for &(i, rq) in &q {
                if rq[s] <= 0.0 {
                    continue;
                }
                let gr = &st.layout.rooms[i];
                let r = bbox(&gr.polygon);
                let t = term_for(s, rq[s]);
                let dd = round(&dn, rq[s], dn.v_branch).max(t.neck_d);
                // the terminal off the room's middle on the side away from the other system's
                let mut c = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
                if !crate::check::inside(&gr.polygon, c) {
                    c = gr.point;
                }
                let off = if rq[1 - s] > 0.0 { 0.25 * ((r.2 - r.0).max(r.3 - r.1) - 2.0 * dn.wall_keep).max(0.0) } else { 0.0 };
                let c = if r.2 - r.0 >= r.3 - r.1 { (c.0 + if s == 0 { -off } else { off }, c.1) } else { (c.0, c.1 + if s == 0 { -off } else { off }) };
                let Some(plan) = route(&lv[s], (hub.0, hub.1), c, dd / 2.0, clear, &rings).or_else(|| {
                    crossings += 1;
                    Some(routes((hub.0, hub.1), c)[0].clone())
                }) else {
                    continue;
                };
                for w in plan.windows(2) {
                    lv[s].segs.push((w[0], w[1], dd / 2.0));
                }
                if s == 0 {
                    lv[1].posts.push((c, dd / 2.0));
                }
                let neck = st.elevation + face + t.neck_h;
                let mut pts: Vec<(f64, f64, f64)> = plan.iter().map(|q| (q.0, q.1, z[s])).collect();
                pts.push((c.0, c.1, neck));
                metres += len(&pts);
                hv.ducts.push(GDuct { net, sys: SYS[s], storey: fl.storey, points: pts, size: (dd, 0.0), flow: rq[s], bend: Some(dn.drop_bend * dd) });
                let idx = st.layout.fixtures.len() + fixtures[fl.storey].len();
                fixtures[fl.storey].push(GFixture { scale: 1.0, min_clear: 0.0, family: t.fam, ty: t.ty, at: c, rotation: 0.0, wall: None, z: face });
                hv.links.push(GAirLink { storey: fl.storey, fixture: idx, connector: "Air", sys: SYS[s], net });
                terminals += 1;
            }
        }
        // outdoor air in and exhaust out at the flat's nearest facade, `gap` apart along it
        let mut wall: Option<(f64, P, P)> = None;
        for w in st.layout.walls.iter().filter(|w| w.role == crate::WallRole::Exterior && w.bulge == 0.0) {
            let (a, b) = (w.a, w.b);
            let l = (b.0 - a.0).hypot(b.1 - a.1);
            if l < gap + 1.0 {
                continue;
            }
            let u = ((b.0 - a.0) / l, (b.1 - a.1) / l);
            let t = ((at.0 - a.0) * u.0 + (at.1 - a.1) * u.1).clamp(gap / 2.0 + 0.5, l - gap / 2.0 - 0.5);
            let f = (a.0 + u.0 * t, a.1 + u.1 * t);
            let nrm = { let v = (at.0 - f.0, at.1 - f.1); let m = v.0.hypot(v.1).max(1e-9); (v.0 / m, v.1 / m) };
            let ok = [-1.0, 1.0].iter().all(|sg| in_flat((f.0 + u.0 * sg * gap / 2.0 + nrm.0 * 0.4, f.1 + u.1 * sg * gap / 2.0 + nrm.1 * 0.4), &rings));
            let dist = (at.0 - f.0).hypot(at.1 - f.1);
            if ok && wall.as_ref().is_none_or(|b| dist < b.0) {
                wall = Some((dist, f, u));
            }
        }
        if let Some((_, f, u)) = wall {
            let q_oa = tot[0].max(tot[1]);
            let dd = round(&dn, q_oa, dn.v_branch);
            let (c, sn) = (rot.cos(), rot.sin());
            for s in 0..2 {
                // the outdoor duct leaves the unit's top on one side of its axis, the exhaust
                // on the other, each at its system's level
                let sg = if s == 0 { 1.0 } else { -1.0 };
                let o = (at.0 - sn * sg * unit.w / 4.0, at.1 + c * sg * unit.w / 4.0);
                let e = (f.0 + u.0 * sg * gap / 2.0, f.1 + u.1 * sg * gap / 2.0);
                let plan = route(&lv[s], o, e, dd / 2.0, clear, &rings).unwrap_or_else(|| {
                    crossings += 1;
                    vec![o, e]
                });
                for w in plan.windows(2) {
                    lv[s].segs.push((w[0], w[1], dd / 2.0));
                }
                let mut pts = vec![(o.0, o.1, base + unit.h)];
                pts.extend(plan.iter().map(|q| (q.0, q.1, z[s])));
                metres += len(&pts);
                facade.push(hv.ducts.len());
                hv.ducts.push(GDuct { net: 0, sys: SYS[s], storey: fl.storey, points: pts, size: (dd, 0.0), flow: q_oa, bend: None });
            }
        } else {
            lost += 1;
        }
        net += 1;
    }
    if hv.plant.is_empty() {
        return Ok(false);
    }
    // the outdoor and exhaust ducts form the last system, after one per flat
    for &i in &facade {
        hv.ducts[i].net = net;
    }
    for (k, f) in fixtures.into_iter().enumerate() {
        p.storeys[k].layout.fixtures.extend(f);
        if p.storeys[k].layout.fixtures.len() > super::super::slots::FIXTURES_MAX as usize {
            return Err(GenError::Infeasible(format!("too many fixtures and air terminals on floor {}", k + 1)));
        }
    }
    let no = |i: u8| if spec.ru { (format!("\u{41f}{i}"), format!("\u{412}{i}")) } else { (format!("S{i}"), format!("E{i}")) };
    hv.names = no(1);
    hv.nets = (2..=net).map(no).collect();
    hv.nets.push(if spec.ru { ("\u{41d}\u{412}".into(), "\u{412}\u{412}".into()) } else { ("OA".into(), "EA".into()) });
    hv.flow = (placed[0], placed[1]);
    p.hvac = hv;
    p.metrics.extend([
        ("hvac_supply_m3h", placed[0]),
        ("hvac_extract_m3h", placed[1]),
        ("hvac_terminals", terminals as f64),
        ("hvac_unserved", 0.0),
        ("hvac_duct_m", (metres * 10.0).round() / 10.0),
        ("hvac_units", p.hvac.plant.len() as f64),
        ("hvac_flat_no_facade", lost as f64),
        ("hvac_flat_crossings", crossings as f64),
    ]);
    Ok(true)
}
