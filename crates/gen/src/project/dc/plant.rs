// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The plant of the data centre: UPS and batteries per power path in their rooms, chillers in
//! the cooling plant room or the yard, generators in the yard; the chillers' headers, a supply
//! and a return main from each end of them to each hall (paths A and B), and a cable tray
//! feeder from each hall trunk to the UPS of its path. Mains and feeders are routed clear of
//! the columns and of each other.

use std::collections::HashMap;

use ddc_geom::math::{v2, v3, V3};
use ddc_geom::mep::route3d::{self, Index, Obstacle, Pass, Routed, RoutedIndex, Rules, Source, Terminal};

use super::hall::{branch_dn, LaidHall, Tap};
use super::why::{n2, s, Whys};
use super::{columns, count_rule, dn_for, flow_of, footprint, pipe, port_world, svc_box, tray, types, DcLink, DcRoom, DcRun, DcUnit, GDc, Kind, Lib, Net, EPS};
use super::super::rules::rule;
use super::super::svc::{self, Side};
use super::super::GProject;

/// Turn of a family whose front (local -y) faces the plan direction `f`.
fn facing(f: (f64, f64)) -> f64 {
    f.0.atan2(-f.1)
}

/// Types by installed capacity for a demand, then by fewer units: (type, units).
fn by_capacity(ts: &[Lib], demand: f64, spare: usize) -> Vec<(&Lib, usize)> {
    let mut v: Vec<(&Lib, usize)> = ts.iter().filter(|l| l.cap > 0.0).map(|l| (l, (demand / l.cap).ceil() as usize + spare)).collect();
    v.sort_by(|a, b| (a.1 as f64 * a.0.cap).total_cmp(&(b.1 as f64 * b.0.cap)).then(a.1.cmp(&b.1)));
    v
}

/// Units packed with their backs to the walls of a room, longest wall first, each keeping
/// `dc.unit_front` free before it; what is taken stays in `busy`.
struct Packer<'a> {
    p: &'a GProject,
    busy: HashMap<(usize, usize), Vec<svc_box::R>>,
}

impl<'a> Packer<'a> {
    /// Up to `n` units on the sides of the room (`only_one`: on its longest side alone);
    /// the plan points and turns.
    fn pack(&mut self, room: &DcRoom, l: &Lib, n: usize, only_one: bool) -> Vec<((f64, f64), f64)> {
        let (gap, front) = (rule("dc.unit_gap"), rule("dc.unit_front"));
        let cols = columns(self.p, room.storey);
        let c = room.room.clear;
        let busy = self.busy.entry((room.storey, room.room.idx)).or_default();
        let mut sides: Vec<&Side> = room.room.sides.iter().collect();
        sides.sort_by(|a, b| b.len.total_cmp(&a.len));
        if only_one {
            sides.truncate(1);
        }
        let mut out = vec![];
        let step = l.w() * rule("dc.unit_step");
        for side in sides {
            let mut x = gap + l.w() / 2.0;
            while out.len() < n && x + l.w() / 2.0 + gap <= side.len + EPS {
                let at = side.at(x, l.hi.y);
                let rot = side.back_rot();
                let body = footprint(l, at, rot, gap);
                let zone = {
                    let reach = Lib { lo: v3(l.lo.x, l.lo.y - front, l.lo.z), ..l.clone() };
                    footprint(&reach, at, rot, 0.0)
                };
                let inside = zone[0] >= c[0] - EPS && zone[1] >= c[1] - EPS && zone[2] <= c[2] + EPS && zone[3] <= c[3] + EPS;
                let ok = inside
                    && side.free(x - l.w() / 2.0, x + l.w() / 2.0, gap, 0.0, l.h())
                    && cols.iter().all(|k| !svc_box::hit(&body, k))
                    && busy.iter().all(|k| !svc_box::hit(&zone, k) && !svc_box::hit(&body, k));
                if ok {
                    busy.push(zone);
                    out.push((at, rot));
                    x += l.w() + gap;
                } else {
                    x += step;
                }
            }
        }
        if only_one && out.len() < n {
            busy.truncate(busy.len() - out.len());
            out.clear();
        }
        out
    }
}

