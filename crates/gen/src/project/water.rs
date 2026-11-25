// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Cold and hot water and drainage: a riser cluster (stack, cold and hot riser) boxed into a
//! corner of each wet room and shared by the wet rooms stacked over it; supply mains in wall
//! chases low along the walls, a rise in the wall to each connector; drains in the void under
//! the slab falling to the stack; mains and the building drain under the ground slab to a
//! capped inlet and outlet; the stack vented above the roof.

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::{self, PipeMaterial};

use super::rules::rule;
use super::svc::{self, norms, Room, Space, SvcPort, R};
use super::{GProject, GRun, GSvcLink, ProjectSpec, SvcSys, Typology};

/// Gap between the pipes of a riser cluster and from the walls, m.
fn g() -> f64 {
    rule("services.water.cluster_gap")
}
/// Supply mains in the chase above the floor, m.
fn z_cw() -> f64 {
    rule("services.water.chase_cw")
}
fn z_hw() -> f64 {
    rule("services.water.chase_hw")
}
/// Under the ground slab: the mains, the top of the building drain, m.
fn under_cw() -> f64 {
    -rule("services.water.under_cw")
}
fn under_hw() -> f64 {
    -rule("services.water.under_hw")
}
fn under_drain() -> f64 {
    -rule("services.water.under_drain")
}
/// Inlet and outlet beyond the outline, m.
fn out_len() -> f64 {
    rule("services.water.outlet_len")
}
/// Stack DN where a WC discharges into it, mm.
fn stack_dn_wc() -> u32 {
    rule("services.water.stack_dn_wc") as u32
}
/// Smallest DN of the building drain, mm.
fn drain_dn_min() -> u32 {
    rule("services.water.building_drain_dn") as u32
}


#[derive(Clone, Copy, Debug)]
struct Frame {
    o: (f64, f64),
    a: (f64, f64),
    b: (f64, f64),
}

impl Frame {
    fn p(&self, u: f64, v: f64) -> (f64, f64) {
        (self.o.0 + self.a.0 * u + self.b.0 * v, self.o.1 + self.a.1 * u + self.b.1 * v)
    }
    fn uv(&self, q: (f64, f64)) -> (f64, f64) {
        let d = (q.0 - self.o.0, q.1 - self.o.1);
        (d.0 * self.a.0 + d.1 * self.a.1, d.0 * self.b.0 + d.1 * self.b.1)
    }
    fn same(&self, o: &Frame) -> bool {
        let near = |p: (f64, f64), q: (f64, f64)| (p.0 - q.0).abs() < 0.02 && (p.1 - q.1).abs() < 0.02;
        near(self.o, o.o) && near(self.a, o.a) && near(self.b, o.b)
    }
}

/// The order a room's corners are tried for a new stack and its risers: nearest a riser shaft
/// first when the storey has riser shafts (`shafts`, plan outlines), else as they come.
pub(crate) fn shaft_order(corners: &[(f64, f64)], shafts: &[Vec<(f64, f64)>]) -> Vec<usize> {
    let mut order: Vec<usize> = (0..corners.len()).collect();
    if shafts.is_empty() {
        return order;
    }
    let to_seg = |q: (f64, f64), a: (f64, f64), b: (f64, f64)| {
        let (dx, dy) = (b.0 - a.0, b.1 - a.1);
        let l2 = dx * dx + dy * dy;
        let t = if l2 > 0.0 { (((q.0 - a.0) * dx + (q.1 - a.1) * dy) / l2).clamp(0.0, 1.0) } else { 0.0 };
        (q.0 - a.0 - t * dx).hypot(q.1 - a.1 - t * dy)
    };
    let dist = |q: (f64, f64)| {
        shafts
            .iter()
            .flat_map(|ring| (0..ring.len()).map(move |i| (ring[i], ring[(i + 1) % ring.len()])))
            .map(|(a, b)| to_seg(q, a, b))
            .fold(f64::INFINITY, f64::min)
    };
    order.sort_by(|&x, &y| dist(corners[x]).total_cmp(&dist(corners[y])));
    order
}

fn corner(room: &Room, ci: usize) -> Frame {
    let s = &room.sides[ci];
    Frame { o: s.o, a: s.t, b: s.n() }
}

/// A stack with its cold and hot risers, in the frame of the corner it is boxed into.
struct Cluster {
    f: Frame,
    stack_dn: u32,
    rs: f64,
    cw: (f64, f64),
    hw: (f64, f64),
    rc: f64,
    rh: f64,
    /// Stack centre in the frame.
    s: (f64, f64),
    /// Storeys joined: storey, cold and hot flows of the room, discharge units, stack join z.
    joins: Vec<Join>,
    vent: f64,
}

#[derive(Clone, Debug)]
struct Join {
    k: usize,
    cw: f64,
    hw: f64,
    du: f64,
    wc: bool,
    stack_z: Option<f64>,
    cz: f64,
    hz: f64,
}

impl Cluster {
    fn new(f: Frame, wc: bool, cw_q: f64, hw_q: f64) -> Cluster {
        let stack_dn = if wc { stack_dn_wc() } else { rule("services.water.stack_dn") as u32 };
        let rs = svc::od(PipeMaterial::Pvc, stack_dn) / 2.0;
        let q_min = rule("services.water.riser_flow_min");
        let (mc, dc) = svc::supply_pipe(cw_q.max(q_min));
        let (mh, dh) = svc::supply_pipe(hw_q.max(q_min));
        let (rc, rh) = (svc::od(mc, dc) / 2.0, svc::od(mh, dh) / 2.0);
        let mut c = Cluster {
            f,
            stack_dn,
            rs,
            cw: (0.0, 0.0),
            hw: (0.0, 0.0),
            rc,
            rh,
            s: (0.0, 0.0),
            joins: vec![],
            vent: 0.0,
        };
        c.set_stack((g() + rs, g() + rs));
        c
    }
    /// Stands the stack at `s`, the risers in a row along u past it.
    fn set_stack(&mut self, s: (f64, f64)) {
        let (g, pad) = (g(), rule("services.water.riser_pad"));
        self.s = s;
        self.cw = (s.0 + self.rs + g + self.rc, 2.0 * self.rc + pad);
        self.hw = (self.cw.0 + self.rc + g + self.rh, 2.0 * self.rh + pad);
    }
    fn stack(&self) -> (f64, f64) {
        self.f.p(self.s.0, self.s.1)
    }
    /// Radius of the widest vertical in the row.
    fn row(&self) -> f64 {
        self.rs.max(self.rc).max(self.rh)
    }
    fn cw_xy(&self) -> (f64, f64) {
        self.f.p(self.cw.0, self.cw.1)
    }
    fn hw_xy(&self) -> (f64, f64) {
        self.f.p(self.hw.0, self.hw.1)
    }
    /// Plan box of the cluster with its boxing-in.
    fn bbox(&self, risers: bool) -> R {
        let g = g();
        let (u1, v1) = if risers {
            (self.hw.0 + self.rh + g, (self.cw.1 + self.rc).max(self.hw.1 + self.rh).max(self.s.1 + self.rs) + g)
        } else {
            (self.s.0 + self.rs + g, self.s.1 + self.rs + g)
        };
        svc::bbox(&[self.f.p(0.0, 0.0), self.f.p(u1, 0.0), self.f.p(u1, v1), self.f.p(0.0, v1)])
    }
}

/// Wet fixtures of the rooms of a storey: the room and the fixture indices in it.
fn wet_rooms(p: &GProject, k: usize) -> Vec<(Room, Vec<usize>)> {
    let st = &p.storeys[k];
    let rooms = svc::rooms(st);
    let mut out: Vec<(Room, Vec<usize>)> = rooms.into_iter().map(|r| (r, vec![])).collect();
    let margin = rule("services.water.wet_margin");
    for (i, f) in st.layout.fixtures.iter().enumerate() {
        if !svc::svc_ports(f).iter().any(|c| c.sys != SvcSys::Power) {
            continue;
        }
        if let Some(r) = out.iter_mut().find(|r| svc::inside(f.at, &svc::grow(&r.0.clear, margin))) {
            r.1.push(i);
        }
    }
    out.retain(|r| !r.1.is_empty());
    out.sort_by_key(|r| std::cmp::Reverse(r.1.len()));
    out
}

/// Top of the roof over a plan point, m above the project base.
fn roof_top(p: &GProject, q: (f64, f64)) -> f64 {
    let roofs = std::iter::once(&p.roof).chain(&p.extra_roofs);
    let mut best: Option<f64> = None;
    for r in roofs {
        let b = svc::bbox(&r.ring);
        if !svc::inside(q, &b) {
            continue;
        }
        let e = (q.0 - b[0]).min(b[2] - q.0).min(q.1 - b[1]).min(b[3] - q.1);
        let z = r.elevation + super::roof_thickness(r.slope) + e * r.slope.to_radians().tan();
        best = Some(best.map_or(z, |x: f64| x.max(z)));
    }
    best.unwrap_or(p.roof.elevation + super::roof_thickness(p.roof.slope))
}

/// The use class of a typology in the drainage and supply tables.
fn use_of(t: Typology) -> &'static str {
    match t {
        Typology::House | Typology::ApartmentSection => "res",
        Typology::Office => "office",
        Typology::School => "public",
    }
}

fn k_factor(t: Typology) -> f64 {
    norms().k(use_of(t))
}

/// Peak flows of the supply pipes: the use class and the largest single cold and hot flow.
struct Peak {
    use_: &'static str,
    max: (f64, f64),
}

impl Peak {
    fn q(&self, sys: SvcSys, sum: f64) -> f64 {
        norms().peak(self.use_, sum, if sys == SvcSys::Hot { self.max.1 } else { self.max.0 })
    }
}

