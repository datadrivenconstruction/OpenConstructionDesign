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
}

impl Cluster {
    fn new(f: Frame, wc: bool, cw_q: f64, hw_q: f64) -> Cluster {
        let stack_dn = if wc { stack_dn_wc() } else { rule("services.water.stack_dn") as u32 };
        let rs = svc::od(PipeMaterial::Pvc, stack_dn) / 2.0;
        let q_min = rule("services.water.riser_flow_min");
        let (mc, dc) = svc::supply_pipe(cw_q.max(q_min));
        let (mh, dh) = svc::supply_pipe(hw_q.max(q_min));
        let (rc, rh) = (svc::od(mc, dc) / 2.0, svc::od(mh, dh) / 2.0);
        let g = g();
        let pad = rule("services.water.riser_pad");
        let x0 = g + 2.0 * rs + g;
        let cw = (x0 + rc, 2.0 * rc + pad);
        let hw = (cw.0 + rc + g + rh, 2.0 * rh + pad);
        Cluster {
            f,
            stack_dn,
            rs,
            cw,
            hw,
            rc,
            rh,
            joins: vec![],
            vent: 0.0,
        }
    }
    fn stack(&self) -> (f64, f64) {
        self.f.p(g() + self.rs, g() + self.rs)
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
            (self.hw.0 + self.rh + g, (self.cw.1 + self.rc).max(self.hw.1 + self.rh).max(2.0 * self.rs + g) + g)
        } else {
            (2.0 * self.rs + 2.0 * g, 2.0 * self.rs + 2.0 * g)
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

fn k_factor(t: Typology) -> f64 {
    let n = norms();
    match t {
        Typology::House | Typology::ApartmentSection => n.k("res"),
        Typology::Office => n.k("office"),
        Typology::School => n.k("public"),
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
) -> (Vec<GRun>, Vec<(usize, &'static str)>, f64, usize) {
    let st = &p.storeys[k];
    let e = st.elevation;
    let z = e + if sys == SvcSys::Cold { z_cw() } else { z_hw() };
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
    // blocked: openings low enough to cut the chase, walls too thin for it
    let mut blocked: Vec<(f64, f64)> = vec![];
    for (j, sd) in room.sides.iter().enumerate() {
        if 2.0 * sd.half < depth + main_r + thin_extra {
            blocked.push((ring.u(j, -depth) - thin_pad, ring.u(j, sd.len + depth) + thin_pad));
            continue;
        }
        for g in &sd.gaps {
            if g.sill < z - e + main_r + sill_margin {
                blocked.push((ring.u(j, g.s0) - gap_pad, ring.u(j, g.s1) + gap_pad));
            }
        }
    }
    let fwd = |u: f64| (u - u0).rem_euclid(per);
    let fwd_block = blocked.iter().map(|b| fwd(b.0)).fold(per, f64::min);
    let bwd_block = blocked.iter().map(|b| per - fwd(b.1)).fold(per, f64::min);
    if blocked.iter().any(|b| fwd(b.0) > fwd(b.1)) {
        // the branch lands in a blocked stretch
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
            if df < fwd_block && df > arm_min {
                o.push((df, 0, *a));
            }
            if db < bwd_block && db > arm_min {
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
        let end = kept.last().unwrap().0;
        let u1 = if dir == 0 { u0 + end } else { u0 - end };
        let pts: Vec<V3> = ring.path(u0, u1).into_iter().map(|w| v3(w.0, w.1, z)).collect();
        let (m, dn) = svc::supply_pipe(q.max(q_min));
        runs.push(svc::pipe_run(sys, k, m, dn, pts));
        for (_, a) in kept {
            let sd = &room.sides[a.j];
            let w = sd.at(a.s, -depth);
            let pz = e + a.c.at.2;
            let (m, dn) = svc::supply_pipe(a.c.flow.max(q_min));
            let mut pts = vec![v3(w.0, w.1, z), v3(w.0, w.1, pz), v3(a.c.at.0, a.c.at.1, pz)];
            if a.c.dir.2 < -port_align {
                // an underside connector is entered from below: along under it, then up into it
                let low = pz - svc::bend_of(m, dn) - svc::gap();
                if low > z {
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
    let r = if sys == SvcSys::Cold { cl.cw } else { cl.hw };
    let from = cl.f.p(r.0, r.1);
    let to = cl.f.p(r.0, -depth);
    let (m, dn) = svc::supply_pipe(flow.max(q_min));
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
fn drains(
    space: &mut Space<'_>,
    p: &GProject,
    k: usize,
    room: &Room,
    fx: &[usize],
    cl: &Cluster,
) -> Option<(Vec<GRun>, f64, Vec<(usize, &'static str)>, f64, bool)> {
    let st = &p.storeys[k];
    let e = st.elevation;
    let n = norms();
    let mut feet = vec![];
    let (waste_down, waste_off) = (rule("services.water.waste_down"), rule("services.water.waste_off"));
    for &i in fx {
        let f = &st.layout.fixtures[i];
        for c in svc::svc_ports(f).into_iter().filter(|c| c.sys == SvcSys::Sanitary) {
            let mut q = (c.at.0, c.at.1);
            if c.dir.2 > -waste_down {
                // a waste into the wall: down inside it behind the connector
                q = (q.0 + c.dir.0 * waste_off, q.1 + c.dir.1 * waste_off);
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
    let (sc, ss) = (n.slope(dn_c) / 100.0, (g() + cl.rs, g() + cl.rs));
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
    let (w, d) = (room.clear[2] - room.clear[0], room.clear[3] - room.clear[1]);
    let span = w.max(d) + 0.5;
    for swap in [false, true] {
        // collector along u at v = line (swap: along v at u = line)
        let uv = |f: &Foot| if swap { (f.uv.1, f.uv.0) } else { f.uv };
        let s0 = if swap { ss.0 } else { ss.1 };
        let a_s = if swap { ss.1 } else { ss.0 };
        // the risers stand in a row along u from the stack: a collector along v first leaves
        // the stack along v past the row, then turns across to its line
        let a_c = if swap {
            ((cl.cw.1 + cl.rc).max(cl.hw.1 + cl.rh) + gap + oc / 2.0).max(a_s + stack_end + tc + gap)
        } else {
            a_s
        };
        let ext = a_c - a_s;
        let mut line = s0 + tc + if swap { tc } else { os.max(stack_end) } + gap;
        while line < span {
            let mut joins: Vec<(f64, &Foot)> = vec![];
            let mut ok = true;
            for f in &feet {
                let (a, b) = uv(f);
                let need = elbow(f.dn) + oc.max(joint(f.dn, dn_c));
                if (b - line).abs() < need || a < a_c + tc + tee_c + gap {
                    ok = false;
                    break;
                }
                joins.push((a, f));
            }
            joins.sort_by(|x, y| x.0.total_cmp(&y.0));
            // the far join ends the collector in a joint; the others are tees on it
            let n_j = joins.len();
            let a_0 = a_c + tc;
            ok &= n_j != 1 || joins[0].0 - a_0 >= joint(joins[0].1.dn, dn_c) + gap;
            ok &= (1..n_j).all(|i| {
                let end = if i + 1 == n_j { joint(joins[i].1.dn, dn_c) } else { tee_c };
                joins[i].0 - joins[i - 1].0 >= join_min.max(tee_c + end + gap)
            });
            if ok {
                let to_xy = |a: f64, b: f64| if swap { cl.f.p(b, a) } else { cl.f.p(a, b) };
                // rises from the stack: final leg, collector, branches
                let leg0 = line - s0;
                let mut rise_max: f64 = 0.0;
                for (a, f) in &joins {
                    let (_, b) = uv(f);
                    let r = sc * (leg0 + (a - a_s)) + n.slope(f.dn) / 100.0 * (b - line).abs();
                    rise_max = rise_max.max(r);
                }
                let z0 = z_max - rise_max;
                if z0 >= z_min {
                    let zc = |a: f64| z0 + sc * (leg0 + (a - a_s));
                    let far = joins.last().unwrap().0;
                    let mut pts = vec![(far, line), (a_c, line), (a_c, s0)];
                    let mut zs = vec![zc(far), z0 + sc * (leg0 + ext), z0 + sc * ext];
                    if ext > 0.0 {
                        pts.push((a_s, s0));
                        zs.push(z0);
                    }
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
                    for (a, f) in &joins {
                        let (_, b) = uv(f);
                        let zj = zc(*a);
                        let zf = zj + n.slope(f.dn) / 100.0 * (b - line).abs();
                        let foot = cl.f.p(f.uv.0, f.uv.1);
                        let join = to_xy(*a, line);
                        let pz = f.c.at.2 + e;
                        let mut pts = vec![v3(f.c.at.0, f.c.at.1, pz)];
                        if (foot.0 - f.c.at.0).abs() > 1e-9 || (foot.1 - f.c.at.1).abs() > 1e-9 {
                            pts.push(v3(foot.0, foot.1, pz));
                        }
                        pts.push(v3(foot.0, foot.1, zf));
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
            line += step;
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
            let mut cands: Vec<Option<usize>> = (0..clusters.len())
                .filter(|&c| (0..4).any(|j| clusters[c].f.same(&corner(room, j))))
                .map(Some)
                .collect();
            cands.extend([None, None, None, None]);
            // a new stack goes into the corner nearest a riser shaft, when the storey has one
            let corners: Vec<(f64, f64)> = (0..4).map(|j| corner(room, j).o).collect();
            let order = shaft_order(&corners, &riser_shafts[k]);
            let mut best: Option<(usize, RoomDesign, Option<Cluster>, usize)> = None;
            let mut corner_i = 0;
            for cand in cands {
                let m = space.mark();
                let mut fresh: Option<Cluster> = None;
                let cl: &Cluster = match cand {
                    Some(c) => &clusters[c],
                    None => {
                        let f = corner(room, order[corner_i]);
                        corner_i += 1;
                        let probe = Cluster::new(f, false, 0.0, 0.0);
                        let (cw, hw, wc) = flows_at(p, &rooms, probe.f.p(g() + probe.rs, g() + probe.rs));
                        let mut c = Cluster::new(f, wc, cw, hw);
                        c.vent = roof_top(p, c.stack()) + norms().vent_above_roof();
                        if !cluster_clear(p, &c, k) || !space.try_add(&verticals(&c, (e + z_cw(), e + z_hw())), svc::gap()) {
                            space.rollback(m);
                            continue;
                        }
                        fresh = Some(c);
                        fresh.as_ref().unwrap()
                    }
                };
                let d = design_room(&mut space, p, k, room, fx, cl);
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
                    space.try_add(&verticals(&c, (e + z_cw(), e + z_hw())), svc::gap());
                    clusters.push(c);
                    clusters.len() - 1
                }
                None => ci,
            };
            let d = design_room(&mut space, p, k, room, fx, &clusters[ci]);
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
            });
            runs.extend(d.runs);
            for (fx, c, sys) in d.links {
                links.push(GSvcLink { storey: k, fixture: fx, connector: c, sys });
            }
        }
    }
    let used: Vec<&Cluster> = clusters.iter().filter(|c| !c.joins.is_empty()).collect();
    if !used.is_empty() {
        let kf = k_factor(spec.typology);
        risers_and_mains(p, &mut space, &used, kf, &mut runs);
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
    p.metrics.push(("water_loading_units", lu));
    unserved += unplaced;
    p.metrics.push(("water_ports_unplaced", unplaced as f64));
    p.metrics.push(("water_unserved", unserved as f64));
    unserved
}

fn design_room(space: &mut Space<'_>, p: &GProject, k: usize, room: &Room, fx: &[usize], cl: &Cluster) -> RoomDesign {
    let st = &p.storeys[k];
    let mut d = RoomDesign::default();
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
    let (mc, dc) = svc::supply_pipe(qc.max(q_min));
    let (mh, dh) = svc::supply_pipe(qh.max(q_min));
    let (rc, rh) = (svc::od(mc, dc) / 2.0, svc::od(mh, dh) / 2.0);
    let chase_gap = rule("services.water.chase_gap");
    let c1 = (rc + chase_gap).max(rule("services.water.chase_min"));
    let c2 = c1 + rc + rh + chase_gap;
    // back to back rooms share the wall their chases run in: when the cold-near-the-room order
    // meets the neighbour's runs, the hot one nearer is tried
    for (dc, dh) in [(c1, c2), (c2, c1)] {
        let (cw_runs, cw_links, cw_q, _) = supply(p, k, room, fx, cl, SvcSys::Cold, dc, rc);
        let (hw_runs, hw_links, hw_q, _) = supply(p, k, room, fx, cl, SvcSys::Hot, dh, rh);
        let mut sup: Vec<GRun> = cw_runs;
        sup.extend(hw_runs);
        if sup.is_empty() {
            break;
        }
        if space.try_add(&sup, svc::gap()) {
            d.runs = sup;
            d.cw = cw_q;
            d.hw = hw_q;
            d.links.extend(cw_links.into_iter().map(|(f, c)| (f, c, SvcSys::Cold)));
            d.links.extend(hw_links.into_iter().map(|(f, c)| (f, c, SvcSys::Hot)));
            break;
        }
    }
    if let Some((runs, z0, links, du, wc)) = drains(space, p, k, room, fx, cl) {
        d.runs.extend(runs);
        d.stack_z = Some(z0);
        d.du = du;
        d.wc = wc;
        d.links.extend(links.into_iter().map(|(f, c)| (f, c, SvcSys::Sanitary)));
    }
    d.served = d.links.len();
    d
}

/// The verticals of every cluster sized storey by storey, and under the ground slab the cold
/// and hot mains to a capped inlet and the building drain to a capped outlet.
fn risers_and_mains(p: &GProject, space: &mut Space<'_>, used: &[&Cluster], kf: f64, runs: &mut Vec<GRun>) {
    let mut cw_feet = vec![];
    let mut hw_feet = vec![];
    let mut drain_feet = vec![];
    for cl in used {
        let e = |k: usize| p.storeys[k].elevation;
        let mut js = cl.joins.clone();
        js.sort_by_key(|j| j.k);
        // cold and hot risers: segments between the storeys they serve, sized by the flow above
        for (sys, xy, zrel, flow) in [
            (SvcSys::Cold, cl.cw_xy(), z_cw(), &(|j: &Join| j.cw) as &dyn Fn(&Join) -> f64),
            (SvcSys::Hot, cl.hw_xy(), z_hw(), &(|j: &Join| j.hw) as &dyn Fn(&Join) -> f64),
        ] {
            let served: Vec<&Join> = js.iter().filter(|j| flow(j) > 0.0).collect();
            if served.is_empty() {
                continue;
            }
            let bottom = if sys == SvcSys::Cold { under_cw() } else { under_hw() };
            let mut z0 = bottom;
            for (i, j) in served.iter().enumerate() {
                let q: f64 = served[i..].iter().map(|x| flow(x)).sum();
                let (m, dn) = svc::supply_pipe(q);
                let z1 = e(j.k) + zrel;
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
    let foot = svc::bbox(&p.storeys[0].layout.slab);
    for (sys, feet, z) in [
        (SvcSys::Cold, &cw_feet, under_cw()),
        (SvcSys::Hot, &hw_feet, under_hw()),
    ] {
        if let Some(r) = comb(space, sys, feet, z, None, &foot) {
            runs.extend(r);
        }
    }
    // stacks start at the lowest join; the building drain hangs below
    let tops: Vec<f64> = used
        .iter()
        .filter_map(|c| c.joins.iter().filter_map(|j| j.stack_z).reduce(f64::min))
        .collect();
    if let Some(r) = comb(space, SvcSys::Sanitary, &drain_feet, under_drain(), Some(kf), &foot) {
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
        None => svc::supply_pipe(total_q),
    };
    let om = svc::od(mm, dm);
    let (gap, join_gap) = (svc::gap(), rule("services.water.main_join_gap"));
    let out_len = out_len();
    for along_x in [true, false] {
        let (lo, hi) = if along_x { (foot[1], foot[3]) } else { (foot[0], foot[2]) };
        let mut line = lo - 2.0;
        while line < hi + 2.0 {
            // (join on the main, foot index, jog off the foot)
            let mut joins: Vec<(f64, usize, f64)> = vec![];
            // per foot: the shortest step along the main off the foot and the nearest the foot
            // may stand to the main when it steps
            let mut steps: Vec<(f64, f64)> = vec![];
            let mut ok = true;
            for (i, f) in feet.iter().enumerate() {
                let (a, b) = if along_x { (f.0 .0, f.0 .1) } else { (f.0 .1, f.0 .0) };
                let (m, dn) = branch_pipe(sys, f, fall);
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
                let jog = if into_a.abs() > 0.5 { (off + bend + gap).max(f.4 + svc::od(m, dn) / 2.0 + gap) } else { 0.0 };
                if (b - line).abs() < if jog > 0.0 { bend } else { off } + om + gap {
                    ok = false;
                    break;
                }
                joins.push((a + into_a.signum() * jog, i, jog));
                steps.push(((off + bend + gap).max(f.4 + svc::od(m, dn) / 2.0 + gap), bend + om + gap));
            }
            joins.sort_by(|x, y| x.0.total_cmp(&y.0));
            // feet too close along the main for their tees: each later one steps on along the
            // main, beside its foot, to the spacing the tees need
            // an end join turns the main into its branch: the elbow and the reducer there take
            // more of the main than a tee does
            let end_trim = |i: usize| {
                let (m, dn) = branch_pipe(sys, &feet[i], fall);
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
                    let a_new = want.max(a_f + steps[i].0);
                    joins[n] = (a_new, i, (a_new - a_f).abs());
                }
            }
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
                    let (_, dn) = branch_pipe(sys, f, fall);
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
                    let (m, dn) = branch_pipe(sys, f, fall);
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
            }
            line += 0.5;
        }
    }
    None
}

fn branch_pipe(_sys: SvcSys, f: &Feed, fall: Option<f64>) -> (PipeMaterial, u32) {
    match fall {
        Some(kf) => (PipeMaterial::Pvc, svc::drain_dn(kf * f.2.sqrt(), drain_dn_min())),
        None => svc::supply_pipe(f.1),
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