/// The yard beside the ground storey on `dc.yard_side`: start corner, direction along the
/// facade, direction away from it, facade length.
fn yard(p: &GProject) -> ((f64, f64), (f64, f64), (f64, f64), f64) {
    let b = svc::bbox(&p.storeys[0].layout.slab);
    match count_rule("dc.yard_side") {
        1 => ((b[0], b[3]), (1.0, 0.0), (0.0, 1.0), b[2] - b[0]),
        2 => ((b[2], b[1]), (0.0, 1.0), (1.0, 0.0), b[3] - b[1]),
        3 => ((b[0], b[1]), (0.0, 1.0), (-1.0, 0.0), b[3] - b[1]),
        _ => ((b[0], b[1]), (1.0, 0.0), (0.0, -1.0), b[2] - b[0]),
    }
}

/// Rows of units in the yard from distance `d0` off the facade: their plan points, the turn
/// that faces the building, and the distance the next rows start at; `None` when `one_row`
/// and they do not fit in one.
fn yard_rows(p: &GProject, l: &Lib, n: usize, d0: f64, one_row: bool) -> Option<(Vec<((f64, f64), f64)>, f64)> {
    let (o, t, nn, len) = yard(p);
    let front = rule("dc.unit_front");
    let per = (((len + front) / (l.w() + front)).floor() as usize).max(1);
    if one_row && n > per {
        return None;
    }
    let rot = facing((-nn.0, -nn.1));
    let mut out = vec![];
    let mut d = d0;
    for i in 0..n {
        if i > 0 && i % per == 0 {
            d += l.d() + front;
        }
        let x = (i % per) as f64 * (l.w() + front) + l.w() / 2.0;
        let dd = d + l.d() / 2.0;
        out.push(((o.0 + t.0 * x + nn.0 * dd, o.1 + t.1 * x + nn.1 * dd), rot));
    }
    Some((out, d + l.d() + front))
}

