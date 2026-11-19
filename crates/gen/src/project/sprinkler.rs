//! Fire protection of the finished building: the sprinkler system (heads by the coverage rule,
//! branches, mains, risers, zone pumps, all sized for the flow they carry), the fans that
//! pressurise the stairs of a tall building, and the car
//! of every lift shaft. Every number is a rule.

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::PipeMaterial;

use super::rules::rule;
use super::svc::{self, pipe_run};
use super::fire_check::{clear_of_columns, coverage_sq, grid_clear, hazard_of, Hazard};
use super::{
    slots, FireRun, GFire, GFireGap, GFireLink, GFireRun, GFireZone, GProject, ProjectSpec, RoomKind,
    SvcSys,
};
use crate::GFixture;

pub(super) type P2 = (f64, f64);

/// Samples across one head spacing when the coverage is measured.
pub(super) const SAMPLES: usize = 4;

const HEAD: &str = "Sprinkler Head";
const PUMP: &str = "Fire Pump";

pub(super) fn protect(spec: &ProjectSpec, p: &mut GProject) {
    p.lifts = super::build::lift_plan(spec);
    p.fire.name = if spec.ru { crate::lang::ru("Sprinklers") } else { "Sprinklers" }.to_string();
    let Some(last) = p.storeys.last() else { return };
    // norms count from the floor of the top occupied storey, not the roof
    let top = last.elevation;
    super::lift::furnish(spec, p);
    if !spec.mep_auto {
        return;
    }
    pressurise(p, top);
    if spec.sprinklers.unwrap_or(top >= rule("fire.sprinkler_height")) {
        sprinklers(p);
    }
}

fn fixture(family: &'static str, ty: &'static str, at: P2) -> GFixture {
    GFixture { scale: 1.0, min_clear: 0.0, family, ty, at, rotation: 0.0, wall: None, z: 0.0 }
}

/// A fan beside the top of each stair of a building over the rule's height.
fn pressurise(p: &mut GProject, top: f64) {
    let n = p.storeys.len();
    if n < 2 || top < rule("fire.stair_pressure_height") {
        return;
    }
    let origins: Vec<P2> = p.storeys[n - 2].layout.stairs.iter().map(|s| s.origin).collect();
    for at in origins {
        let l = &mut p.storeys[n - 1].layout.fixtures;
        if l.len() < slots::FIXTURES_MAX as usize {
            l.push(fixture("Pressurisation Fan", "Stair fan", at));
            p.fire.pressurised = true;
        }
    }
}

pub(super) fn bbox(r: &[P2]) -> [f64; 4] {
    r.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |b, q| {
        [b[0].min(q.0), b[1].min(q.1), b[2].max(q.0), b[3].max(q.1)]
    })
}

pub(super) fn inside(poly: &[P2], q: P2) -> bool {
    let mut c = false;
    for i in 0..poly.len() {
        let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
        if (a.1 > q.1) != (b.1 > q.1) && q.0 < (b.0 - a.0) * (q.1 - a.1) / (b.1 - a.1) + a.0 {
            c = !c;
        }
    }
    c
}

