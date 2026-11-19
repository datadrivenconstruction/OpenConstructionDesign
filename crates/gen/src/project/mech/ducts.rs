//! The duct network read back from the ventilation as a tree per system from the unit port
//! to every terminal: each run classed (plant, riser, main, branch), its velocity against
//! the cap of its class, its friction, every fitting with its loss coefficient (elbows by
//! radius and angle, tees and takeoffs by the velocity ratios, transitions by the area ratio
//! and taper), insulation, VAV boxes on the supply branches, dampers, and the pressure drop
//! of each path from the unit to a terminal with the critical one.

use super::super::hvac::air_ports;
use super::super::rules::rule;
use super::super::{AirSys, GProject};
use super::equip::{self, Equip, Kind, Medium, Port};
use super::geo::{dist, dot, grow, len, overlap, seg_box, soffit, turn, unit, Beam, Bx, Solid, P3};
use crate::WallRole;
use ddc_geom::mep::duct_sizing::{dynamic, friction_rect, friction_round};
use ddc_geom::mep::fittings::{contraction_zeta, elbow_zeta, expansion_zeta, tee_converging, tee_diverging};
use std::collections::HashMap;
use std::f64::consts::PI;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum Class {
    Plant,
    Riser,
    Main,
    Branch,
}

impl Class {
    pub fn name(self) -> &'static str {
        match self {
            Class::Plant => "plant",
            Class::Riser => "riser",
            Class::Main => "main",
            Class::Branch => "branch",
        }
    }
}

/// One duct run in flow-tree order: points from the unit side.
#[derive(Clone, Debug, PartialEq)]
pub struct Run {
    /// Index in the project's ducts.
    pub duct: usize,
    pub sys: AirSys,
    /// The ventilation system of the run, as in the project's ducts.
    pub net: u8,
    pub storey: usize,
    pub points: Vec<P3>,
    pub size: (f64, f64),
    pub flow: f64,
    pub bend: Option<f64>,
    pub class: Class,
    pub parent: Option<usize>,
    pub children: Vec<usize>,
    pub term: Option<usize>,
    pub len: f64,
    /// m/s, cap of the class, Pa/m.
    pub v: f64,
    pub v_max: f64,
    pub r: f64,
    /// Pa: friction along the run, its elbows, the fitting where it leaves its parent.
    pub friction: f64,
    pub fittings: f64,
    pub insulation: f64,
    /// Sheet metal and insulation surface, m2.
    pub sheet: f64,
    pub ins_area: f64,
}

impl Run {
    pub fn area(&self) -> f64 {
        sec_area(self.size)
    }
    pub fn dp(&self) -> f64 {
        self.friction + self.fittings
    }
    /// Half width and half height of the insulated section.
    pub fn half(&self) -> (f64, f64) {
        let (w, h) = if self.size.1 > 0.0 { self.size } else { (self.size.0, self.size.0) };
        (w / 2.0 + self.insulation, h / 2.0 + self.insulation)
    }
    pub fn boxes(&self) -> Vec<Bx> {
        let (hw, hh) = self.half();
        self.points.windows(2).map(|w| seg_box(w[0], w[1], hw, hh)).collect()
    }
}

pub fn sec_area(size: (f64, f64)) -> f64 {
    if size.1 > 0.0 {
        size.0 * size.1
    } else {
        PI * size.0 * size.0 / 4.0
    }
}

pub fn sec_friction(q: f64, size: (f64, f64)) -> f64 {
    if size.1 > 0.0 {
        friction_rect(q, size.0, size.1)
    } else {
        friction_round(q, size.0)
    }
}