pub(crate) fn plant(p: &GProject, rooms: &HashMap<&'static str, Vec<DcRoom>>, laid: &[LaidHall], dc: &mut GDc, w: &mut Whys) {
    let it: f64 = laid.iter().map(|h| h.it_kw).sum();
    let heat: f64 = laid.iter().map(|h| h.heat_kw).sum();
    if it <= 0.0 {
        return;
    }
    let mut pk = Packer { p, busy: HashMap::new() };
    let none = vec![];
    let ups_rooms = rooms.get("ups").unwrap_or(&none);
    let bat_rooms = rooms.get("battery").unwrap_or(&none);
    let paths = count_rule("dc.ups_paths").clamp(1, 2);
    let mut ups_at: [Option<usize>; 2] = [None, None];
    // UPS and batteries per path
    for path in 0..paths {
        let tag = path_code(path);
        let Some(room) = ups_rooms.get(path % ups_rooms.len().max(1)) else {
            w.add("dc", "dc.ups_paths", "dc_no_room_ups", vec![("path", tag.into())]);
            dc_red(w, path, 1, 0);
            break;
        };
        let ups = types(Kind::Ups);
        let spare = count_rule("dc.ups_spare");
        let cands = by_capacity(&ups, it, spare);
        let Some(&(l, need)) = cands.first() else { break };
        let at = pk.pack(room, l, need, false);
        if at.len() < need {
            w.add("dc", "dc.unit_front", "dc_unplaced_ups", vec![("path", tag.into()), ("n", s(at.len())), ("need", s(need)), ("room", room.name.clone())]);
        }
        w.add(
            "dc",
            "dc.ups_paths",
            "dc_ups",
            vec![("path", tag.into()), ("n", s(at.len())), ("kw", n2(l.cap)), ("load", n2(it)), ("spare", s(spare))],
        );
        dc_red(w, path, need.saturating_sub(spare), at.len());
        for (xy, rot) in at {
            if ups_at[path].is_none() {
                ups_at[path] = Some(dc.units.len());
            }
            dc.units.push(DcUnit { kind: Kind::Ups, family: l.family.clone(), ty: l.ty.clone(), storey: room.storey, at: xy, z: 0.0, rot, path: path as u8 + 1 });
        }
        let bats = types(Kind::Battery);
        let kwh = it * rule("dc.battery_min") / 60.0;
        let Some(&(b, nb)) = by_capacity(&bats, kwh, 0).first() else { continue };
        let broom = bat_rooms.get(path % bat_rooms.len().max(1)).unwrap_or(room);
        let at = pk.pack(broom, b, nb, false);
        if at.len() < nb {
            w.add("dc", "dc.unit_front", "dc_unplaced_battery", vec![("path", tag.into()), ("n", s(at.len())), ("need", s(nb)), ("room", broom.name.clone())]);
        }
        w.add(
            "dc",
            "dc.battery_min",
            "dc_battery",
            vec![("path", tag.into()), ("n", s(at.len())), ("kwh", n2(at.len() as f64 * b.cap)), ("min", n2(rule("dc.battery_min")))],
        );
        for (xy, rot) in at {
            dc.units.push(DcUnit { kind: Kind::Battery, family: b.family.clone(), ty: b.ty.clone(), storey: broom.storey, at: xy, z: 0.0, rot, path: path as u8 + 1 });
        }
    }
    // chillers: one row along the plant room's longest wall, else in the yard
    let chill = types(Kind::Chiller);
    let spare = count_rule("dc.chiller_spare");
    let cands = by_capacity(&chill, heat, spare);
    // the place is the plant room's name, None for the yard
    let mut row: Option<(&Lib, usize, Vec<((f64, f64), f64)>, Option<String>)> = None;
    if let Some(room) = rooms.get("cooling").and_then(|r| r.first()) {
        for &(l, n) in &cands {
            let at = pk.pack(room, l, n, true);
            if at.len() == n {
                row = Some((l, room.storey, at, Some(room.name.clone())));
                break;
            }
        }
    }
    let gap0 = rule("dc.yard_gap");
    let mut next = gap0;
    if row.is_none() {
        for &(l, n) in &cands {
            if let Some((at, d)) = yard_rows(p, l, n, gap0, true) {
                row = Some((l, 0, at, None));
                next = d;
                break;
            }
        }
    }
    let mut chillers = vec![];
    if let Some((l, storey, at, place)) = &row {
        let mut args = vec![("n", s(at.len())), ("kw", n2(l.cap)), ("heat", n2(heat)), ("spare", s(spare))];
        let key = match place {
            Some(room) => {
                args.push(("room", room.clone()));
                "dc_chillers_room"
            }
            None => "dc_chillers_yard",
        };
        w.add("dc", "dc.chiller_spare", key, args);
        w.red.push(("chiller", at.len().saturating_sub(spare), at.len()));
        for &(xy, rot) in at {
            chillers.push(dc.units.len());
            dc.units.push(DcUnit { kind: Kind::Chiller, family: l.family.clone(), ty: l.ty.clone(), storey: *storey, at: xy, z: 0.0, rot, path: 0 });
        }
    } else {
        w.add("dc", "dc.unit_front", "dc_no_room_chillers", vec![("n", s(cands.first().map_or(0, |c| c.1)))]);
        w.red.push(("chiller", cands.first().map_or(1, |c| c.1.saturating_sub(spare)), 0));
    }
    // generators in the yard, beyond the chillers there
    let gens = types(Kind::Genset);
    let gspare = count_rule("dc.gen_spare");
    let load = it * rule("dc.facility_factor");
    if let Some(&(g, ng)) = by_capacity(&gens, load, gspare).first() {
        if let Some((at, _)) = yard_rows(p, g, ng, next, false) {
            w.add("dc", "dc.gen_spare", "dc_gensets", vec![("n", s(at.len())), ("kw", n2(g.cap)), ("load", n2(load)), ("spare", s(gspare))]);
            w.red.push(("genset", ng.saturating_sub(gspare), at.len()));
            for (xy, rot) in at {
                dc.units.push(DcUnit { kind: Kind::Genset, family: g.family.clone(), ty: g.ty.clone(), storey: 0, at: xy, z: 0.0, rot, path: 0 });
            }
        }
    }
    let mut net = Router::new(p, dc);
    if let Some((l, storey, _, _)) = &row {
        chw_plant(p, l, *storey, &chillers, laid, heat, dc, w, &mut net);
    } else {
        for h in laid {
            for tap in h.chw.iter().flatten().flatten() {
                cap(dc, tap);
            }
        }
    }
    feeders(p, laid, &ups_at, dc, w, &mut net);
}