/// What one wet room adds: its runs, links, flows and the stack join.
#[derive(Default)]
struct RoomDesign {
    runs: Vec<GRun>,
    links: Vec<(usize, &'static str, SvcSys)>,
    cw: f64,
    hw: f64,
    du: f64,
    wc: bool,
    stack_z: Option<f64>,
    /// Heights of the cold and hot chases above the floor.
    cz: f64,
    hz: f64,
    served: usize,
    ports: usize,
}

/// A supply connector placed on a chase: ring length, side, along, depth, the connector.
#[derive(Clone, Copy)]
struct Att {
    u: f64,
    j: usize,
    s: f64,
    d: f64,
    fx: usize,
    c: SvcPort,
}

/// An upright run already in the space: plan point, bottom, top, half width.
type Upright = ((f64, f64), f64, f64, f64);

/// Cold or hot supply of a room from the cluster's riser: the riser branch to the chase, the
/// two chase arms, a rise to each connector. `None` when nothing could be joined.
#[allow(clippy::too_many_arguments)]
fn supply(
    p: &GProject,
    k: usize,
    room: &Room,
    fx: &[usize],
    cl: &Cluster,
    sys: SvcSys,
    depth: f64,
    main_r: f64,
    zr: f64,
    pk: &Peak,
    ups: &[Upright],
) -> (Vec<GRun>, Vec<(usize, &'static str)>, f64, usize) {
    let st = &p.storeys[k];
    let e = st.elevation;
    let z = e + zr;
    let bridge_bend = svc::bend_of(PipeMaterial::Pp, svc::supply_pipe(0.0).1);
    let (thin_extra, thin_pad) = (rule("services.water.wall_extra"), rule("services.water.block_pad"));
    let (sill_margin, gap_pad) = (rule("services.water.sill_margin"), rule("services.water.gap_pad"));
    let (port_align, port_reach) = (rule("services.water.port_align"), rule("services.water.port_reach"));
    let port_down = rule("services.water.port_reach_down");
    let tap_corner = rule("services.water.tap_corner");
    let (arm_min, tee_min) = (rule("services.water.arm_min"), rule("services.water.tee_min"));
    let q_min = rule("services.water.flow_min");
    let ring = svc::Ring::new(room, -depth);
    let per = ring.per();
    // where the riser branch meets the chase: side A of the cluster corner
    let ci = (0..4).find(|&j| cl.f.same(&corner(room, j)));
    let Some(ci) = ci else { return (vec![], vec![], 0.0, 0) };
    let ru = if sys == SvcSys::Cold { cl.cw } else { cl.hw };
    let u0 = ring.u(ci, ru.0);
    // blocked: openings low enough to cut the chase, walls too thin for it; the chase bridges
    // an opening over its head when that stays under the slab
    let z_top = e + st.height - svc::slab() - main_r - svc::gap();
    let bridge_clear = rule("services.water.bridge_clear");
    let mut blocked: Vec<(f64, f64, Option<f64>, bool)> = vec![];
    for (j, sd) in room.sides.iter().enumerate() {
        if 2.0 * sd.half < depth + main_r + thin_extra {
            blocked.push((ring.u(j, -depth) - thin_pad, ring.u(j, sd.len + depth) + thin_pad, None, false));
            continue;
        }
        for g in &sd.gaps {
            if g.sill < z - e + main_r + sill_margin {
                let over = Some(e + g.head + bridge_clear + main_r).filter(|zo| *zo <= z_top);
                blocked.push((ring.u(j, g.s0) - gap_pad, ring.u(j, g.s1) + gap_pad, over, false));
            }
        }
        // the neighbour's rises in a shared wall: bridged like an opening
        for &(xy, z0, z1, r) in ups {
            let (s, pad) = (sd.s_of(xy), r + main_r + svc::gap());
            if s < 0.0 || s > sd.len || (sd.d_of(xy) + depth).abs() >= pad || z1 < z - pad || z0 > z + pad {
                continue;
            }
            // the leg over it holds its two bends
            let (over, pad) = (Some(z1 + bridge_clear + main_r).filter(|zo| *zo <= z_top), pad + bridge_bend);
            blocked.push((ring.u(j, s) - pad, ring.u(j, s) + pad, over, true));
        }
    }
    // bridges that overlap or meet are one bridge at the higher height: two would cross each other
    blocked.sort_by(|a, b| a.0.total_cmp(&b.0));
    let mut one: Vec<(f64, f64, Option<f64>, bool)> = vec![];
    for b in blocked {
        match one.last_mut() {
            Some(l) if l.2.is_some() && b.2.is_some() && b.0 <= l.1 + bridge_bend => {
                *l = (l.0, l.1.max(b.1), l.2.zip(b.2).map(|(x, y)| x.max(y)), l.3 && b.3);
            }
            _ => one.push(b),
        }
    }
    let blocked = one;
    let fwd = |u: f64| (u - u0).rem_euclid(per);
    let fwd_block = blocked.iter().filter(|b| b.2.is_none()).map(|b| fwd(b.0)).fold(per, f64::min);
    let bwd_block = blocked.iter().filter(|b| b.2.is_none()).map(|b| per - fwd(b.1)).fold(per, f64::min);
    // the bridges along each arm: from, to, height
    let bridges: [Vec<(f64, f64, f64, bool)>; 2] = [
        blocked.iter().filter_map(|b| b.2.map(|zo| (fwd(b.0), fwd(b.1), zo, b.3))).filter(|b| b.1 < fwd_block).collect(),
        blocked.iter().filter_map(|b| b.2.map(|zo| (per - fwd(b.1), per - fwd(b.0), zo, b.3))).filter(|b| b.1 < bwd_block).collect(),
    ];
    // no tap over an opening, nor at a bridge's legs; over a rise it leaves at the bridge height
    let bridged = |dir: usize, d: f64| bridges[dir].iter().any(|b| d > b.0 - tee_min && d < b.1 + tee_min && (!b.3 || (d - b.0).abs() < tee_min || (d - b.1).abs() < tee_min));
    let z_at = |dir: usize, d: f64| bridges[dir].iter().find(|b| d > b.0 && d < b.1).map_or(z, |b| b.2);
    if blocked.iter().any(|b| fwd(b.0) > fwd(b.1)) {
        return (vec![], vec![], 0.0, 0);
    }
    // per connector, every wall it can be fed from
    let mut atts: Vec<Vec<Att>> = vec![];
    let mut ports = 0;
    for &i in fx {
        let f = &st.layout.fixtures[i];
        for c in svc::svc_ports(f).into_iter().filter(|c| c.sys == sys) {
            ports += 1;
            let q = (c.at.0, c.at.1);
            let sides: Vec<usize> = (0..room.sides.len())
                .filter(|&j| {
                    let sd = &room.sides[j];
                    let n = sd.n();
                    let s = sd.s_of(q);
                    let facing = -(c.dir.0 * n.0 + c.dir.1 * n.1) > port_align && sd.d_of(q).abs() < port_reach;
                    // a connector on the underside is fed by a short branch from the wall
                    let down = c.dir.2 < -port_align && sd.d_of(q).abs() < port_down;
                    // the ring turns its corner the chase depth beyond the room's: a tap keeps the
                    // elbow and tee length from it
                    let (near, far) = (s + depth, sd.len - s + depth);
                    (facing || down) && s > 0.0 && s < sd.len && near >= tap_corner && far >= tap_corner
                })
                .collect();
            let alts: Vec<Att> = sides
                .into_iter()
                .map(|j| {
                    let sd = &room.sides[j];
                    let s = sd.s_of(q);
                    Att { u: ring.u(j, s), j, s, d: sd.d_of(q), fx: i, c }
                })
                .collect();
            if !alts.is_empty() {
                atts.push(alts);
            }
        }
    }
    // every way a connector can be reached: an open arm of any wall it can be fed from, the
    // nearest first
    let options = |alts: &[Att]| -> Vec<(f64, usize, Att)> {
        let mut o: Vec<(f64, usize, Att)> = vec![];
        for a in alts {
            let (df, db) = (fwd(a.u), per - fwd(a.u));
            if df < fwd_block && df > arm_min && !bridged(0, df) {
                o.push((df, 0, *a));
            }
            if db < bwd_block && db > arm_min && !bridged(1, db) {
                o.push((db, 1, *a));
            }
        }
        o.sort_by(|x, y| x.0.total_cmp(&y.0));
        o
    };
    // nearest connectors placed first, each on its nearest way that keeps the tee spacing from
    // those already on that arm; a connector crowded off one arm or wall takes the next way
    let mut opts: Vec<Vec<(f64, usize, Att)>> = atts.iter().map(|a| options(a.as_slice())).filter(|o| !o.is_empty()).collect();
    opts.sort_by(|x, y| x[0].0.total_cmp(&y[0].0));
    let mut arms: [Vec<(f64, Att)>; 2] = [vec![], vec![]];
    for o in &opts {
        if let Some(&(d, dir, a)) = o.iter().find(|(d, dir, _)| arms[*dir].iter().all(|x| (x.0 - d).abs() >= tee_min)) {
            arms[dir].push((d, a));
        }
    }
    let mut runs = vec![];
    let mut links = vec![];
    let mut flow = 0.0;
    let mut branch_runs = vec![];
    for (dir, arm) in arms.iter_mut().enumerate() {
        arm.sort_by(|x, y| x.0.total_cmp(&y.0));
        let kept: Vec<&(f64, Att)> = arm.iter().collect();
        if kept.is_empty() {
            continue;
        }
        let q: f64 = kept.iter().map(|a| a.1.c.flow).sum();
        flow += q;
        let arm_d = |j: usize, s_t: f64| {
            let d = fwd(ring.u(j, s_t));
            if dir == 0 { d } else { per - d }
        };
        let reach = if dir == 0 { fwd_block } else { bwd_block };
        // a waste of the fixture into the same wall crosses a rise: the rise stands off it
        // along the wall, on the arm, and runs across to the tap above it
        let taps: Vec<Option<f64>> = kept
            .iter()
            .map(|(_, a)| {
                let sd = &room.sides[a.j];
                let pz = e + a.c.at.2;
                let (m, dn) = svc::supply_pipe(a.c.flow.max(q_min));
                let ws = svc::svc_ports(&st.layout.fixtures[a.fx])
                    .into_iter()
                    .filter(|c| c.sys == SvcSys::Sanitary && c.dir.2 > -port_align && z.min(pz) < e + c.at.2 && e + c.at.2 < z.max(pz))
                    .map(|c| (sd.s_of((c.at.0, c.at.1)), c.size))
                    .find(|w| (w.0 - a.s).abs() < w.1 / 2.0 + svc::od(m, dn) / 2.0 + rule("services.water.waste_clear"))?;
                let clear = (ws.1 / 2.0 + svc::od(m, dn) / 2.0 + rule("services.water.waste_clear")).max(2.0 * svc::bend_of(m, dn) + svc::gap());
                [ws.0 - clear, ws.0 + clear]
                    .into_iter()
                    .filter(|s_t| *s_t > 0.0 && *s_t < sd.len && arm_d(a.j, *s_t) > arm_min && arm_d(a.j, *s_t) < reach && !bridged(dir, arm_d(a.j, *s_t)))
                    .min_by(|x, y| arm_d(a.j, *x).total_cmp(&arm_d(a.j, *y)))
            })
            .collect();
        let end = kept.iter().zip(&taps).map(|((d, a), t)| t.map_or(*d, |s| arm_d(a.j, s))).fold(0.0, f64::max);
        let at = |d: f64| if dir == 0 { u0 + d } else { u0 - d };
        let mut legs: Vec<(f64, f64, f64)> = vec![];
        let mut cur = 0.0;
        let mut over: Vec<&(f64, f64, f64, bool)> = bridges[dir].iter().filter(|b| b.0 < end).collect();
        over.sort_by(|x, y| x.0.total_cmp(&y.0));
        for b in over {
            legs.push((cur, b.0, z));
            legs.push((b.0, b.1.min(end), b.2));
            cur = b.1.min(end);
        }
        if end > cur {
            legs.push((cur, end, z));
        }
        let mut pts: Vec<V3> = vec![];
        for (d0, d1, zl) in legs {
            for w in ring.path(at(d0), at(d1)) {
                let q = v3(w.0, w.1, zl);
                if pts.last().is_none_or(|l: &V3| (l.x - q.x).abs() + (l.y - q.y).abs() + (l.z - q.z).abs() > 1e-9) {
                    pts.push(q);
                }
            }
        }
        let (m, dn) = svc::supply_pipe(pk.q(sys, q).max(q_min));
        runs.push(svc::pipe_run(sys, k, m, dn, pts));
        for ((_, a), tap) in kept.into_iter().zip(taps) {
            let sd = &room.sides[a.j];
            let w = sd.at(a.s, -depth);
            let pz = e + a.c.at.2;
            let (m, dn) = svc::supply_pipe(a.c.flow.max(q_min));
            let z = z_at(dir, tap.map_or(arm_d(a.j, a.s), |s_t| arm_d(a.j, s_t)));
            let mut pts = vec![v3(w.0, w.1, z), v3(w.0, w.1, pz), v3(a.c.at.0, a.c.at.1, pz)];
            if let Some(s_t) = tap {
                let t = sd.at(s_t, -depth);
                pts = vec![v3(t.0, t.1, z), v3(t.0, t.1, pz), v3(w.0, w.1, pz), v3(a.c.at.0, a.c.at.1, pz)];
            }
            if a.c.dir.2 < -port_align {
                // an underside connector is entered from below: along under it, then up into it
                let low = pz - svc::bend_of(m, dn) - svc::gap();
                if low > z || z > pz {
                    pts = vec![v3(w.0, w.1, z), v3(w.0, w.1, low), v3(a.c.at.0, a.c.at.1, low), v3(a.c.at.0, a.c.at.1, pz)];
                }
            }
            branch_runs.push(svc::pipe_run(sys, k, m, dn, pts));
            links.push((a.fx, a.c.name));
        }
    }
    if runs.is_empty() {
        return (vec![], vec![], 0.0, ports);
    }
    let (from, to) = (cl.f.p(ru.0, ru.1), cl.f.p(ru.0, -depth));
    let (m, dn) = svc::supply_pipe(pk.q(sys, flow).max(q_min));
    runs.insert(0, svc::pipe_run(sys, k, m, dn, vec![v3(from.0, from.1, z), v3(to.0, to.1, z)]));
    runs.extend(branch_runs);
    (runs, links, flow, ports)
}

/// A drain foot: the fixture, its waste connector, the plan point under it, the branch DN.
struct Foot {
    fx: usize,
    c: SvcPort,
    uv: (f64, f64),
    dn: u32,
    du: f64,
    wc: bool,
}

/// Drains of a room in the void under its slab: a collector along one axis of the corner frame
/// turning into the stack, a branch from each foot square to it. Returns the runs, the stack
/// join z and the links, trying collector lines until one fits.
type Drains = (Vec<GRun>, f64, Vec<(usize, &'static str)>, f64, bool);

fn drains(space: &mut Space<'_>, p: &GProject, k: usize, room: &Room, fx: &[usize], cl: &Cluster) -> Option<Drains> {
    // a wall waste drops behind its connector; deeper in the wall when that one meets the chases
    [rule("services.water.waste_off"), rule("services.water.waste_deep")]
        .into_iter()
        .find_map(|off| drains_at(space, p, k, room, fx, cl, off))
}

fn drains_at(space: &mut Space<'_>, p: &GProject, k: usize, room: &Room, fx: &[usize], cl: &Cluster, waste_off: f64) -> Option<Drains> {
    let st = &p.storeys[k];
    let e = st.elevation;
    let n = norms();
    let mut feet = vec![];
    let waste_down = rule("services.water.waste_down");
    for &i in fx {
        let f = &st.layout.fixtures[i];
        for c in svc::svc_ports(f).into_iter().filter(|c| c.sys == SvcSys::Sanitary) {
            let mut q = (c.at.0, c.at.1);
            if c.dir.2 > -waste_down {
                // a waste into the wall: down inside it behind the connector, within the wall
                let wall = room.sides.iter().filter(|sd| sd.d_of(q).abs() < rule("services.water.port_reach")).map(|sd| 2.0 * sd.half).fold(0.0, f64::max);
                let off = if waste_off + c.size / 2.0 <= wall { waste_off } else { rule("services.water.waste_off") };
                q = (q.0 + c.dir.0 * off, q.1 + c.dir.1 * off);
            }
            let fam = f.family;
            feet.push(Foot {
                fx: i,
                c,
                uv: cl.f.uv(q),
                dn: n.branch_dn(fam).max((c.size * 1000.0).round() as u32),
                du: n.du(fam),
                wc: fam == "WC",
            });
        }
    }
    if feet.is_empty() {
        return None;
    }
    let pvc = PipeMaterial::Pvc;
    let dn_c = if feet.iter().any(|f| f.wc) {
        stack_dn_wc()
    } else {
        feet.iter().map(|f| f.dn).max().unwrap_or(rule("services.water.collector_dn") as u32)
    };
    let (oc, os) = (svc::od(pvc, dn_c), 2.0 * cl.rs);
    let (sc, ss) = (n.slope(dn_c) / 100.0, cl.s);
    // what the fittings at a stretch end take from it; falls turn an elbow past a right angle
    let s_max = feet.iter().map(|f| n.slope(f.dn)).fold(n.slope(dn_c), f64::max) / 100.0;
    let tan_h = (std::f64::consts::FRAC_PI_4 + s_max.atan()).tan();
    let sec = |dn: u32| svc::pipe_run(SvcSys::Sanitary, k, pvc, dn, vec![]).section;
    let elbow = |dn: u32| svc::bend_of(pvc, dn) * tan_h;
    // two sizes meeting: the smaller one's elbow, the reducer on the larger
    let joint = |a: u32, b: u32| if a == b { elbow(a) } else { elbow(a.min(b)) + mep::transition_length(&sec(a), &sec(b)) };
    let tc = elbow(dn_c);
    let tee_c = mep::junction_leg(&sec(dn_c));
    // the stack runs on through the join (down to the drain, up to the vent): a tee on it
    let stack_end = mep::junction_leg(&sec(cl.stack_dn));
    // under the slab, above the ceiling below (ground floor: free below)
    let z_max = e - svc::slab() - rule("services.water.slab_clear") - oc / 2.0;
    let z_min = if k == 0 {
        f64::NEG_INFINITY
    } else {
        p.storeys[k - 1].elevation + svc::ceiling(p, k - 1) + rule("services.water.ceiling_clear") + oc / 2.0
    };
    // the layer depth never takes the pipe below the void: the falls need the room above z_min
    let z_max = svc::depth_water(oc).map_or(z_max, |d| z_max.min(e - svc::slab() - d).max(z_min.min(z_max)));
    let (gap, join_min, step) = (svc::gap(), rule("services.water.join_gap"), rule("services.water.drain_step"));
    let shift_max = rule("services.water.waste_shift");
    let (w, d) = (room.clear[2] - room.clear[0], room.clear[3] - room.clear[1]);
    let span = w.max(d) + 0.5;
    let c = room.clear;
    let room_uv: Vec<(f64, f64)> = [(c[0], c[1]), (c[2], c[1]), (c[2], c[3]), (c[0], c[3])].iter().map(|q| cl.f.uv(*q)).collect();
    let dir_uv = |d: (f64, f64, f64)| cl.f.uv((cl.f.o.0 + d.0, cl.f.o.1 + d.1));
    // tees apart by their legs; the far join ends the collector in a joint
    let sp_tee = join_min.max(2.0 * tee_c + gap);
    let sp_end = |dn: u32| join_min.max(tee_c + joint(dn, dn_c) + gap);
    let jog_min = |dn: u32| 2.0 * elbow(dn) + gap;
    for swap in [false, true] {
        // collector along u at v = line (swap: along v at u = line)
        let uv = |q: (f64, f64)| if swap { (q.1, q.0) } else { q };
        let s0 = if swap { ss.0 } else { ss.1 };
        let a_s = if swap { ss.1 } else { ss.0 };
        // the risers stand in a row along u from the stack: a collector along v first leaves
        // the stack along v past the row, then turns across to its line
        let a_turn = if swap {
            ((cl.cw.1 + cl.rc).max(cl.hw.1 + cl.rh) + gap + oc / 2.0).max(a_s + stack_end + tc + gap)
        } else {
            a_s
        };
        let a_max = room_uv.iter().map(|q| uv(*q).0).fold(f64::MIN, f64::max) - g();
        // along v the collector may also leave the stack straight, clear of the row beside it
        let mut lines: Vec<f64> = if swap { vec![s0] } else { vec![] };
        let mut l = s0 + tc + if swap { tc } else { os.max(stack_end) } + gap;
        while l < span {
            lines.push(l);
            l += step;
        }
        for line in lines {
            let straight = (line - s0).abs() < 1e-9;
            let (a_c, ext) = if straight { (a_s, 0.0) } else { (a_turn, a_turn - a_s) };
            let a_0 = if straight { a_s + stack_end } else { a_c + tc };
            // per foot: its index, the plan point of its drop, the join along the collector
            let mut plan: Vec<(usize, (f64, f64), f64)> = vec![];
            let mut free: Vec<usize> = vec![];
            let mut ok = true;
            for (i, f) in feet.iter().enumerate() {
                let (a, b) = uv(f.uv);
                if (b - line).abs() >= elbow(f.dn) + oc.max(joint(f.dn, dn_c)) - 1e-9 {
                    free.push(i);
                    continue;
                }
                // too near the line for a branch: a wall waste runs on along its outlet to drop
                // straight into the collector
                let (ca, cb) = uv(cl.f.uv((f.c.at.0, f.c.at.1)));
                let (da, db) = uv(dir_uv(f.c.dir));
                let at = if (b - line).abs() < 1e-9 {
                    Some(a)
                } else if f.c.dir.2 > -waste_down && db.abs() > waste_down {
                    let t = (line - cb) / db;
                    (t >= -1e-9 && t <= shift_max + 1e-9).then_some(ca + da * t)
                } else {
                    None
                };
                match at {
                    Some(at) => plan.push((i, (at, line), at)),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            // the free feet, nearest the stack first, each at its own place along the line or
            // jogged to the nearest one clear of the joins already set
            let a_lo = a_0 + tee_c + gap;
            free.sort_by(|x, y| uv(feet[*x].uv).0.total_cmp(&uv(feet[*y].uv).0));
            for &i in &free {
                let f = &feet[i];
                let (a, b) = uv(f.uv);
                let mut cands = vec![a, a_lo, a - jog_min(f.dn), a + jog_min(f.dn), a_lo.max(a + jog_min(f.dn))];
                for p in &plan {
                    cands.extend([p.2 - sp_tee, p.2 + sp_tee]);
                }
                let best = cands
                    .into_iter()
                    .filter(|x| *x >= a_lo - 1e-9 && *x <= a_max + 1e-9)
                    .filter(|x| (x - a).abs() < 1e-9 || (x - a).abs() >= jog_min(f.dn) - 1e-9)
                    .filter(|x| plan.iter().all(|p| (p.2 - x).abs() >= sp_tee - 1e-9))
                    .min_by(|x, y| (x - a).abs().total_cmp(&(y - a).abs()).then(x.total_cmp(y)));
                match best {
                    Some(x) => plan.push((i, (a, b), x)),
                    None => {
                        ok = false;
                        break;
                    }
                }
            }
            if !ok {
                continue;
            }
            plan.sort_by(|x, y| x.2.total_cmp(&y.2));
            // the far join is the collector's end: its joint takes more than a tee
            let n_j = plan.len();
            if n_j > 1 {
                let (i, (a, _), x) = plan[n_j - 1];
                let want = plan[n_j - 2].2 + sp_end(feet[i].dn);
                if x < want && free.contains(&i) && (want - a).abs() >= jog_min(feet[i].dn) - 1e-9 && want <= a_max + 1e-9 {
                    plan[n_j - 1].2 = want;
                }
            }
            ok &= n_j != 1 || plan[0].2 - a_0 >= joint(feet[plan[0].0].dn, dn_c) + gap - 1e-9;
            ok &= plan[0].2 >= a_lo - 1e-9;
            ok &= (1..n_j).all(|i| {
                let end = if i + 1 == n_j { sp_end(feet[plan[i].0].dn) } else { sp_tee };
                plan[i].2 - plan[i - 1].2 >= end - 1e-9
            });
            if !ok {
                continue;
            }
            let to_xy = |a: f64, b: f64| if swap { cl.f.p(b, a) } else { cl.f.p(a, b) };
            // rises from the stack: final leg, collector, branches
            let leg0 = line - s0;
            let sb = |dn: u32| n.slope(dn) / 100.0;
            let run_b = |q: &(usize, (f64, f64), f64)| (q.1 .1 - line).abs() + (q.2 - q.1 .0).abs();
            let rise_max = plan.iter().map(|q| sc * (leg0 + (q.2 - a_s)) + sb(feet[q.0].dn) * run_b(q)).fold(0.0, f64::max);
            let z0 = z_max - rise_max;
            if z0 < z_min {
                continue;
            }
            let zc = |a: f64| z0 + sc * (leg0 + (a - a_s));
            let far = plan.last().unwrap().2;
            let (pts, zs) = if straight {
                (vec![(far, line), (a_s, s0)], vec![zc(far), z0])
            } else {
                let mut pts = vec![(far, line), (a_c, line), (a_c, s0)];
                let mut zs = vec![zc(far), z0 + sc * (leg0 + ext), z0 + sc * ext];
                if ext > 0.0 {
                    pts.push((a_s, s0));
                    zs.push(z0);
                }
                (pts, zs)
            };
            let stack = cl.stack();
            let mut cpts: Vec<V3> = pts
                .iter()
                .zip(zs)
                .map(|(q, z)| {
                    let xy = to_xy(q.0, q.1);
                    v3(xy.0, xy.1, z)
                })
                .collect();
            let last = cpts.len() - 1;
            cpts[last] = v3(stack.0, stack.1, z0);
            let mut runs = vec![svc::pipe_run(SvcSys::Sanitary, k, pvc, dn_c, cpts)];
            let mut links = vec![];
            for q in &plan {
                let f = &feet[q.0];
                let ((a, b), x) = (q.1, q.2);
                let zj = zc(x);
                let s = sb(f.dn);
                let pz = f.c.at.2 + e;
                let foot = to_xy(a, b);
                let mut pts = vec![v3(f.c.at.0, f.c.at.1, pz)];
                let stub = (foot.0 - f.c.at.0).hypot(foot.1 - f.c.at.1);
                if stub > 1e-9 {
                    // the outlet into the wall falls to the drop like any branch
                    pts.push(v3(foot.0, foot.1, pz - s * stub));
                }
                if (b - line).abs() > 1e-9 {
                    pts.push(v3(foot.0, foot.1, zj + s * run_b(q)));
                    if (x - a).abs() > 1e-9 {
                        let j = to_xy(x, b);
                        pts.push(v3(j.0, j.1, zj + s * (b - line).abs()));
                    }
                }
                let join = to_xy(x, line);
                pts.push(v3(join.0, join.1, zj));
                runs.push(svc::pipe_run(SvcSys::Sanitary, k, pvc, f.dn, pts));
                links.push((f.fx, f.c.name));
            }
            if space.try_add(&runs, gap) {
                let du: f64 = feet.iter().map(|f| f.du).sum();
                return Some((runs, z0, links, du, feet.iter().any(|f| f.wc)));
            }
        }
    }
    None
}

/// Plan box of the cluster fits on every storey it passes: inside a room, clear of the
/// furniture and fittings there and of the openings beside it.
fn cluster_clear(p: &GProject, cl: &Cluster, risers_to: usize) -> bool {
    let (door_clear, door_depth) = (rule("services.water.door_clear"), rule("services.water.door_depth"));
    for (k, st) in p.storeys.iter().enumerate() {
        let b = cl.bbox(k <= risers_to);
        let rooms = svc::rooms(st);
        let Some(room) = rooms.iter().find(|r| {
            b[0] >= r.clear[0] - 1e-6 && b[1] >= r.clear[1] - 1e-6 && b[2] <= r.clear[2] + 1e-6 && b[3] <= r.clear[3] + 1e-6
        }) else {
            return false;
        };
        for f in &st.layout.fixtures {
            if matches!(f.family, "Supply Diffuser" | "Exhaust Grille") {
                continue;
            }
            if let Some(fp) = svc::footprint(f) {
                if svc::hit(&fp, &b, 0.0) {
                    return false;
                }
            }
        }
        for sd in &room.sides {
            for g in &sd.gaps {
                let (p0, p1) = (sd.at(g.s0, 0.0), sd.at(g.s1, 0.0));
                let gb = svc::grow(&svc::bbox(&[p0, p1, sd.at(g.s0, door_depth), sd.at(g.s1, door_depth)]), door_clear);
                if g.door && svc::hit(&gb, &b, 0.0) {
                    return false;
                }
            }
        }
    }
    true
}

fn vertical(sys: SvcSys, k: usize, m: PipeMaterial, dn: u32, xy: (f64, f64), z0: f64, z1: f64) -> GRun {
    svc::pipe_run(sys, k, m, dn, vec![v3(xy.0, xy.1, z0), v3(xy.0, xy.1, z1)])
}

/// The provisional verticals of a cluster, for the clash search while rooms join it.
fn verticals(cl: &Cluster, top_z: (f64, f64)) -> Vec<GRun> {
    let mut c = vec![vertical(SvcSys::Sanitary, 0, PipeMaterial::Pvc, cl.stack_dn, cl.stack(), under_drain() - rule("services.water.stack_drop"), cl.vent)];
    let sec = |r: f64| {
        let mut s = ddc_geom::mep::Section::new(ddc_geom::mep::Domain::Pipe, ddc_geom::mep::Shape::Round { d: 2.0 * r });
        s.wall = 0.0;
        s
    };
    for (sys, xy, r, z0, z1) in [
        (SvcSys::Cold, cl.cw_xy(), cl.rc, under_cw(), top_z.0),
        (SvcSys::Hot, cl.hw_xy(), cl.rh, under_hw(), top_z.1),
    ] {
        c.push(GRun {
            sys,
            storey: 0,
            points: vec![(xy.0, xy.1, z0), (xy.0, xy.1, z1)],
            section: sec(r),
            pipe: None,
            bend: None,
            caps: (false, false),
        });
    }
    c
}

/// Flows of the wet rooms of every storey whose clear rectangle holds a plan point.
fn flows_at(p: &GProject, rooms: &[Vec<(Room, Vec<usize>)>], q: (f64, f64)) -> (f64, f64, bool) {
    let (mut cw, mut hw, mut wc) = (0.0, 0.0, false);
    for (k, list) in rooms.iter().enumerate() {
        for (r, fx) in list {
            if !svc::inside(q, &r.clear) {
                continue;
            }
            for &i in fx {
                let f = &p.storeys[k].layout.fixtures[i];
                wc |= f.family == "WC";
                for c in svc::svc_ports(f) {
                    match c.sys {
                        SvcSys::Cold => cw += c.flow,
                        SvcSys::Hot => hw += c.flow,
                        _ => {}
                    }
                }
            }
        }
    }
    (cw, hw, wc)
}

/// Designs the water services into the project. Returns the number of connectors left
/// unserved.
pub(crate) fn plumb(spec: &ProjectSpec, p: &mut GProject) -> usize {
    let n_st = p.storeys.len();
    let rooms: Vec<Vec<(Room, Vec<usize>)>> = (0..n_st).map(|k| wet_rooms(p, k)).collect();
    // water connectors of fixtures that stand in no room get no service; they are counted
    let water_ports = |k: usize, i: usize| svc::svc_ports(&p.storeys[k].layout.fixtures[i]).iter().filter(|c| c.sys != SvcSys::Power).count();
    let all_ports: usize = (0..n_st).map(|k| (0..p.storeys[k].layout.fixtures.len()).map(|i| water_ports(k, i)).sum::<usize>()).sum();
    let placed: usize = (0..n_st).map(|k| rooms[k].iter().map(|(_, fx)| fx.iter().map(|&i| water_ports(k, i)).sum::<usize>()).sum::<usize>()).sum();
    let unplaced = all_ports - placed;
    if rooms.iter().all(|r| r.is_empty()) {
        if unplaced > 0 {
            p.metrics.push(("water_ports_unplaced", unplaced as f64));
            p.metrics.push(("water_unserved", unplaced as f64));
        }
        return unplaced;
    }
    let riser_shafts: Vec<Vec<Vec<(f64, f64)>>> = p
        .storeys
        .iter()
        .map(|st| st.layout.shafts.iter().filter(|s| s.kind == crate::ShaftKind::Riser).map(|s| s.ring.clone()).collect())
        .collect();
    let mut space = Space::new(p);
    let mut big = (0.0f64, 0.0f64);
    for (k, list) in rooms.iter().enumerate() {
        for c in list.iter().flat_map(|r| r.1.iter()).flat_map(|&i| svc::svc_ports(&p.storeys[k].layout.fixtures[i])) {
            match c.sys {
                SvcSys::Cold => big.0 = big.0.max(c.flow),
                SvcSys::Hot => big.1 = big.1.max(c.flow),
                _ => {}
            }
        }
    }
    let pk = Peak { use_: use_of(spec.typology), max: big };
    // the provisional risers reach the highest chases a room may take
    let wide = rule("services.water.riser_wide");
    let z_max = |sys: SvcSys, z0: f64| {
        (0..n_st)
            .flat_map(|k| rooms[k].iter().map(move |r| (k, r)))
            .filter_map(|(k, (_, fx))| high_chase(&p.storeys[k], fx, wide, wide, sys))
            .fold(z0 + rule("services.water.chase_shift"), f64::max)
    };
    let (cz_max, hz_max) = (z_max(SvcSys::Cold, z_cw()), z_max(SvcSys::Cold, z_hw()).max(z_max(SvcSys::Hot, z_hw())) + 2.0 * wide + svc::gap());
    let mut clusters: Vec<Cluster> = vec![];
    let mut runs: Vec<GRun> = vec![];
    let mut links: Vec<GSvcLink> = vec![];
    let mut unserved = 0usize;
    let mut lu = 0.0;
    for k in (0..n_st).rev() {
        let e = p.storeys[k].elevation;
        for (room, fx) in &rooms[k] {
            let total: usize = fx.iter().map(|&i| svc::svc_ports(&p.storeys[k].layout.fixtures[i]).iter().filter(|c| c.sys != SvcSys::Power).count()).sum();
            for &i in fx {
                lu += norms().lu(p.storeys[k].layout.fixtures[i].family);
            }
            // the clusters this room can join, then a new one in each corner
            // a new stack goes into the corner nearest a riser shaft, when the storey has one; a
            // floor WC outlet too near the stack's line to branch takes the stack in line with it
            let corners: Vec<(f64, f64)> = (0..4).map(|j| corner(room, j).o).collect();
            let order = shaft_order(&corners, &riser_shafts[k]);
            let wc_outs: Vec<(f64, f64)> = fx
                .iter()
                .filter(|&&i| p.storeys[k].layout.fixtures[i].family == "WC")
                .flat_map(|&i| svc::svc_ports(&p.storeys[k].layout.fixtures[i]))
                .filter(|c| c.sys == SvcSys::Sanitary && c.dir.2 < -0.5)
                .map(|c| (c.at.0, c.at.1))
                .collect();
            let mut cands: Vec<(Option<usize>, usize, Option<(usize, bool)>)> = (0..clusters.len())
                .filter(|&c| (0..4).any(|j| clusters[c].f.same(&corner(room, j))))
                .map(|c| (Some(c), 0, None))
                .collect();
            for j in 0..4 {
                cands.push((None, order[j], None));
                for w in 0..wc_outs.len() {
                    cands.extend([(None, order[j], Some((w, true))), (None, order[j], Some((w, false)))]);
                }
            }
            let stack_shift = rule("services.water.stack_shift");
            let mut best: Option<(usize, RoomDesign, Option<Cluster>, usize)> = None;
            for (cand, cj, align) in cands {
                let m = space.mark();
                let mut fresh: Option<Cluster> = None;
                let cl: &Cluster = match cand {
                    Some(c) => &clusters[c],
                    None => {
                        let f = corner(room, cj);
                        let probe = Cluster::new(f, false, 0.0, 0.0);
                        let (cw, hw, wc) = flows_at(p, &rooms, probe.stack());
                        let mut c = Cluster::new(f, wc, cw, hw);
                        if let Some((w, along_u)) = align {
                            let q = c.f.uv(wc_outs[w]);
                            let s = if along_u { (q.0, c.s.1) } else { (c.s.0, q.1) };
                            let off = if along_u { s.0 - c.s.0 } else { s.1 - c.s.1 };
                            if off <= 1e-9 || off > stack_shift {
                                continue;
                            }
                            c.set_stack(s);
                        }
                        c.vent = roof_top(p, c.stack()) + norms().vent_above_roof();
                        if !cluster_clear(p, &c, k) || !space.try_add(&verticals(&c, (e + cz_max, e + hz_max)), svc::gap()) {
                            space.rollback(m);
                            continue;
                        }
                        fresh = Some(c);
                        fresh.as_ref().unwrap()
                    }
                };
                let d = design_room(&mut space, p, k, room, fx, cl, &pk);
                let score = d.served;
                let keep = best.as_ref().is_none_or(|b| score > b.3);
                space.rollback(m);
                if keep {
                    best = Some((cand.unwrap_or(usize::MAX), d, fresh, score));
                }
                if score == total {
                    break;
                }
            }
            let Some((ci, _, fresh, _)) = best else {
                unserved += total;
                continue;
            };
            // redo the chosen design for real
            let ci = match fresh {
                Some(c) => {
                    space.try_add(&verticals(&c, (e + cz_max, e + hz_max)), svc::gap());
                    clusters.push(c);
                    clusters.len() - 1
                }
                None => ci,
            };
            let d = design_room(&mut space, p, k, room, fx, &clusters[ci], &pk);
            unserved += total - d.served;
            if d.served == 0 {
                continue;
            }
            clusters[ci].joins.push(Join {
                k,
                cw: d.cw,
                hw: d.hw,
                du: d.du,
                wc: d.wc,
                stack_z: d.stack_z,
                cz: d.cz,
                hz: d.hz,
            });
            runs.extend(d.runs);
            for (fx, c, sys) in d.links {
                links.push(GSvcLink { storey: k, fixture: fx, connector: c, sys });
            }
        }
    }
    let used: Vec<&Cluster> = clusters.iter().filter(|c| !c.joins.is_empty()).collect();
    // the provisional verticals of every tried cluster leave with a fresh space
    drop(space);
    let mut space = Space::new(p);
    for r in &runs {
        space.add(r);
    }
    if !used.is_empty() {
        let kf = k_factor(spec.typology);
        risers_and_mains(p, &mut space, &used, kf, &pk, &mut runs);
    }
    into_shaft(p, &mut space, &mut runs);
    // slab holes where a run crosses a slab upright
    let mut holes: Vec<(usize, R)> = vec![];
    for r in &runs {
        let d = r.section.envelope().wh().0;
        for w in r.points.windows(2) {
            if (w[0].0 - w[1].0).abs() > 1e-9 || (w[0].1 - w[1].1).abs() > 1e-9 {
                continue;
            }
            let (lo, hi) = (w[0].2.min(w[1].2), w[0].2.max(w[1].2));
            // a core shaft cuts the slabs itself
            if p.storeys[0].layout.shafts.iter().any(|sh| super::hvac::in_rect(&sh.ring, (w[0].0, w[0].1))) {
                continue;
            }
            for (k, st) in p.storeys.iter().enumerate() {
                if hi > st.elevation - svc::slab() && lo < st.elevation {
                    holes.push((k, svc::hole(w[0].0, w[0].1, d)));
                }
            }
        }
    }
    drop(space);
    svc::add_holes(p, &holes);
    p.services.water = runs;
    p.services.links.extend(links);
    p.services.water_use = pk.use_.to_string();
    p.metrics.push(("water_loading_units", lu));
    unserved += unplaced;
    p.metrics.push(("water_ports_unplaced", unplaced as f64));
    p.metrics.push(("water_unserved", unserved as f64));
    unserved
}

/// Height above the floor of a hot chase over every supply connector of a room, when every hot
/// one stands clear above the cold chase so the hot drops pass it.
fn high_chase(st: &super::PStorey, fx: &[usize], rc: f64, rh: f64, high: SvcSys) -> Option<f64> {
    let ports: Vec<SvcPort> = fx.iter().flat_map(|&i| svc::svc_ports(&st.layout.fixtures[i])).collect();
    let pass = rc + rh + svc::gap();
    let other = if high == SvcSys::Hot { z_cw() } else { z_hw() };
    let top = ports.iter().filter(|c| matches!(c.sys, SvcSys::Cold | SvcSys::Hot)).map(|c| c.at.2).fold(f64::MIN, f64::max);
    let low = ports.iter().filter(|c| c.sys == high).map(|c| c.at.2).fold(f64::MAX, f64::min);
    let bend = svc::bend_of(PipeMaterial::Pp, svc::supply_pipe(0.0).1);
    // the drop to the top connector holds its two bends
    (low < f64::MAX && low - bend > other + pass).then_some(top + pass.max(2.0 * bend) + svc::gap())
}

fn design_room(space: &mut Space<'_>, p: &GProject, k: usize, room: &Room, fx: &[usize], cl: &Cluster, pk: &Peak) -> RoomDesign {
    let total: usize = fx.iter().map(|&i| svc::svc_ports(&p.storeys[k].layout.fixtures[i]).iter().filter(|c| c.sys != SvcSys::Power).count()).sum();
    let m = space.mark();
    let d = design_room_in(space, p, k, room, fx, cl, pk, false);
    if d.served >= total {
        return d;
    }
    // the drains first, the chases then bridging their drops in the wall
    space.rollback(m);
    let d2 = design_room_in(space, p, k, room, fx, cl, pk, true);
    if d2.served > d.served {
        return d2;
    }
    space.rollback(m);
    design_room_in(space, p, k, room, fx, cl, pk, false)
}

#[allow(clippy::too_many_arguments)]
fn design_room_in(space: &mut Space<'_>, p: &GProject, k: usize, room: &Room, fx: &[usize], cl: &Cluster, pk: &Peak, drains_first: bool) -> RoomDesign {
    let st = &p.storeys[k];
    let mut d = RoomDesign { cz: z_cw(), hz: z_hw(), ..Default::default() };
    let (mut qc, mut qh) = (0.0, 0.0);
    for &i in fx {
        for c in svc::svc_ports(&st.layout.fixtures[i]) {
            match c.sys {
                SvcSys::Cold => qc += c.flow,
                SvcSys::Hot => qh += c.flow,
                _ => {}
            }
        }
    }
    let q_min = rule("services.water.flow_min");
    let (mc, dc) = svc::supply_pipe(pk.q(SvcSys::Cold, qc).max(q_min));
    let (mh, dh) = svc::supply_pipe(pk.q(SvcSys::Hot, qh).max(q_min));
    let (rc, rh) = (svc::od(mc, dc) / 2.0, svc::od(mh, dh) / 2.0);
    let chase_gap = rule("services.water.chase_gap");
    let c1 = (rc + chase_gap).max(rule("services.water.chase_min"));
    let c2 = c1 + rc + rh + chase_gap;
    // back to back rooms share a chase wall: the order, depth and height of the two chases
    // are tried until they pass the neighbour's runs
    let dz = rule("services.water.chase_shift");

    let mut ways = vec![(c1, c2, z_cw(), z_hw()), (c2, c1, z_cw(), z_hw()), (c1, c2, z_cw() + dz, z_hw() + dz), (c2, c1, z_cw() + dz, z_hw() + dz)];
    if let Some(hi) = high_chase(st, fx, rc, rh, SvcSys::Hot) {
        ways.push((c1, c1, z_cw(), hi));
    }
    // back to back with the neighbour's taps in a thin wall: the cold chase passes over them
    if let Some(hi) = high_chase(st, fx, rc, rh, SvcSys::Cold) {
        ways.push((c1, c1, hi, z_hw()));
    }
    if drains_first {
        add_drains(space, p, k, room, fx, cl, &mut d);
    }
    // last, the chases bridge the rises of the room behind a shared wall and the drops
    let none: Vec<Upright> = vec![];
    let ups = space.uprights();
    let mut over = ways.clone();
    // or both chases run over the rises ending in the walls, under the ceiling, and drop to the taps
    let (e, ceil) = (st.elevation, svc::ceiling(p, k));
    let reach = room.sides.iter().map(|sd| 2.0 * sd.half).fold(0.0, f64::max) + svc::gap();
    let top = ups
        .iter()
        .filter(|u| u.1 < e + ceil && u.2 > e && u.2 < e + ceil && svc::inside(u.0, &svc::grow(&room.clear, reach)))
        .map(|u| u.2 - e)
        .fold(f64::MIN, f64::max);
    if top > f64::MIN {
        let zc = top + rule("services.water.bridge_clear") + rc;
        let zh = zc + rc + rh + chase_gap;
        if zh + rh + svc::gap() <= ceil {
            over.extend([(c1, c2, zc, zh), (c2, c1, zc, zh)]);
        }
    }
    let tries: Vec<_> = ways.iter().map(|w| (*w, &none)).chain(over.iter().map(|w| (*w, &ups))).collect();
    let want = fx.iter().flat_map(|&i| svc::svc_ports(&st.layout.fixtures[i])).filter(|c| matches!(c.sys, SvcSys::Cold | SvcSys::Hot)).count();
    // the first way serving every tap, else the one serving most
    let mut part: Option<(usize, usize)> = None;
    let mut take: Option<usize> = None;
    for (i, ((dc, dh, zc, zh), ups)) in tries.iter().enumerate() {
        let (cw_runs, cw_links, _, _) = supply(p, k, room, fx, cl, SvcSys::Cold, *dc, rc, *zc, pk, ups);
        let (hw_runs, hw_links, _, _) = supply(p, k, room, fx, cl, SvcSys::Hot, *dh, rh, *zh, pk, ups);
        let mut sup: Vec<GRun> = cw_runs;
        sup.extend(hw_runs);
        if sup.is_empty() {
            break;
        }
        let n = cw_links.len() + hw_links.len();
        let m = space.mark();
        let ok = space.try_add(&sup, svc::gap());
        space.rollback(m);
        if !ok {
            continue;
        }
        if n == want {
            take = Some(i);
            break;
        }
        if part.is_none_or(|p| n > p.1) {
            part = Some((i, n));
        }
    }
    if let Some(i) = take.or(part.map(|p| p.0)) {
        let ((dc, dh, zc, zh), ups) = tries[i];
        let (cw_runs, cw_links, cw_q, _) = supply(p, k, room, fx, cl, SvcSys::Cold, dc, rc, zc, pk, ups);
        let (hw_runs, hw_links, hw_q, _) = supply(p, k, room, fx, cl, SvcSys::Hot, dh, rh, zh, pk, ups);
        let mut sup: Vec<GRun> = cw_runs;
        sup.extend(hw_runs);
        space.try_add(&sup, svc::gap());
        d.runs.extend(sup);
        d.cw = cw_q;
        d.hw = hw_q;
        d.cz = zc;
        d.hz = zh;
        d.links.extend(cw_links.into_iter().map(|(f, c)| (f, c, SvcSys::Cold)));
        d.links.extend(hw_links.into_iter().map(|(f, c)| (f, c, SvcSys::Hot)));
    }
    if !drains_first {
        add_drains(space, p, k, room, fx, cl, &mut d);
    }
    d.served = d.links.len();
    d
}

fn add_drains(space: &mut Space<'_>, p: &GProject, k: usize, room: &Room, fx: &[usize], cl: &Cluster, d: &mut RoomDesign) {
    let dr = drains(space, p, k, room, fx, cl);
    if let Some((runs, z0, links, du, wc)) = dr {
        d.runs.extend(runs);
        d.stack_z = Some(z0);
        d.du = du;
        d.wc = wc;
        d.links.extend(links.into_iter().map(|(f, c)| (f, c, SvcSys::Sanitary)));
    }
}

/// The verticals of every cluster sized storey by storey, and under the ground slab the cold
/// and hot mains to a capped inlet and the building drain to a capped outlet.
fn risers_and_mains(p: &GProject, space: &mut Space<'_>, used: &[&Cluster], kf: f64, pk: &Peak, runs: &mut Vec<GRun>) {
    let mut cw_feet = vec![];
    let mut hw_feet = vec![];
    let mut drain_feet = vec![];
    let n0 = runs.len();
    for cl in used {
        let e = |k: usize| p.storeys[k].elevation;
        let mut js = cl.joins.clone();
        js.sort_by_key(|j| j.k);
        // cold and hot risers: segments between the storeys they serve, sized by the flow above
        for (sys, xy, zrel, flow) in [
            (SvcSys::Cold, cl.cw_xy(), &(|j: &Join| j.cz) as &dyn Fn(&Join) -> f64, &(|j: &Join| j.cw) as &dyn Fn(&Join) -> f64),
            (SvcSys::Hot, cl.hw_xy(), &(|j: &Join| j.hz) as &dyn Fn(&Join) -> f64, &(|j: &Join| j.hw) as &dyn Fn(&Join) -> f64),
        ] {
            let served: Vec<&Join> = js.iter().filter(|j| flow(j) > 0.0).collect();
            if served.is_empty() {
                continue;
            }
            let bottom = if sys == SvcSys::Cold { under_cw() } else { under_hw() };
            let mut z0 = bottom;
            for (i, j) in served.iter().enumerate() {
                let q: f64 = served[i..].iter().map(|x| flow(x)).sum();
                let (m, dn) = svc::supply_pipe(pk.q(sys, q));
                let z1 = e(j.k) + zrel(j);
                runs.push(vertical(sys, j.k, m, dn, xy, z0, z1));
                z0 = z1;
            }
            let q: f64 = served.iter().map(|x| flow(x)).sum();
            if sys == SvcSys::Cold {
                cw_feet.push((xy, q, 0.0, cl.f.b, cl.row(), None));
            } else {
                hw_feet.push((xy, q, 0.0, cl.f.b, cl.row(), None));
            }
        }
        // the stack: from the building drain up through every join to the vent
        let joins: Vec<(usize, f64, f64)> = js.iter().filter_map(|j| j.stack_z.map(|z| (j.k, z, j.du))).collect();
        if joins.is_empty() {
            continue;
        }
        let du: f64 = joins.iter().map(|j| j.2).sum();
        let qww = kf * du.sqrt();
        let mut zs: Vec<f64> = joins.iter().map(|j| j.1).collect();
        zs.push(cl.vent);
        drain_feet.push((cl.stack(), qww, du, cl.f.b, cl.row(), Some((PipeMaterial::Pvc, cl.stack_dn))));
        let xy = cl.stack();
        let mut segs = vec![];
        let mut z = joins[0].1;
        for zz in zs.iter().skip(1) {
            segs.push((z, *zz));
            z = *zz;
        }
        for (i, (a, b)) in segs.into_iter().enumerate() {
            let mut r = vertical(SvcSys::Sanitary, joins[i.min(joins.len() - 1)].0, PipeMaterial::Pvc, cl.stack_dn, xy, a, b);
            if (b - cl.vent).abs() < 1e-9 {
                r.caps.1 = true;
            }
            runs.push(r);
        }
    }
    for r in &runs[n0..] {
        space.add(r);
    }
    // each stack reaches below its lowest join to the building drain, still to be laid
    for cl in used {
        if let Some(top) = cl.joins.iter().filter_map(|j| j.stack_z).reduce(f64::min) {
            let z0 = under_drain() - rule("services.water.stack_drop");
            space.add(&vertical(SvcSys::Sanitary, 0, PipeMaterial::Pvc, cl.stack_dn, cl.stack(), z0, top));
        }
    }
    let foot = svc::bbox(&p.storeys[0].layout.slab);
    for (sys, feet, z) in [
        (SvcSys::Cold, &cw_feet, under_cw()),
        (SvcSys::Hot, &hw_feet, under_hw()),
    ] {
        if let Some(r) = comb(space, sys, feet, z, None, &|q| pk.q(sys, q), &foot) {
            runs.extend(r);
        }
    }
    // stacks start at the lowest join; the building drain hangs below
    let tops: Vec<f64> = used
        .iter()
        .filter_map(|c| c.joins.iter().filter_map(|j| j.stack_z).reduce(f64::min))
        .collect();
    if let Some(r) = comb(space, SvcSys::Sanitary, &drain_feet, under_drain(), Some(kf), &|q| q, &foot) {
        // join each stack bottom to its branch: the branch starts at (x, y, z_top); extend the
        // stack down from its lowest join to there
        for (cl, top) in used.iter().filter(|c| c.joins.iter().any(|j| j.stack_z.is_some())).zip(&tops) {
            let xy = cl.stack();
            if let Some(b) = r.iter().find(|b| (b.points[0].0 - xy.0).abs() < 1e-9 && (b.points[0].1 - xy.1).abs() < 1e-9) {
                let z0 = b.points[0].2;
                runs.push(vertical(SvcSys::Sanitary, 0, PipeMaterial::Pvc, cl.stack_dn, xy, z0, *top));
            }
        }
        runs.extend(r);
    }
}

/// Moves the long uprights of the wet-room stacks into the core riser shaft: each keeps its
/// ends where the room's branches meet it and reaches the shaft by a horizontal leg at each
/// end, the drain's legs falling towards the stack's foot. A stack's uprights share one place
/// in the shaft, tried nearest first until its legs and uprights clear everything built; one
/// with no such place, or standing too near the shaft for the legs, stays upright.
fn into_shaft(p: &GProject, space: &mut Space<'_>, runs: &mut [GRun]) {
    let bays = super::hvac::core_bays(p, crate::ShaftKind::Riser);
    if bays.is_empty() {
        return;
    }
    let (pitch, leg, fall) = (rule("services.water.shaft_pitch"), rule("services.water.shaft_leg"), rule("services.water.shaft_fall"));
    let cable = super::hvac::core_bays(p, crate::ShaftKind::Electrical);
    // the places in each bay; a bay shared with the cable tray keeps the tray's middle clear
    let mut slots: Vec<Vec<(f64, f64)>> = bays
        .iter()
        .map(|r| {
            let (nx, ny) = (((r.2 - r.0) / pitch).floor() as usize, ((r.3 - r.1) / pitch).floor() as usize);
            let (ox, oy) = (r.0 + ((r.2 - r.0) - nx as f64 * pitch) / 2.0 + pitch / 2.0, r.1 + ((r.3 - r.1) - ny as f64 * pitch) / 2.0 + pitch / 2.0);
            let mid = ((r.0 + r.2) / 2.0, (r.1 + r.3) / 2.0);
            let shared = cable.contains(r);
            (0..nx * ny)
                .map(|i| (ox + (i % nx) as f64 * pitch, oy + (i / nx) as f64 * pitch))
                .filter(|q| !shared || (q.0 - mid.0).hypot(q.1 - mid.1) > 2.0 * pitch)
                .collect()
        })
        .collect();
    let mut groups: Vec<((i64, i64, SvcSys), Vec<usize>)> = vec![];
    for (i, r) in runs.iter().enumerate() {
        if !matches!(r.sys, SvcSys::Cold | SvcSys::Hot | SvcSys::Sanitary) || r.points.len() != 2 {
            continue;
        }
        let (a, b) = (r.points[0], r.points[1]);
        if (a.0 - b.0).abs() > 1e-9 || (a.1 - b.1).abs() > 1e-9 || (a.2 - b.2).abs() <= 1.0 {
            continue;
        }
        if bays.iter().any(|q| super::hvac::in_rect_r(q, (a.0, a.1))) {
            continue;
        }
        let key = ((a.0 * 1000.0).round() as i64, (a.1 * 1000.0).round() as i64, r.sys);
        match groups.iter_mut().find(|g| g.0 == key) {
            Some(g) => g.1.push(i),
            None => groups.push((key, vec![i])),
        }
    }
    for (_, idx) in groups {
        let xy = (runs[idx[0]].points[0].0, runs[idx[0]].points[0].1);
        let mut cands: Vec<(usize, usize, f64)> = vec![];
        for (b, list) in slots.iter().enumerate() {
            for (j, q) in list.iter().enumerate() {
                cands.push((b, j, (q.0 - xy.0).hypot(q.1 - xy.1)));
            }
        }
        cands.sort_by(|x, y| x.2.total_cmp(&y.2));
        for (b, j, d) in cands.into_iter().filter(|c| c.2 >= leg).take(rule("services.water.shaft_candidates") as usize) {
            let at = slots[b][j];
            let drop = |r: &GRun| if r.sys == SvcSys::Sanitary { fall * d } else { 0.0 };
            let moved: Vec<GRun> = idx
                .iter()
                .map(|&i| {
                    let r = &runs[i];
                    let (a, c) = (r.points[0], r.points[1]);
                    let (lo, hi, dr) = (a.2.min(c.2), a.2.max(c.2), drop(r));
                    let pts = vec![(xy.0, xy.1, lo), (at.0, at.1, lo + dr), (at.0, at.1, hi - dr), (xy.0, xy.1, hi)];
                    let mut m = r.clone();
                    m.points = if a.2 <= c.2 { pts } else { pts.into_iter().rev().collect() };
                    m
                })
                .collect();
            if space.try_add(&moved, svc::gap()) {
                for (&i, m) in idx.iter().zip(moved) {
                    runs[i] = m;
                }
                slots[b].swap_remove(j);
                break;
            }
        }
    }
}

/// A foot of a vertical on the mains under the ground slab: plan point, flow, discharge units,
/// the way into the room from its cluster, the cluster's widest vertical radius and the pipe
/// of the vertical when it is not the branch's own.
type Feed = ((f64, f64), f64, f64, (f64, f64), f64, Option<(PipeMaterial, u32)>);

/// Under the ground slab: a main along one plan axis at a line chosen clear of everything,
/// a branch square to it from each foot, the main on to a capped end beyond the outline.
/// `fall` (the K factor) makes it a drain falling to the outlet and sizes it by discharge units.
fn comb(
    space: &mut Space<'_>,
    sys: SvcSys,
    feet: &[Feed],
    z: f64,
    fall: Option<f64>,
    peak: &dyn Fn(f64) -> f64,
    foot: &R,
) -> Option<Vec<GRun>> {
    if feet.is_empty() {
        return None;
    }
    let drain = fall.is_some();
    let total_q: f64 = feet.iter().map(|f| f.1).sum();
    let total_du: f64 = feet.iter().map(|f| f.2).sum();
    let (mm, dm) = match fall {
        Some(kf) => (PipeMaterial::Pvc, svc::drain_dn(kf * total_du.sqrt(), drain_dn_min())),
        None => svc::supply_pipe(peak(total_q)),
    };
    let om = svc::od(mm, dm);
    let (gap, join_gap) = (svc::gap(), rule("services.water.main_join_gap"));
    let out_len = out_len();
    for along_x in [true, false] {
        let (lo, hi) = if along_x { (foot[1], foot[3]) } else { (foot[0], foot[2]) };
        let mut line = lo - 2.0;
        while line < hi + 2.0 {
            // under the slab a jog may as well step out of the room, when into it meets a vertical
            let mut flip = vec![1.0; feet.len()];
            for _ in 0..3 {
            // (join on the main, foot index, jog off the foot)
            let mut joins: Vec<(f64, usize, f64)> = vec![];
            // per foot: the shortest step along the main off the foot and the nearest the foot
            // may stand to the main when it steps
            let mut steps: Vec<(f64, f64)> = vec![];
            let mut jog0: Vec<f64> = vec![];
            let mut ok = true;
            for (i, f) in feet.iter().enumerate() {
                let (a, b) = if along_x { (f.0 .0, f.0 .1) } else { (f.0 .1, f.0 .0) };
                let (m, dn) = branch_pipe(f, fall, peak);
                // the turn off the vertical: an elbow, and a reducer when the sizes differ
                let bend = svc::bend_of(m, dn);
                let off = match f.5 {
                    Some((vm, vdn)) if (vm, vdn) != (m, dn) => {
                        let s = |m, dn| svc::pipe_run(sys, 0, m, dn, vec![]).section;
                        bend.max(svc::bend_of(vm, vdn)) + mep::transition_length(&s(m, dn), &s(vm, vdn))
                    }
                    _ => bend,
                };
                // a branch square to the main along the cluster's row would run through the
                // other verticals there: it first steps into the room, clear of the row
                let into_a = if along_x { f.3 .0 } else { f.3 .1 };
                let jog_in = if into_a.abs() > 0.5 { (off + bend + gap).max(f.4 + svc::od(m, dn) / 2.0 + gap) } else { 0.0 };
                // a leg that would run through another foot's stack steps round it along the
                // main, either way: there are no walls under the slab
                let hits = |a0: f64, a1: f64, b0: f64, b1: f64| {
                    feet.iter().enumerate().any(|(j, g)| {
                        let (ga, gb) = if along_x { (g.0 .0, g.0 .1) } else { (g.0 .1, g.0 .0) };
                        let r = svc::od(m, dn) / 2.0 + g.5.map_or(0.0, |(gm, gdn)| svc::od(gm, gdn) / 2.0) + gap;
                        j != i && ga > a0.min(a1) - r && ga < a0.max(a1) + r && gb > b0.min(b1) - r && gb < b0.max(b1) + r
                    })
                };
                let blocked = |s: f64, jog: f64| hits(a, a + s * jog, b, b) || hits(a + s * jog, a + s * jog, b, line);
                let pref = if into_a.abs() > 0.5 { into_a.signum() } else { 1.0 };
                let pref = flip[i] * pref;
                let (s, jog) = if !blocked(pref, jog_in) {
                    (pref, jog_in)
                } else {
                    let j_min = jog_in.max(off + bend + gap);
                    (0..30)
                        .map(|k| j_min + 0.1 * f64::from(k))
                        .flat_map(|jog| [(pref, jog), (-pref, jog)])
                        .find(|&(s, jog)| !blocked(s, jog))
                        .unwrap_or((pref, jog_in))
                };
                if (b - line).abs() < if jog > 0.0 { bend } else { off } + om + gap {
                    ok = false;
                    break;
                }
                joins.push((a + s * jog, i, jog));
                // the step into the room, not a step round a stack: the retry below reads it
                jog0.push(jog_in);
                steps.push(((off + bend + gap).max(f.4 + svc::od(m, dn) / 2.0 + gap), bend + om + gap));
            }
            joins.sort_by(|x, y| x.0.total_cmp(&y.0));
            // feet too close along the main for their tees: each later one steps on along the
            // main, beside its foot, to the spacing the tees need
            // an end join turns the main into its branch: the elbow and the reducer there take
            // more of the main than a tee does
            let end_trim = |i: usize| {
                let (m, dn) = branch_pipe(&feet[i], fall, peak);
                let s = |m, dn| svc::pipe_run(sys, 0, m, dn, vec![]).section;
                let red = if (m, dn) != (mm, dm) { mep::transition_length(&s(m, dn), &s(mm, dm)) } else { 0.0 };
                svc::bend_of(mm, dm).max(svc::bend_of(m, dn)) + red
            };
            let n_j = joins.len();
            let need_at = |joins: &[(f64, usize, f64)], n: usize| {
                let mut t = om;
                if n == 1 {
                    t = t.max(end_trim(joins[0].1));
                }
                if n + 1 == n_j {
                    t = t.max(end_trim(joins[n].1));
                }
                om + t + join_gap
            };
            if ok {
                for n in 1..joins.len() {
                    let want = joins[n - 1].0 + need_at(&joins, n);
                    if joins[n].0 >= want {
                        continue;
                    }
                    let (_, i, _) = joins[n];
                    let a_f = if along_x { feet[i].0 .0 } else { feet[i].0 .1 };
                    let b_f = if along_x { feet[i].0 .1 } else { feet[i].0 .0 };
                    if (b_f - line).abs() < steps[i].1 {
                        break;
                    }
                    // one whose step met a vertical steps back past the join before it
                    let a_new = if flip[i] < 0.0 && jog0[i] == 0.0 {
                        (joins[n - 1].0 - need_at(&joins, n)).min(a_f - steps[i].0)
                    } else {
                        want.max(a_f + steps[i].0)
                    };
                    joins[n] = (a_new, i, (a_new - a_f).abs());
                }
            }
            joins.sort_by(|x, y| x.0.total_cmp(&y.0));
            if ok && !(1..n_j).all(|n| joins[n].0 - joins[n - 1].0 >= need_at(&joins, n) - 1e-9) {
                ok = false;
            }
            if ok {
                // the outlet on the nearer end of the outline
                let (a0, a1) = if along_x { (foot[0], foot[2]) } else { (foot[1], foot[3]) };
                let first = joins[0].0;
                let last = joins[joins.len() - 1].0;
                // beyond the outline and beyond every join, so the outlet leg holds its fittings
                // when a vertical stands outside the ground slab
                let (out, far) = if first - a0 <= a1 - last { (a0.min(first) - out_len, last) } else { (a1.max(last) + out_len, first) };
                let at = |a: f64, b: f64| if along_x { (a, b) } else { (b, a) };
                let slope_m = if drain { norms().slope(dm) / 100.0 } else { 0.0 };
                // the outlet lowest: z at the join from the top of the highest branch
                let mut rise: f64 = 0.0;
                for (a, i, jog) in &joins {
                    let f = &feet[*i];
                    let (_, dn) = branch_pipe(f, fall, peak);
                    let b = if along_x { f.0 .1 } else { f.0 .0 };
                    let sb = if drain { norms().slope(dn) / 100.0 } else { 0.0 };
                    rise = rise.max(slope_m * (a - out).abs() + sb * ((b - line).abs() + jog));
                }
                let z_out = z - rise;
                let zm = |a: f64| z_out + slope_m * (a - out).abs();
                let p0 = at(far, line);
                let p1 = at(out, line);
                let mut main = svc::pipe_run(sys, 0, mm, dm, vec![v3(p0.0, p0.1, zm(far)), v3(p1.0, p1.1, z_out)]);
                main.caps.1 = true;
                let mut out_runs = vec![main];
                for (a, i, jog) in &joins {
                    let f = &feet[*i];
                    let (m, dn) = branch_pipe(f, fall, peak);
                    let b = if along_x { f.0 .1 } else { f.0 .0 };
                    let sb = if drain { norms().slope(dn) / 100.0 } else { 0.0 };
                    let z_jog = zm(*a) + sb * (b - line).abs();
                    let top = if drain { z_jog + sb * jog } else { z };
                    let j = at(*a, line);
                    let mut pts = vec![v3(f.0 .0, f.0 .1, top)];
                    if *jog > 0.0 {
                        let q = at(*a, b);
                        pts.push(v3(q.0, q.1, if drain { z_jog } else { z }));
                    }
                    pts.push(v3(j.0, j.1, zm(*a)));
                    out_runs.push(svc::pipe_run(sys, 0, m, dn, pts));
                }
                if space.try_add(&out_runs, gap) {
                    return Some(out_runs);
                }
                // a jog or a step meeting a vertical turns the other way
                let stuck: Vec<usize> = (0..n_j)
                    .filter(|&n| joins[n].2 > 0.0 && !space.fits(&out_runs[n + 1], gap))
                    .map(|n| joins[n].1)
                    .filter(|&i| flip[i] > 0.0)
                    .collect();
                if stuck.is_empty() {
                    break;
                }
                for i in stuck {
                    flip[i] = -1.0;
                }
            } else {
                break;
            }
            }
            line += 0.5;
        }
    }
    None
}

fn branch_pipe(f: &Feed, fall: Option<f64>, peak: &dyn Fn(f64) -> f64) -> (PipeMaterial, u32) {
    match fall {
        Some(kf) => (PipeMaterial::Pvc, svc::drain_dn(kf * f.2.sqrt(), drain_dn_min())),
        None => svc::supply_pipe(peak(f.1)),
    }
}

#[cfg(test)]
mod tests {
    use super::shaft_order;

    #[test]
    fn stack_corner_follows_the_riser_shaft() {
        let corners = [(0.0, 0.0), (4.0, 0.0), (4.0, 3.0), (0.0, 3.0)];
        // no shaft: the corners as they come
        assert_eq!(shaft_order(&corners, &[]), vec![0, 1, 2, 3]);
        // a riser shaft just outside the far corner: that corner first, then its neighbours
        let shaft = vec![(4.1, 3.1), (4.8, 3.1), (4.8, 3.8), (4.1, 3.8)];
        let o = shaft_order(&corners, &[shaft]);
        assert_eq!(o[0], 2);
        assert_eq!(o[3], 0);
        // two shafts: the nearer one wins
        let near = vec![(-0.6, -0.6), (-0.1, -0.6), (-0.1, -0.1), (-0.6, -0.1)];
        let far = vec![(6.0, 6.0), (7.0, 6.0), (7.0, 7.0), (6.0, 7.0)];
        assert_eq!(shaft_order(&corners, &[far, near])[0], 0);
    }
}