/// An air terminal on the network.
#[derive(Clone, Debug, PartialEq)]
pub struct Term {
    pub sys: AirSys,
    pub storey: usize,
    pub fixture: usize,
    pub room: Option<usize>,
    pub at: P3,
    pub flow: f64,
    pub run: Option<usize>,
    /// Drop across the terminal, and across its VAV box when it has one, Pa.
    pub dp: f64,
    pub vav: Option<usize>,
    /// Unit to terminal, Pa.
    pub path: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum FitKind {
    Elbow,
    Mitre,
    Tee,
    Takeoff,
    Transition,
}

impl FitKind {
    pub fn name(self) -> &'static str {
        match self {
            FitKind::Elbow => "elbow",
            FitKind::Mitre => "mitred elbow",
            FitKind::Tee => "tee",
            FitKind::Takeoff => "takeoff",
            FitKind::Transition => "transition",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Fitting {
    pub kind: FitKind,
    pub sys: AirSys,
    pub storey: usize,
    pub at: P3,
    pub size: (f64, f64),
    /// Branch or outlet size.
    pub size2: (f64, f64),
    pub angle: f64,
    pub radius: f64,
    pub zeta: f64,
    pub dp: f64,
    pub run: usize,
}

/// The critical path of a system: pressure from the unit port to its farthest terminal.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Crit {
    pub dp: f64,
    pub term: Option<usize>,
    pub runs: Vec<usize>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Air {
    pub runs: Vec<Run>,
    pub terms: Vec<Term>,
    pub fittings: Vec<Fitting>,
    pub boxes: Vec<Equip>,
    pub dampers: Vec<Equip>,
    /// Supply, extract.
    pub critical: [Crit; 2],
    pub faults: Vec<String>,
    /// How far each storey's ducts were lowered to pass under the beams, m.
    pub lowered: Vec<f64>,
}

fn key(p: P3) -> (i64, i64, i64) {
    let r = |v: f64| (v * 1000.0).round() as i64;
    (r(p.0), r(p.1), r(p.2))
}

fn sys_index(s: AirSys) -> usize {
    match s {
        AirSys::Supply => 0,
        AirSys::Extract => 1,
    }
}

/// Elbow radius over the duct dimension in the plane of the turn.
fn r_over_d(run: &Run, d1: P3, d2: P3) -> (f64, f64) {
    let flat = d1.2.abs() < 1e-6 && d2.2.abs() < 1e-6;
    let dim = if run.size.1 > 0.0 {
        if flat {
            run.size.0
        } else {
            run.size.1
        }
    } else {
        run.size.0
    };
    let radius = run.bend.unwrap_or(rule("services.hvac.round_bend") * dim);
    (radius / dim.max(1e-9), radius)
}

/// Reads the network of `p` and analyses it; `vav` puts a box on every supply branch.
pub fn analyse(p: &GProject, vav: bool, beams: &[Beam], solids: &[Solid]) -> Air {
    let n = p.storeys.len();
    let mut air = Air::default();
    let h = &p.hvac;
    if h.plant.is_empty() {
        if !h.ducts.is_empty() {
            air.faults.push("ducts without an air handling unit".into());
        }
        return air;
    }
    let roof = p.roof.elevation;
    let nets = 1 + h.nets.len();
    for (sys, net) in [AirSys::Supply, AirSys::Extract].into_iter().flat_map(|s| (0..nets).map(move |n| (s, n))) {
        // the unit of a network is the one its plant links name
        let unit = h.links.iter().find(|l| l.storey == n && usize::from(l.net) == net).map_or(net, |l| l.fixture);
        let Some(unit_fx) = h.plant.get(unit) else { continue };
        let ids: Vec<usize> = (0..h.ducts.len()).filter(|&i| h.ducts[i].sys == sys && usize::from(h.ducts[i].net) == net).collect();
        if ids.is_empty() {
            continue;
        }
        let mut ends: HashMap<(i64, i64, i64), Vec<(usize, usize)>> = HashMap::new();
        for &i in &ids {
            let d = &h.ducts[i];
            ends.entry(key(d.points[0])).or_default().push((i, 0));
            ends.entry(key(*d.points.last().expect("a run has points"))).or_default().push((i, 1));
        }
        let port = if sys == AirSys::Supply { "Supply" } else { "Return" };
        let Some((_, q)) = air_ports(unit_fx).into_iter().find(|c| c.0 == port) else {
            air.faults.push(format!("{sys:?}: the unit has no {port} port"));
            continue;
        };
        let root = (q.0, q.1, roof + q.2);
        // breadth first from the unit port: each run is reached at one end
        let mut seen: HashMap<usize, usize> = HashMap::new();
        let mut queue: Vec<((i64, i64, i64), Option<usize>)> = vec![(key(root), None)];
        let mut at = 0;
        while at < queue.len() {
            let (node, parent) = queue[at];
            at += 1;
            for &(d, e) in ends.get(&node).map_or(&[][..], |v| v.as_slice()) {
                if seen.contains_key(&d) {
                    continue;
                }
                let g = &h.ducts[d];
                let mut pts = g.points.clone();
                if e == 1 {
                    pts.reverse();
                }
                let ri = air.runs.len();
                seen.insert(d, ri);
                air.runs.push(Run {
                    duct: d,
                    sys,
                    net: g.net,
                    storey: g.storey,
                    size: g.size,
                    flow: g.flow,
                    bend: g.bend,
                    class: Class::Branch,
                    parent,
                    children: vec![],
                    term: None,
                    len: len(&pts),
                    v: 0.0,
                    v_max: 0.0,
                    r: 0.0,
                    friction: 0.0,
                    fittings: 0.0,
                    insulation: 0.0,
                    sheet: 0.0,
                    ins_area: 0.0,
                    points: pts,
                });
                if let Some(pi) = parent {
                    air.runs[pi].children.push(ri);
                }
                let far = *air.runs[ri].points.last().expect("a run has points");
                queue.push((key(far), Some(ri)));
            }
        }
        for &i in &ids {
            if !seen.contains_key(&i) {
                air.faults.push(format!("{sys:?}: duct {i} is not reached from the unit"));
            }
        }
        // terminals by their links
        for l in h.links.iter().filter(|l| l.sys == sys && usize::from(l.net) == net && l.storey < n) {
            let st = &p.storeys[l.storey];
            let f = &st.layout.fixtures[l.fixture];
            let Some((_, q)) = air_ports(f).into_iter().find(|c| c.0 == l.connector) else { continue };
            let at = (q.0, q.1, st.elevation + q.2);
            let run = air.runs.iter().position(|r| r.sys == sys && usize::from(r.net) == net && r.children.is_empty() && dist(*r.points.last().expect("points"), at) < 5e-3);
            let room = st.layout.rooms.iter().position(|r| super::geo::inside(f.at, &r.polygon));
            let dp = if sys == AirSys::Supply { rule("mep.hvac.dp_diffuser") } else { rule("mep.hvac.dp_grille") };
            let ti = air.terms.len();
            let flow = run.map_or(0.0, |r| air.runs[r].flow);
            if let Some(r) = run {
                air.runs[r].term = Some(ti);
            } else {
                air.faults.push(format!("{sys:?}: {} {} on storey {} is not connected", f.family, f.ty, l.storey));
            }
            air.terms.push(Term { sys, storey: l.storey, fixture: l.fixture, room, at, flow, run, dp, vav: None, path: 0.0 });
        }
    }
    // classes, velocities, friction, insulation
    let counts = leaf_counts(&air.runs);
    for (i, r) in air.runs.iter_mut().enumerate() {
        let vertical = r.points.windows(2).all(|w| (w[0].0 - w[1].0).abs() < 1e-6 && (w[0].1 - w[1].1).abs() < 1e-6);
        r.class = if r.storey >= n {
            Class::Plant
        } else if vertical {
            Class::Riser
        } else if counts[i] > 1 || r.size.1 > 0.0 {
            Class::Main
        } else {
            Class::Branch
        };
        r.v_max = match r.class {
            Class::Plant | Class::Riser => rule("services.hvac.v_riser"),
            Class::Main => rule("services.hvac.v_main"),
            Class::Branch => rule("services.hvac.v_branch"),
        };
        r.v = r.flow / 3600.0 / r.area();
        r.r = sec_friction(r.flow, r.size);
        r.friction = r.r * r.len;
        r.insulation = match (r.sys, r.storey >= n) {
            (_, true) => rule("mep.hvac.ins_outdoor"),
            (AirSys::Supply, false) => rule("mep.hvac.ins_supply"),
            (AirSys::Extract, false) => rule("mep.hvac.ins_extract"),
        };
        let perim = if r.size.1 > 0.0 { 2.0 * (r.size.0 + r.size.1) } else { PI * r.size.0 };
        let outer = if r.size.1 > 0.0 {
            2.0 * (r.size.0 + r.size.1 + 4.0 * r.insulation)
        } else {
            PI * (r.size.0 + 2.0 * r.insulation)
        };
        r.sheet = perim * r.len;
        r.ins_area = if r.insulation > 0.0 { outer * r.len } else { 0.0 };
        if r.v > r.v_max + 1e-9 {
            air.faults.push(format!("{:?} {} run {}: {:.2} m/s over the {:.1} m/s cap", r.sys, r.class.name(), r.duct, r.v, r.v_max));
        }
    }
    lower_under_beams(p, &mut air, beams);
    fittings(&mut air);
    flows_balance(&mut air);
    if vav {
        place_boxes(p, &mut air, solids);
    }
    dampers(p, &mut air);
    paths(&mut air);
    air
}

/// Number of terminals each run feeds.
fn leaf_counts(runs: &[Run]) -> Vec<usize> {
    let mut c = vec![0; runs.len()];
    for (i, r) in runs.iter().enumerate() {
        if r.children.is_empty() {
            let mut at = Some(i);
            while let Some(j) = at {
                c[j] += 1;
                at = runs[j].parent;
            }
        }
    }
    c
}

/// Lowers the ducts of each storey under the deepest beam they would meet, keeping the
/// terminals where they are: the drops get shorter, the risers longer or shorter.
fn lower_under_beams(p: &GProject, air: &mut Air, beams: &[Beam]) {
    let n = p.storeys.len();
    air.lowered = vec![0.0; n];
    if beams.is_empty() {
        return;
    }
    let clear = rule("mep.hvac.beam_clear");
    for (k, low) in air.lowered.iter_mut().enumerate() {
        for r in air.runs.iter().filter(|r| r.storey == k && r.class != Class::Riser) {
            for b in r.boxes() {
                for beam in beams {
                    let bb = grow(&beam.bbox(), clear);
                    if overlap(&b, &bb, 0.0) {
                        *low = low.max(b[5] - bb[2]);
                    }
                }
            }
        }
    }
    let band = |z: f64| (0..n).find(|&k| z >= p.storeys[k].elevation - 1e-6 && z <= soffit(p, k) + 1e-6);
    let terms: Vec<P3> = air.terms.iter().map(|t| t.at).collect();
    for r in air.runs.iter_mut().filter(|r| r.storey < n) {
        for q in r.points.iter_mut() {
            if terms.iter().any(|t| dist(*t, *q) < 5e-3) {
                continue;
            }
            if let Some(k) = band(q.2) {
                q.2 -= air.lowered[k];
            }
        }
        r.len = len(&r.points);
        r.friction = r.r * r.len;
    }
    // a drop must keep room for its elbow above the terminal neck
    let gap = rule("services.hvac.drop_gap");
    for r in air.runs.iter().filter(|r| r.term.is_some() && r.storey < n && air.lowered[r.storey] > 0.0) {
        let m = r.points.len();
        if m < 2 {
            continue;
        }
        let (top, neck) = (r.points[m - 2], r.points[m - 1]);
        let d = r.size.0.max(r.size.1);
        let need = r.bend.unwrap_or(rule("services.hvac.round_bend") * d) + gap;
        if top.2 - neck.2 < need - 1e-9 {
            air.faults.push(format!(
                "storey {}: lowered {:.2} m under the beams, the drop at ({:.2}, {:.2}) is {:.2} m, needs {:.2} m",
                r.storey,
                air.lowered[r.storey],
                neck.0,
                neck.1,
                top.2 - neck.2,
                need
            ));
        }
    }
}

/// Elbows inside each run; at each joint the tee, takeoff, transition or mitre where a run
/// leaves its parent, its loss charged to that run.
fn fittings(air: &mut Air) {
    let a_tee = rule("mep.hvac.takeoff_a");
    let half = rule("services.hvac.reducer_angle");
    let mut out = vec![];
    for i in 0..air.runs.len() {
        let r = &air.runs[i];
        let pd = dynamic(r.v);
        let mut loss = 0.0;
        for k in 1..r.points.len().saturating_sub(1) {
            let (d1, d2) = (unit(r.points[k - 1], r.points[k]), unit(r.points[k], r.points[k + 1]));
            let angle = turn(d1, d2);
            if angle < 1e-3 {
                continue;
            }
            let (rd, radius) = r_over_d(r, d1, d2);
            let z = elbow_zeta(rd, angle);
            loss += z * pd;
            out.push(Fitting {
                kind: FitKind::Elbow,
                sys: r.sys,
                storey: r.storey,
                at: r.points[k],
                size: r.size,
                size2: r.size,
                angle,
                radius,
                zeta: z,
                dp: z * pd,
                run: i,
            });
        }
        air.runs[i].fittings = loss;
    }
    // joints: by parent
    for pi in 0..air.runs.len() {
        let kids = air.runs[pi].children.clone();
        if kids.is_empty() {
            continue;
        }
        let pr = air.runs[pi].clone();
        let m = pr.points.len();
        let dp_dir = unit(pr.points[m - 2], pr.points[m - 1]);
        let at = pr.points[m - 1];
        let dirs: HashMap<usize, P3> = kids.iter().map(|&c| (c, unit(air.runs[c].points[0], air.runs[c].points[1]))).collect();
        let start = |c: usize| dirs[&c];
        let straight = kids.iter().copied().find(|&c| dot(dp_dir, start(c)) > 1.0 - 1e-6);
        let supply = pr.sys == AirSys::Supply;
        let (vc, ac, qc) = (pr.v, pr.area(), pr.flow);
        if kids.len() == 1 {
            let c = kids[0];
            let cr = &air.runs[c];
            let (z, dp, kind) = if straight.is_some() {
                let (a1, a2, v1, v2) = if supply { (ac, cr.area(), vc, cr.v) } else { (cr.area(), ac, cr.v, vc) };
                if (a1 - a2).abs() < 1e-9 {
                    continue;
                }
                if a2 < a1 {
                    let z = contraction_zeta(a1, a2, half);
                    (z, z * dynamic(v2), FitKind::Transition)
                } else {
                    let z = expansion_zeta(a1, a2, half);
                    (z, z * dynamic(v1), FitKind::Transition)
                }
            } else {
                let z = elbow_zeta(0.0, turn(dp_dir, start(c)));
                (z, z * dynamic(cr.v.max(vc)), FitKind::Mitre)
            };
            out.push(Fitting {
                kind,
                sys: pr.sys,
                storey: cr.storey,
                at,
                size: pr.size,
                size2: cr.size,
                angle: turn(dp_dir, start(c)),
                radius: 0.0,
                zeta: z,
                dp,
                run: c,
            });
            air.runs[c].fittings += dp;
            continue;
        }
        let branch_q: f64 = kids.iter().filter(|&&c| Some(c) != straight).map(|&c| air.runs[c].flow).sum();
        let mut worst = (0.0, 0.0, kids[0]);
        for &c in &kids {
            let cr = &air.runs[c];
            let is_straight = Some(c) == straight;
            let vs = straight.map_or(0.0, |s| air.runs[s].v);
            let z = if supply {
                let (zb, zs) = tee_diverging(vc, cr.v, vs, a_tee);
                if is_straight {
                    zs
                } else {
                    zb
                }
            } else if is_straight {
                tee_converging(vc, 0.0, branch_q / qc.max(1e-9), a_tee).1
            } else {
                tee_converging(vc, cr.v, cr.flow / qc.max(1e-9), a_tee).0
            };
            let dp = z * dynamic(vc);
            air.runs[c].fittings += dp;
            if !is_straight && dp >= worst.1 {
                worst = (z, dp, c);
            }
        }
        let all_leaves = kids.iter().filter(|&&c| Some(c) != straight).all(|&c| air.runs[c].class == Class::Branch);
        let kind = if straight.is_some() && all_leaves { FitKind::Takeoff } else { FitKind::Tee };
        out.push(Fitting {
            kind,
            sys: pr.sys,
            storey: air.runs[worst.2].storey,
            at,
            size: pr.size,
            size2: air.runs[worst.2].size,
            angle: turn(dp_dir, start(worst.2)),
            radius: 0.0,
            zeta: worst.0,
            dp: worst.1,
            run: worst.2,
        });
    }
    air.fittings = out;
}

/// Every joint passes on what it takes: a parent carries the sum of its children.
fn flows_balance(air: &mut Air) {
    for r in &air.runs {
        if r.children.is_empty() {
            continue;
        }
        let sum: f64 = r.children.iter().map(|&c| air.runs[c].flow).sum();
        if (sum - r.flow).abs() > 1.0 {
            let msg = format!("{:?} run {}: carries {:.0} m3/h, its branches {:.0}", r.sys, r.duct, r.flow, sum);
            air.faults.push(msg);
        }
    }
}

/// A box on each supply branch: on its first level stretch with the inlet straight before
/// it and room after it, clear of the structure, other ducts, walls and the slab.
fn place_boxes(p: &GProject, air: &mut Air, solids: &[Solid]) {
    let sizes = equip::vav_sizes();
    let (straight, after, step) = (rule("mep.hvac.vav_straight"), rule("mep.hvac.vav_out"), rule("mep.hvac.vav_step"));
    let gap = rule("services.gap");
    let duct_boxes: Vec<(usize, Bx)> =
        air.runs.iter().enumerate().flat_map(|(i, r)| r.boxes().into_iter().map(move |b| (i, b))).collect();
    for t in 0..air.terms.len() {
        let term = &air.terms[t];
        let (Some(ri), AirSys::Supply) = (term.run, term.sys) else { continue };
        let r = &air.runs[ri];
        let s = equip::pick(&sizes, term.flow);
        let k = r.storey;
        let st = &p.storeys[k];
        let top = soffit(p, k) - gap;
        let ceil = super::geo::ceiling(p, k);
        let hh = if r.size.1 > 0.0 { r.size.1 / 2.0 } else { r.size.0 / 2.0 };
        let mut placed = None;
        'seg: for w in r.points.windows(2) {
            let (a, b) = (w[0], w[1]);
            if (a.2 - b.2).abs() > 1e-6 {
                continue;
            }
            let l = dist(a, b);
            let u0 = straight * s.d + s.l / 2.0;
            let u1 = l - after - s.l / 2.0;
            let d = unit(a, b);
            let rot = d.1.atan2(d.0);
            let mut u = u0;
            while u <= u1 + 1e-9 {
                let c = (a.0 + d.0 * u, a.1 + d.1 * u, a.2);
                // top flush with the duct's top, the inlet on the duct axis
                let base = c.2 + hh - s.h;
                let mut e = Equip::new(Kind::VavBox, k, (c.0, c.1, base), rot, (s.l, s.w, s.h));
                e.net = r.net;
                let bx = e.bbox();
                let free = bx[5] <= top
                    && bx[2] >= ceil
                    && !solids.iter().any(|o| overlap(&bx, &o.b, gap))
                    && !duct_boxes.iter().any(|(j, db)| *j != ri && overlap(&bx, db, gap))
                    && !st.layout.walls.iter().any(|wl| crosses_wall(&bx, wl.a, wl.b, wl.role));
                if free {
                    e.room = term.room;
                    let half = s.l / 2.0;
                    e.ports = vec![
                        Port { name: "Inlet", medium: Medium::SupplyAir, at: e.local(-half, 0.0, c.2 - base), dir: e.dir(-1.0, 0.0, 0.0), size: s.d, flow: term.flow },
                        Port { name: "Outlet", medium: Medium::SupplyAir, at: e.local(half, 0.0, c.2 - base), dir: e.dir(1.0, 0.0, 0.0), size: s.d, flow: term.flow },
                    ];
                    e.duty = vec![
                        ("flow_m3h", term.flow),
                        ("min_m3h", term.flow * rule("mep.hvac.vav_min")),
                        ("dp_pa", rule("mep.hvac.dp_vav")),
                        ("inlet_m", s.d),
                    ];
                    placed = Some(e);
                    break 'seg;
                }
                u += step;
            }
        }
        match placed {
            Some(e) => {
                air.terms[t].vav = Some(air.boxes.len());
                air.terms[t].dp += rule("mep.hvac.dp_vav");
                air.boxes.push(e);
            }
            None => air.faults.push(format!("no room for the VAV box of the terminal at ({:.2}, {:.2}) on storey {k}", term.at.0, term.at.1)),
        }
    }
}

/// Whether a wall axis passes through a box's plan (walls a box may not straddle).
fn crosses_wall(b: &Bx, a: (f64, f64), c: (f64, f64), role: WallRole) -> bool {
    let t = role.thickness() / 2.0;
    let (x0, y0, x1, y1) = (b[0] - t, b[1] - t, b[3] + t, b[4] + t);
    // the wall's own box against the plan box, then the axis line against it
    if a.0.max(c.0) < x0 || a.0.min(c.0) > x1 || a.1.max(c.1) < y0 || a.1.min(c.1) > y1 {
        return false;
    }
    let side = |x: f64, y: f64| (c.0 - a.0) * (y - a.1) - (c.1 - a.1) * (x - a.0);
    let s = [side(x0, y0), side(x1, y0), side(x1, y1), side(x0, y1)];
    s.iter().any(|v| *v > 0.0) && s.iter().any(|v| *v < 0.0)
}

/// Fire dampers where a storey's ducts leave the shaft and where they cross a fire-rated wall; a
/// volume damper on each branch takeoff (extract always, supply by rule).
fn dampers(p: &GProject, air: &mut Air) {
    let n = p.storeys.len();
    let dl = rule("mep.hvac.damper_len");
    for r in &air.runs {
        if r.storey >= n || r.class == Class::Riser {
            continue;
        }
        let from_riser = r.parent.is_some_and(|q| air.runs[q].class == Class::Riser || air.runs[q].class == Class::Plant);
        let (w, h) = if r.size.1 > 0.0 { r.size } else { (r.size.0, r.size.0) };
        let make = |kind: Kind, at: P3, d: P3| {
            let mut e = Equip::new(kind, r.storey, (at.0, at.1, at.2 - h / 2.0), d.1.atan2(d.0), (dl, w, h));
            e.duty = vec![("flow_m3h", r.flow)];
            e.net = r.net;
            e
        };
        if from_riser {
            let ring = p.storeys[r.storey]
                .layout
                .shafts
                .iter()
                .filter(|s| s.kind == crate::ShaftKind::Duct)
                .nth(usize::from(r.net))
                .map(|s| s.ring.clone())
                .unwrap_or_default();
            if let Some(at) = leave_ring(&r.points, &ring) {
                let d = unit(r.points[0], r.points[1]);
                air.dampers.push(make(Kind::FireDamper, at, d));
            }
        }
        let rated = rule("mep.hvac.fire_damper_min");
        let walls = p.storeys[r.storey].layout.walls.iter().filter(|w| w.fire.is_some_and(|f| f.minutes as f64 >= rated));
        for wl in walls {
            for w in r.points.windows(2) {
                for at in wall_cross(w[0], w[1], wl.a, wl.b, wl.bulge) {
                    // a duct above the wall top (the soffit of its storey) or below its floor passes over
                    if !in_wall_height(at.2, p.storeys[r.storey].elevation, super::geo::soffit(p, r.storey)) {
                        continue;
                    }
                    let near = air.dampers.iter().any(|q| q.storey == r.storey && q.kind == Kind::FireDamper && dist(q.at, at) < 2.0 * dl);
                    if !near {
                        air.dampers.push(make(Kind::FireDamper, at, unit(w[0], w[1])));
                    }
                }
            }
        }
        let takeoff = r.class == Class::Branch && r.parent.is_some() && r.term.is_some();
        if takeoff && (r.sys == AirSys::Extract || rule("mep.hvac.volume_damper_supply") >= 1.0) {
            let (a, b) = (r.points[0], r.points[1]);
            let l = dist(a, b);
            let u = rule("mep.hvac.damper_off").min(l / 2.0);
            let d = unit(a, b);
            air.dampers.push(make(Kind::VolumeDamper, (a.0 + d.0 * u, a.1 + d.1 * u, a.2), d));
        }
    }
    for (i, d) in air.dampers.iter_mut().enumerate() {
        d.tag = format!("{}{}", d.kind.tag(), i + 1);
    }
}

/// Where the run segment `a`-`b` crosses the wall axis `c`-`d` in plan, at the run's height.
fn seg_cross(a: P3, b: P3, c: (f64, f64), d: (f64, f64)) -> Option<P3> {
    let (r, s) = ((b.0 - a.0, b.1 - a.1), (d.0 - c.0, d.1 - c.1));
    let den = r.0 * s.1 - r.1 * s.0;
    if den.abs() < 1e-12 {
        return None;
    }
    let qp = (c.0 - a.0, c.1 - a.1);
    let u = (qp.0 * s.1 - qp.1 * s.0) / den;
    let v = (qp.0 * r.1 - qp.1 * r.0) / den;
    ((0.0..=1.0).contains(&u) && (0.0..=1.0).contains(&v)).then(|| (a.0 + r.0 * u, a.1 + r.1 * u, a.2 + (b.2 - a.2) * u))
}

/// Whether height `z` lies within a wall that stands from `floor` to `top`.
fn in_wall_height(z: f64, floor: f64, top: f64) -> bool {
    z >= floor - 1e-6 && z <= top + 1e-6
}

/// Where the run segment `a`-`b` crosses a wall in plan: its chord `c`-`d`, or the arc over the
/// chord when `bulge` (tan of a quarter of the sweep, positive counter-clockwise) is not zero.
fn wall_cross(a: P3, b: P3, c: (f64, f64), d: (f64, f64), bulge: f64) -> Vec<P3> {
    if bulge.abs() < 1e-9 {
        return seg_cross(a, b, c, d).into_iter().collect();
    }
    let sweep = 4.0 * bulge.atan();
    let chord = ((d.0 - c.0).hypot(d.1 - c.1)).max(1e-9);
    let left = (-(d.1 - c.1) / chord, (d.0 - c.0) / chord);
    let (m, half) = (((c.0 + d.0) / 2.0, (c.1 + d.1) / 2.0), sweep / 2.0);
    let off = chord / 2.0 / half.tan();
    let o = (m.0 + left.0 * off, m.1 + left.1 * off);
    let rad = (chord / 2.0 / half.sin()).abs();
    let (r, f) = ((b.0 - a.0, b.1 - a.1), (a.0 - o.0, a.1 - o.1));
    let (qa, qb, qc) = (r.0 * r.0 + r.1 * r.1, 2.0 * (f.0 * r.0 + f.1 * r.1), f.0 * f.0 + f.1 * f.1 - rad * rad);
    let disc = qb * qb - 4.0 * qa * qc;
    if qa < 1e-12 || disc < 0.0 {
        return vec![];
    }
    let phi0 = (c.1 - o.1).atan2(c.0 - o.0);
    [(-qb - disc.sqrt()) / (2.0 * qa), (-qb + disc.sqrt()) / (2.0 * qa)]
        .into_iter()
        .filter(|u| (0.0..=1.0).contains(u))
        .filter(|u| {
            let q = (a.0 + r.0 * u, a.1 + r.1 * u);
            let da = (q.1 - o.1).atan2(q.0 - o.0) - phi0;
            let along = if sweep > 0.0 { da.rem_euclid(std::f64::consts::TAU) } else { (-da).rem_euclid(std::f64::consts::TAU) };
            along <= sweep.abs() + 1e-9
        })
        .map(|u| (a.0 + r.0 * u, a.1 + r.1 * u, a.2 + (b.2 - a.2) * u))
        .collect()
}

/// Where a run first passes out of a plan ring.
fn leave_ring(pts: &[P3], ring: &[(f64, f64)]) -> Option<P3> {
    if ring.is_empty() {
        return None;
    }
    for w in pts.windows(2) {
        let (a, b) = (w[0], w[1]);
        let (ia, ib) = (super::geo::inside((a.0, a.1), ring), super::geo::inside((b.0, b.1), ring));
        if ia && !ib {
            // bisect to the ring edge
            let (mut lo, mut hi) = (0.0, 1.0);
            for _ in 0..40 {
                let m = (lo + hi) / 2.0;
                let q = (a.0 + (b.0 - a.0) * m, a.1 + (b.1 - a.1) * m);
                if super::geo::inside(q, ring) {
                    lo = m;
                } else {
                    hi = m;
                }
            }
            return Some((a.0 + (b.0 - a.0) * hi, a.1 + (b.1 - a.1) * hi, a.2 + (b.2 - a.2) * hi));
        }
    }
    None
}

/// Pressure from the unit to each terminal and the critical path of each system.
fn paths(air: &mut Air) {
    for t in 0..air.terms.len() {
        let Some(r) = air.terms[t].run else { continue };
        let mut sum = air.terms[t].dp;
        let mut runs = vec![];
        let mut at = Some(r);
        while let Some(i) = at {
            sum += air.runs[i].dp();
            runs.push(i);
            at = air.runs[i].parent;
        }
        air.terms[t].path = sum;
        let s = sys_index(air.terms[t].sys);
        if sum > air.critical[s].dp {
            air.critical[s] = Crit { dp: sum, term: Some(t), runs };
        }
    }
}

#[cfg(test)]
mod damper_tests {
    use super::{in_wall_height, seg_cross, wall_cross};