fn cap(dc: &mut GDc, t: &Tap) {
    if t.start {
        dc.runs[t.run].caps.0 = true;
    } else {
        dc.runs[t.run].caps.1 = true;
    }
}

/// Routing of mains and feeders: the columns and the units below the trays are obstacles;
/// each routed line is kept clear of by the next.
struct Router {
    ix: Index,
    bounds: ((f64, f64), (f64, f64)),
}

impl Router {
    fn new(p: &GProject, dc: &GDc) -> Router {
        let mut obs = vec![];
        let mut lo = (f64::INFINITY, f64::INFINITY);
        let mut hi = (f64::NEG_INFINITY, f64::NEG_INFINITY);
        let mut grow = |x: f64, y: f64| {
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        };
        for (k, st) in p.storeys.iter().enumerate() {
            for q in &st.layout.slab {
                grow(q.0, q.1);
            }
            for c in columns(p, k) {
                obs.push(Obstacle::aabb(v3(c[0], c[1], st.elevation), v3(c[2], c[3], st.elevation + st.height), Pass::Block, 0));
            }
        }
        let libs: HashMap<(String, String), Lib> = Kind::ALL
            .iter()
            .flat_map(|k| types(*k))
            .map(|l| ((l.family.clone(), l.ty.clone()), l))
            .collect();
        for u in &dc.units {
            let Some(l) = libs.get(&(u.family.clone(), u.ty.clone())) else { continue };
            let fp = footprint(l, u.at, u.rot, 0.0);
            grow(fp[0], fp[1]);
            grow(fp[2], fp[3]);
            if matches!(u.kind, Kind::Crah | Kind::Chiller) {
                continue;
            }
            let e = p.storeys[u.storey].elevation + u.z;
            obs.push(Obstacle::aabb(v3(fp[0], fp[1], e), v3(fp[2], fp[3], e + l.h()), Pass::Block, 0));
        }
        let m = rule("dc.yard_gap") + rule("dc.pipe_pitch");
        Router { ix: Index::new(obs), bounds: ((lo.0 - m, lo.1 - m), (hi.0 + m, hi.1 + m)) }
    }