/// Distance from a point to the nearest edge of a ring.
pub(super) fn edge_dist(poly: &[P2], q: P2) -> f64 {
    (0..poly.len())
        .map(|i| {
            let (a, b) = (poly[i], poly[(i + 1) % poly.len()]);
            let (dx, dy) = (b.0 - a.0, b.1 - a.1);
            let l2 = dx * dx + dy * dy;
            let t = if l2 > 0.0 { (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
            (q.0 - a.0 - t * dx).hypot(q.1 - a.1 - t * dy)
        })
        .fold(f64::MAX, f64::min)
}

/// A head stands in the room, clear of its walls by the least wall distance.
pub(super) fn placeable(poly: &[P2], q: P2) -> bool {
    inside(poly, q) && edge_dist(poly, q) >= rule("fire.head_wall_min")
}

/// The head grid of a box in the ordinary hazard class.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn grid(b: &[f64; 4]) -> (Vec<f64>, Vec<f64>) {
    grid_for(b, &Hazard::class(super::fire_check::ORDINARY))
}

/// Cell centres of the head grid of a box: the fewest cells whose side stays within the
/// spacing, the area per head and twice the wall distance of the hazard class.
pub(super) fn grid_for(b: &[f64; 4], hz: &Hazard) -> (Vec<f64>, Vec<f64>) {
    let (w, h) = (b[2] - b[0], b[3] - b[1]);
    let (sp, area, wall) = (hz.spacing, hz.area, hz.wall);
    let cells = |len: f64| (len / sp).ceil().max(1.0) as usize;
    let (mut nx, mut ny) = (cells(w), cells(h));
    while w / nx as f64 / 2.0 > wall {
        nx += 1;
    }
    while h / ny as f64 / 2.0 > wall {
        ny += 1;
    }
    while w / nx as f64 * (h / ny as f64) > area + 1e-9 {
        if w / nx as f64 >= h / ny as f64 {
            nx += 1;
        } else {
            ny += 1;
        }
    }
    let at = |lo: f64, len: f64, n: usize| (0..n).map(|i| lo + len * (i as f64 + 0.5) / n as f64).collect();
    (at(b[0], w, nx), at(b[1], h, ny))
}

/// Smallest catalogue steel pipe whose bore keeps `q` l/min under the speed rule.
pub(super) fn steel(q: f64) -> u32 {
    steel_from(q, rule("fire.pipe_dn_min") as u32)
}

/// [`steel`] with its own smallest size.
pub(super) fn steel_from(q: f64, min: u32) -> u32 {
    let mut s = PipeMaterial::Steel.sizes();
    s.sort_by_key(|x| x.dn);
    let ls = q / SECONDS / 1000.0;
    let v = rule("fire.velocity_max");
    s.iter()
        .filter(|x| x.dn >= min)
        .find(|x| ls <= v * std::f64::consts::PI * x.id().powi(2) / 4.0)
        .or(s.last())
        .map_or(0, |x| x.dn)
}

/// Litres per minute in a second.
const SECONDS: f64 = 60.0;

/// The room of a storey a head grid is laid in.
struct Cell {
    room: usize,
    poly: Vec<P2>,
    xs: Vec<f64>,
    ys: Vec<f64>,
    hz: Hazard,
    /// The heads as placed: grid points in the room, moved clear of the columns.
    heads: Vec<P2>,
}

#[derive(Clone, Copy)]
struct Plan {
    z_row: f64,
    z_port: f64,
    q_head: f64,
    design: f64,
}

impl Plan {
    fn run(&self, kind: FireRun, k: usize, pts: Vec<V3>, heads: u32) -> GFireRun {
        let flow = f64::from(heads).min(self.design) * self.q_head;
        let run = pipe_run(SvcSys::Fire, k, PipeMaterial::Steel, steel(flow), pts);
        GFireRun { kind, run, flow, heads }
    }
}

fn sprinklers(p: &mut GProject) {
    let n = p.storeys.len();
    // a main or riser is sized for the heaviest class it feeds; the ordinary class until then
    let q_base = Hazard::class(super::fire_check::ORDINARY).flow();
    let design = rule("fire.design_heads");
    let mut q_of = vec![vec![0.0f64; 0]; n];
    // risers stand apart beside the lift bank, or at the plate's centre
    let plate = bbox(&p.storeys[0].layout.slab);
    let anchor = super::lift::rings(p).first().map_or(
        ((plate[0] + plate[2]) / 2.0, (plate[1] + plate[3]) / 2.0),
        |ring| {
            let b = bbox(ring);
            ((b[0] + b[2]) / 2.0, b[3] + rule("fire.head_spacing") / 2.0)
        },
    );
    let area = super::massing::ring_area(&p.storeys[0].layout.slab);
    let count = (area / rule("fire.riser_area")).ceil().max(rule("fire.risers_min")) as usize;
    let sep = rule("fire.head_spacing");
    let xs: Vec<f64> = (0..count).map(|i| anchor.0 + sep * i as f64).filter(|x| *x < plate[2]).collect();
    let xs = if xs.is_empty() { vec![anchor.0] } else { xs };
    p.fire.risers = xs.iter().map(|x| (*x, anchor.1)).collect();
    let zone_h = rule("fire.zone_height");
    let zone_of = |k: usize| (p.storeys[k].elevation / zone_h).floor() as usize;
    let mut zones: Vec<(usize, usize)> = vec![];
    for k in 0..n {
        match zones.last_mut() {
            Some(z) if zone_of(z.0) == zone_of(k) => z.1 = k,
            _ => zones.push((k, k)),
        }
    }
    // heads of every storey, grouped by the riser nearest to their room
    let mut heads_of = vec![vec![0u32; xs.len()]; n];
    let mut built: Vec<Vec<GFireRun>> = vec![vec![]; xs.len() * n];
    let mut z_rows = vec![0.0; n];
    for k in 0..n {
        q_of[k] = vec![0.0; xs.len()];
        let ceiling = svc::ceiling(p, k);
        let st = &p.storeys[k];
        // the deflector hangs under the ceiling face by the rule, the inlet no higher than the
        // head drop rule under the soffit; the branch row stands its drop above the inlet
        let inlet = svc::svc_ports(&fixture(HEAD, "Pendent", (0.0, 0.0))).first().map_or(0.0, |c| c.at.2);
        let z_port_rel = (st.height - svc::slab() - rule("fire.head_drop")).min(ceiling - rule("fire.deflector_below") + inlet);
        let deflector = z_port_rel - inlet;
        let z_row_rel = z_port_rel + rule("fire.branch_drop");
        let z_row_rel = svc::layer(st.height, "sprinkler").map_or(z_row_rel, |z| z_row_rel.min(z));
        let pl = Plan {
            z_row: st.elevation + z_row_rel,
            z_port: st.elevation + z_port_rel,
            q_head: q_base,
            design,
        };
        z_rows[k] = pl.z_row;
        let cols = &st.layout.columns;
        let cells: Vec<Cell> = st
            .layout
            .rooms
            .iter()
            .enumerate()
            .filter(|(i, r)| {
                r.polygon.len() > 2 && !matches!(st.rooms.get(*i).map(|x| x.kind), Some(RoomKind::Stair | RoomKind::Lift))
            })
            .map(|(i, r)| {
                let hz = st.rooms.get(i).map_or(Hazard::class(super::fire_check::ORDINARY), |x| hazard_of(x.kind));
                let (xs, ys) = grid_clear(&r.polygon, &hz, cols);
                let heads = ys
                    .iter()
                    .flat_map(|y| xs.iter().map(move |x| (*x, *y)))
                    .filter(|q| placeable(&r.polygon, *q))
                    .filter_map(|q| clear_of_columns(cols, q))
                    .collect();
                Cell { room: i, poly: r.polygon.clone(), xs, ys, hz, heads }
            })
            .collect();
        let need: usize = cells.iter().map(|c| c.heads.len()).sum();
        if st.layout.fixtures.len() + need > slots::FIXTURES_MAX as usize {
            p.fire.issues.push(format!("storey {k}: {need} heads exceed the fixture capacity"));
            // heads that cannot be placed leave their rooms uncovered
            for c in &cells {
                p.fire.area += super::massing::ring_area(&c.poly);
                let b = bbox(&c.poly);
                p.fire.gaps.push(GFireGap { storey: k, room: c.room, at: ((b[0] + b[2]) / 2.0, (b[1] + b[3]) / 2.0) });
            }
            continue;
        }
        let main_y = {
            let corridor = st.rooms.iter().position(|r| r.kind == RoomKind::Corridor);
            let ys: Vec<f64> = st
                .layout
                .rooms
                .iter()
                .map(|r| {
                    let b = bbox(&r.polygon);
                    (b[1] + b[3]) / 2.0
                })
                .collect();
            match corridor.and_then(|c| ys.get(c)) {
                Some(y) => *y,
                None => ys.iter().sum::<f64>() / ys.len().max(1) as f64,
            }
        };
        let near = |x: f64| {
            (0..xs.len())
                .min_by(|a, b| (xs[*a] - x).abs().total_cmp(&(xs[*b] - x).abs()))
                .unwrap_or(0)
        };
        // per riser: the x extent its main must reach
        let mut span: Vec<(f64, f64)> = xs.iter().map(|x| (*x, *x)).collect();
        let mut fixtures: Vec<(GFixture, usize, f64)> = vec![];
        let mut runs: Vec<(usize, GFireRun)> = vec![];
        for c in &cells {
            let g = near(c.xs[0]);
            let pl = Plan { q_head: c.hz.flow(), ..pl };
            let on_main = |y: f64| (y - main_y).abs() < 1e-6;
            let rows: Vec<(f64, Vec<P2>)> = c
                .ys
                .iter()
                .map(|y| (*y, c.heads.iter().filter(|q| (q.1 - *y).abs() < 1e-6).copied().collect::<Vec<P2>>()))
                .filter(|r| !r.1.is_empty())
                .collect();
            if !rows.is_empty() {
                q_of[k][g] = q_of[k][g].max(c.hz.flow());
            }
            let Some(x0) = rows.iter().flat_map(|r| r.1.iter().map(|q| q.0)).reduce(f64::min) else { continue };
            let room_heads: u32 = rows.iter().map(|r| r.1.len() as u32).sum();
            let lo = rows.iter().map(|r| r.0).fold(main_y, f64::min);
            let hi = rows.iter().map(|r| r.0).fold(main_y, f64::max);
            for (y, row) in &rows {
                for q in row {
                    fixtures.push((fixture(HEAD, "Pendent", *q), g, c.hz.flow()));
                    if on_main(*y) {
                        span[g].0 = span[g].0.min(q.0);
                        span[g].1 = span[g].1.max(q.0);
                    }
                }
                if on_main(*y) {
                    continue;
                }
                // a row whose first cell fell outside the room starts at the trunk
                let mut pts: Vec<P2> = row.clone();
                let start = usize::from(pts[0].0 - x0 > 1e-6);
                if start == 1 {
                    pts.insert(0, (x0, *y));
                }
                let per = rule("fire.branch_heads") as usize;
                let (mut a, m) = (0, pts.len());
                while a + 1 < m {
                    let b = (a + per).min(m - 1);
                    let seg = (a..=b).map(|i| v3(pts[i].0, pts[i].1, pl.z_row)).collect();
                    runs.push((g, pl.run(FireRun::Branch, k, seg, (m - a.max(start)) as u32)));
                    a = b;
                }
            }
            if hi - lo > 1e-6 {
                span[g].0 = span[g].0.min(x0);
                span[g].1 = span[g].1.max(x0);
                let pts = vec![v3(x0, lo, pl.z_row), v3(x0, hi, pl.z_row)];
                runs.push((g, pl.run(FireRun::Branch, k, pts, room_heads)));
            }
            heads_of[k][g] += room_heads;
        }
        for (g, s) in span.iter().enumerate() {
            if heads_of[k][g] == 0 {
                continue;
            }
            let pl = Plan { q_head: q_of[k][g], ..pl };
            if s.1 - s.0 > 1e-6 {
                let pts = vec![v3(s.0, main_y, pl.z_row), v3(s.1, main_y, pl.z_row)];
                runs.push((g, pl.run(FireRun::Main, k, pts, heads_of[k][g])));
            }
            // the riser feeds the main of its storey across the plan
            if (anchor.1 - main_y).abs() > 1e-6 {
                let pts = vec![v3(xs[g], anchor.1, pl.z_row), v3(xs[g], main_y, pl.z_row)];
                let mut feed = pl.run(FireRun::Main, k, pts, heads_of[k][g]);
                // with no main on the line the feed ends at the room trunk or at nothing
                feed.run.caps.1 = s.1 - s.0 <= 1e-6 && !runs.iter().any(|r| r.0 == g && trunk_at(&r.1, xs[g], main_y));
                runs.push((g, feed));
            }
        }
        let first = p.storeys[k].layout.fixtures.len();
        let e = p.storeys[k].elevation;
        for (i, (f, g, q)) in fixtures.iter().enumerate() {
            let fx = first + i;
            let mut f = f.clone();
            f.z = deflector;
            let at = v3(f.at.0, f.at.1, pl.z_row);
            let port = v3(f.at.0, f.at.1, e + f.z + inlet);
            p.storeys[k].layout.fixtures.push(f);
            p.fire.links.push(GFireLink { storey: k, fixture: fx, connector: "Inlet" });
            p.fire.heads += 1;
            let pl = Plan { q_head: *q, ..pl };
            built[k * xs.len() + *g].push(pl.run(FireRun::Drop, k, vec![at, port], 1));
        }
        for (g, r) in runs {
            built[k * xs.len() + g].push(r);
        }
        measure(p, k, &cells);
    }
    // risers and pumps per zone
    let mut holes = vec![];
    let mut tops: std::collections::HashMap<usize, (f64, u32)> = Default::default();
    for (first, last) in zones {
        let zone_heads = |g: usize| (first..=last).map(|k| heads_of[k][g]).sum::<u32>();
        let mut zone = GFireZone { first, last, pump: (first, 0) };
        for (g, x) in xs.iter().enumerate() {
            let mut pump = fixture(PUMP, "Booster", (*x, anchor.1));
            let ports_of = svc::svc_ports(&pump);
            let out = ports_of.iter().find(|c| c.name == "Outlet").cloned();
            let inlet = ports_of.iter().find(|c| c.name == "Inlet").cloned();
            let Some(out) = out else { continue };
            pump.at = (2.0 * x - out.at.0, 2.0 * anchor.1 - out.at.1);
            let l = &mut p.storeys[first].layout.fixtures;
            if l.len() >= slots::FIXTURES_MAX as usize {
                p.fire.issues.push(format!("storey {first}: no room for the pump set"));
                continue;
            }
            let fx = l.len();
            l.push(pump);
            if g == 0 {
                zone.pump = (first, fx);
            }
            p.fire.links.push(GFireLink { storey: first, fixture: fx, connector: "Outlet" });
            let heads = zone_heads(g);
            let z0 = p.storeys[first].elevation + out.at.2;
            let z1 = z_rows[last];
            let q_head = (first..=last).map(|k| q_of[k][g]).fold(0.0, f64::max);
            let q_head = if q_head > 0.0 { q_head } else { q_base };
            let pl = Plan { z_row: z1, z_port: z0, q_head, design };
            let flow = f64::from(heads).min(design) * q_head;
            let mut run = pl.run(FireRun::Riser, first, vec![v3(*x, anchor.1, z0), v3(*x, anchor.1, z1)], heads);
            run.run.caps.1 = true;
            // the pump of an upper zone takes its water from the top of the zone below
            if let (Some(inlet), Some(below)) = (inlet, tops.get(&g).copied()) {
                let (heads_below, ix) = (below.1, *x + inlet.at.0 - out.at.0);
                let pts = vec![v3(*x, anchor.1, below.0), v3(ix, anchor.1, below.0), v3(ix, anchor.1, p.storeys[first].elevation + inlet.at.2)];
                p.fire.runs.push(pl.run(FireRun::Riser, first, pts, heads_below));
                if let Some(prev) = p.fire.runs.iter_mut().rev().find(|r| r.kind == FireRun::Riser && r.run.points.last().is_some_and(|q| (q.0 - *x).abs() < 1e-6 && (q.2 - below.0).abs() < 1e-6) && r.run.caps.1) {
                    prev.run.caps.1 = false;
                }
                holes.push((first, svc::hole(ix, anchor.1, svc::od(PipeMaterial::Steel, steel(flow)))));
                p.fire.links.push(GFireLink { storey: first, fixture: fx, connector: "Inlet" });
            }
            tops.insert(g, (z1, heads));
            let od = svc::od(PipeMaterial::Steel, steel(flow));
            for k in first + 1..=last {
                holes.push((k, svc::hole(*x, anchor.1, od)));
            }
            p.fire.runs.push(run);
        }
        p.fire.zones.push(zone);
    }
    for r in built.into_iter().flatten() {
        p.fire.runs.push(r);
    }
    svc::add_holes(p, &holes);
    let ports = ports(p);
    let refs: Vec<&super::GRun> = p.fire.runs.iter().map(|r| &r.run).collect();
    let faults = svc::net_faults(&refs, &ports);
    p.fire.issues.extend(faults);
}

/// A run that starts or ends at the plan point.
fn trunk_at(r: &GFireRun, x: f64, y: f64) -> bool {
    let near = |q: &(f64, f64, f64)| (q.0 - x).abs() < 1e-6 && (q.1 - y).abs() < 1e-6;
    r.run.points.first().is_some_and(near) || r.run.points.last().is_some_and(near)
}

/// World points of the connectors the system joins.
pub(crate) fn ports(p: &GProject) -> Vec<V3> {
    p.fire
        .links
        .iter()
        .filter_map(|l| svc::port_world(p, l.storey, l.fixture, l.connector))
        .map(|c| v3(c.at.0, c.at.1, c.at.2))
        .collect()
}

/// The share of a room's floor within `reach` of a head, sampled every `step`, and the first
/// sample out of reach.
#[cfg_attr(not(test), allow(dead_code))]
pub(super) fn coverage(poly: &[P2], heads: &[P2], reach: f64, step: f64) -> (f64, Option<P2>) {
    coverage_by(poly, step, |q| heads.iter().any(|h| (h.0 - q.0).hypot(h.1 - q.1) <= reach))
}

/// The share of a room's floor samples, every `step`, that `hit` accepts, and the first that
/// it does not.
pub(super) fn coverage_by(poly: &[P2], step: f64, hit_at: impl Fn(P2) -> bool) -> (f64, Option<P2>) {
    let b = bbox(poly);
    let (mut total, mut hit, mut gap) = (0usize, 0usize, None);
    let mut y = b[1] + step / 2.0;
    while y < b[3] {
        let mut x = b[0] + step / 2.0;
        while x < b[2] {
            if inside(poly, (x, y)) {
                total += 1;
                if hit_at((x, y)) {
                    hit += 1;
                } else if gap.is_none() {
                    gap = Some((x, y));
                }
            }
            x += step;
        }
        y += step;
    }
    (if total > 0 { hit as f64 / total as f64 } else { 0.0 }, gap)
}

/// Floor area of a storey's rooms and the part a placed head protects: a head protects the
/// points within its class's wall distance along both plan axes.
fn measure(p: &mut GProject, k: usize, cells: &[Cell]) {
    for c in cells {
        let area = super::massing::ring_area(&c.poly);
        let (share, gap) = coverage_sq(&c.poly, &c.heads, &c.hz);
        p.fire.area += area;
        p.fire.covered += area * share;
        if let Some(at) = gap {
            p.fire.gaps.push(GFireGap { storey: k, room: c.room, at });
        }
    }
}