    #[test]
    fn a_run_crossing_a_wall_axis_is_found_where_it_crosses() {
        let at = seg_cross((0.0, 0.0, 3.0), (4.0, 0.0, 3.0), (2.0, -1.0), (2.0, 1.0)).unwrap();
        assert!((at.0 - 2.0).abs() < 1e-9 && at.1.abs() < 1e-9 && (at.2 - 3.0).abs() < 1e-9, "{at:?}");
        assert!(seg_cross((0.0, 0.0, 3.0), (1.0, 0.0, 3.0), (2.0, -1.0), (2.0, 1.0)).is_none(), "stops short");
        assert!(seg_cross((0.0, 0.0, 3.0), (4.0, 0.0, 3.0), (0.0, 1.0), (4.0, 1.0)).is_none(), "parallel");
    }

    #[test]
    fn a_duct_above_the_wall_top_passes_over_it() {
        assert!(in_wall_height(2.7, 0.0, 3.0));
        assert!(!in_wall_height(3.4, 0.0, 3.0), "above the soffit");
        assert!(!in_wall_height(-0.5, 0.0, 3.0), "below the floor");
    }

    #[test]
    fn an_arc_wall_is_crossed_on_the_arc_not_on_the_chord() {
        // chord from (-2, 0) to (2, 0), counter-clockwise sweep of 90 degrees: the arc bulges
        // below the chord by bulge * half the chord
        let bulge = (std::f64::consts::FRAC_PI_2 / 4.0).tan();
        let on_chord = wall_cross((0.0, 1.0, 3.0), (0.0, -3.0, 3.0), (-2.0, 0.0), (2.0, 0.0), 0.0);
        assert!((on_chord[0].1).abs() < 1e-9);
        let arc = wall_cross((0.0, 1.0, 3.0), (0.0, -3.0, 3.0), (-2.0, 0.0), (2.0, 0.0), bulge);
        assert_eq!(arc.len(), 1, "{arc:?}");
        let sag = 2.0 * (std::f64::consts::FRAC_PI_2 / 4.0).tan();
        assert!((arc[0].1 + sag).abs() < 1e-6, "crosses at the arc's apex y = {}, got {}", -sag, arc[0].1);
        // a run that passes the chord's line outside the arc's span does not meet the arc
        assert!(wall_cross((5.0, 1.0, 3.0), (5.0, -3.0, 3.0), (-2.0, 0.0), (2.0, 0.0), bulge).is_empty());
        // the mirror bulge puts the arc above the chord
        let up = wall_cross((0.0, 3.0, 3.0), (0.0, -1.0, 3.0), (-2.0, 0.0), (2.0, 0.0), -bulge);
        assert!((up[0].1 - sag).abs() < 1e-6, "{up:?}");
    }
}