    /// A line from `a` (leaving up) to `b` at the high levels of storey `k`, clear by `r` of the
    /// columns, the units and every run laid so far but those its own ends sit on.
    #[allow(clippy::too_many_arguments)]
    fn route(&mut self, p: &GProject, runs: &[DcRun], k: usize, a: V3, b: V3, b_up: bool, r: f64) -> Option<Vec<V3>> {
        let mut laid = RoutedIndex::default();
        for run in runs {
            let c = run.half();
            for w in run.points.windows(2) {
                let (s, e) = (v3(w[0].0, w[0].1, w[0].2), v3(w[1].0, w[1].1, w[1].2));
                let at_end = [a, b].iter().any(|&q| route3d::seg_seg_dist(q, q, s, e) < r + c);
                if !at_end {
                    laid.push(Routed { a: s, b: e, clearance: c });
                }
            }
        }
        let st = &p.storeys[k];
        let pitch = rule("dc.pipe_pitch");
        let types = types(Kind::Rack);
        let rack_h = types.iter().map(|l| l.h()).fold(0.0, f64::max);
        let lo = st.elevation + rule("dc.raised_floor") + rack_h + rule("dc.tray_gap") + rule("dc.tray_h") + pitch;
        let top = st.elevation + svc::ceiling(p, k) - pitch;
        let mut layers = vec![];
        let mut z = top;
        while z >= lo - EPS {
            layers.push(z);
            z -= pitch;
        }
        if layers.is_empty() {
            layers.push(lo.max(top));
        }
        let bounds = (v2(self.bounds.0 .0, self.bounds.0 .1), v2(self.bounds.1 .0, self.bounds.1 .1));
        let rules = Rules::new(r, pitch, layers, bounds);
        let up = Some(v3(0.0, 0.0, 1.0));
        let src = Source::Terminal(Terminal { at: a, dir: up });
        let tgt = [Terminal { at: b, dir: if b_up { up } else { None } }];
        let tree = route3d::route_tree(&self.ix, &rules, &src, &tgt, &mut laid);
        if !tree.failed.is_empty() {
            return None;
        }
        let br = tree.branches.first()?;
        let mut pts: Vec<V3> = br.points.iter().rev().copied().collect();
        if pts.first().map_or(true, |q| (*q - a).len() > EPS) {
            pts.insert(0, a);
        }
        if pts.last().map_or(true, |q| (*q - b).len() > EPS) {
            pts.push(b);
        }
        Some(route3d::clean(&pts))
    }
}

/// The code of a power path: A or B.
fn path_code(path: usize) -> &'static str {
    if path == 0 {
        "A"
    } else {
        "B"
    }
}

/// Records a UPS path's duty and built units.
fn dc_red(w: &mut Whys, path: usize, duty: usize, built: usize) {
    w.red.push((if path == 0 { "ups_a" } else { "ups_b" }, duty, built));
}

/// The chillers' supply and return headers with a tap at each end per hall, branches to the
/// chillers, and the mains: path A from the start of the headers to each hall's wall A, path
/// B from their end to wall B.
#[allow(clippy::too_many_arguments)]
fn chw_plant(p: &GProject, l: &Lib, storey: usize, chillers: &[usize], laid: &[LaidHall], heat: f64, dc: &mut GDc, w: &mut Whys, net: &mut Router) {
    let ports = super::data()["ports"].clone();
    let names = [ports["supply"].as_str().unwrap_or(""), ports["return"].as_str().unwrap_or("")];
    let elev = p.storeys[storey].elevation;
    // per hall: supply and return of both paths are the duty, the routed ones are built
    let mut mains = vec![0usize; laid.len()];
    let (pitch, off, gap) = (rule("dc.pipe_pitch"), rule("dc.pipe_off"), rule("dc.unit_gap"));
    let (hdn, hv) = dn_for(flow_of(heat));
    w.add("dc", "dc.chw_v", "dc_dn_plant", vec![("dn", s(hdn)), ("flow", n2(flow_of(heat) * 1000.0)), ("v", n2(hv))]);
    let bdn = branch_dn(flow_of(l.cap), l.port(names[0]).map_or(0.0, |x| x.3));
    for (k, (pname, nt)) in [(names[0], Net::ChwSupply), (names[1], Net::ChwReturn)].into_iter().enumerate() {
        let mut tees: Vec<(V3, V3)> = vec![];
        for &ui in chillers {
            if let Some(pw) = port_world(&dc.units[ui], l, pname, elev) {
                tees.push(pw);
                dc.links.push(DcLink { unit: ui, connector: pname.to_string(), net: nt });
            }
        }
        let Some(&(_, dir)) = tees.first() else { continue };
        let dir = v3(dir.x, dir.y, 0.0).normalized();
        let t = v3(-dir.y, dir.x, 0.0);
        let d = off + pitch * k as f64;
        let z = tees[0].0.z;
        let base = tees[0].0 + dir * d;
        let along = |q: V3| (q - base).dot(t);
        let at = |x: f64| v3(base.x + t.x * x, base.y + t.y * x, z);
        let mut xs: Vec<f64> = vec![];
        for &(pw, _) in &tees {
            let x = along(pw);
            let foot = at(x);
            dc.runs.push(pipe(nt, storey, bdn, &[pw, foot]));
            xs.push(x);
        }
        let (x0, x1) = xs.iter().fold((f64::INFINITY, f64::NEG_INFINITY), |a, &x| (a.0.min(x), a.1.max(x)));
        let spacing = pitch * 2.0 + gap;
        let mut taps = vec![];
        for (i, h) in laid.iter().enumerate() {
            for path in 0..2 {
                let Some(ht) = h.chw[path][k] else { continue };
                let x = if path == 0 { x0 - spacing * (i + 1) as f64 } else { x1 + spacing * (i + 1) as f64 };
                let (hdn_h, _) = dn_for(flow_of(h.heat_kw));
                let r = ddc_geom::mep::PipeMaterial::Steel.by_dn(hdn_h).map_or(0.0, |q| q.od) / 2.0 + gap;
                match net.route(p, &dc.runs, h.storey, at(x), ht.at, true, r) {
                    Some(pts) => {
                        dc.runs.push(pipe(nt, h.storey, hdn_h, &pts));
                        taps.push(x);
                        mains[i] += 1;
                    }
                    None => {
                        cap(dc, &ht);
                        let key = if k == 0 { "dc_route_supply" } else { "dc_route_return" };
                        w.add("dc", "dc.pipe_pitch", key, vec![("hall", h.name.clone()), ("path", path_code(path).into())]);
                    }
                }
            }
        }
        xs.extend(taps);
        xs.sort_by(f64::total_cmp);
        xs.dedup_by(|a, b| (*a - *b).abs() < EPS);
        let n = xs.len();
        for (i, pair) in xs.windows(2).enumerate() {
            let mut run = pipe(nt, storey, hdn, &[at(pair[0]), at(pair[1])]);
            run.caps = (i == 0 && (pair[0] - x0).abs() < EPS, i + 2 == n && (pair[1] - x1).abs() < EPS);
            dc.runs.push(run);
        }
    }
    for built in mains {
        w.red.push(("chw_main", 4, built));
    }
}

/// A tray from each hall trunk of a path up to the high level and over to the UPS of that
/// path, ending capped above it.
fn feeders(p: &GProject, laid: &[LaidHall], ups_at: &[Option<usize>; 2], dc: &mut GDc, w: &mut Whys, net: &mut Router) {
    let ups = types(Kind::Ups);
    let (tw, gap) = (rule("dc.tray_w"), rule("dc.unit_gap"));
    for h in laid {
        for (path, nt) in [(0usize, Net::PowerA), (1, Net::PowerB)] {
            let Some(tap) = h.feed[path] else { continue };
            // a path feeds only from its own UPS, so the two feeds of a rack never share one
            let Some(own) = ups_at[path] else {
                cap(dc, &tap);
                w.add("dc", "dc.ups_paths", "dc_no_feed", vec![("hall", h.name.clone()), ("path", path_code(path).into())]);
                continue;
            };
            let target = Some(own).and_then(|i| {
                let u = &dc.units[i];
                let l = ups.iter().find(|l| l.ty == u.ty)?;
                let e = p.storeys[u.storey].elevation;
                Some(v3(u.at.0, u.at.1, e + u.z + l.h() + rule("dc.tray_gap") + rule("dc.tray_h") / 2.0))
            });
            let routed = target.and_then(|b| net.route(p, &dc.runs, h.storey, tap.at, b, false, tw / 2.0 + gap));
            match routed {
                Some(pts) => dc.runs.push(tray(nt, h.storey, &pts, (false, true))),
                None => {
                    cap(dc, &tap);
                    w.add("dc", "dc.pipe_pitch", "dc_route_feeder", vec![("hall", h.name.clone()), ("path", path_code(path).into())]);
                }
            }
        }
    }
}
