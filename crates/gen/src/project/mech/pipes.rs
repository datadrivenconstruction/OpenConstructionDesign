//! Chilled and heating water from the roof plant to every coil: a header per medium on the
//! roof, a riser per facade bundle at the west end through sleeves in the slabs, a lane per
//! storey along the facade in the ceiling void (radiators: at the floor along the wall), and
//! a branch from the lane to each port. Every piece is sized by its flow within the friction
//! and velocity limits of the rules, with its elbows, tees and reducers; valves at every
//! coil, lane and pump; the pumps sized on the index circuit.

use super::super::rules::rule;
use super::super::GProject;
use super::equip::{plant_plan, Equip, Kind, Medium, Port};
use super::geo::{bbox2, dist, grow, overlap, seg_box, turn, unit, Bx, P3};
use super::select::PlantKind;
use ddc_geom::mep::fittings::{
    contraction_zeta, elbow_zeta, motor_kw, pick_pipe, pipe_friction, shaft_kw, tee_converging, tee_diverging,
    water_rho, CP_WATER, ROUGH_STEEL,
};
use ddc_geom::mep::{PipeMaterial, PipeSize};
use std::f64::consts::PI;

/// Chilled water flow and return, heating water flow and return.
pub const SLOTS: [Medium; 4] = [Medium::ChwFlow, Medium::ChwReturn, Medium::HwFlow, Medium::HwReturn];

pub fn circuit(slot: usize) -> usize {
    slot / 2
}

pub fn flow_side(slot: usize) -> bool {
    slot % 2 == 0
}

/// Flow temperature and design difference of circuit `c` (0 chilled, 1 heating).
pub fn temps(c: usize, hp: bool) -> (f64, f64) {
    match (c, hp) {
        (0, _) => (rule("mep.hvac.chw_supply"), rule("mep.hvac.chw_dt")),
        (_, true) => (rule("mep.hvac.hw_supply_hp"), rule("mep.hvac.hw_dt_hp")),
        _ => (rule("mep.hvac.hw_supply"), rule("mep.hvac.hw_dt")),
    }
}

/// Mean water temperature in a slot's pipes.
fn slot_t(slot: usize, hp: bool) -> f64 {
    let (t, dt) = temps(circuit(slot), hp);
    match (circuit(slot), flow_side(slot)) {
        (_, true) => t,
        (0, false) => t + dt,
        _ => t - dt,
    }
}

/// Water flow, m3/h, carrying `kw` at a difference of `dt` K.
pub fn water_q(kw: f64, t: f64, dt: f64) -> f64 {
    kw * 1000.0 / (water_rho(t) * CP_WATER * dt) * 3600.0
}

/// A coil, radiator or panel the water serves.
#[derive(Clone, Debug, PartialEq)]
pub struct Consumer {
    /// Index in the caller's equipment list.
    pub equip: usize,
    /// `storeys.len()` for the roof.
    pub storey: usize,
    /// 0: the facade at the low y of the plan, 1: at the high y.
    pub side: usize,
    /// Fed from a lane at the floor rather than in the ceiling.
    pub low: bool,
    /// Cooling and heating, kW.
    pub kw: [f64; 2],
    /// Water side drop of the coil at design flow, Pa.
    pub coil_dp: f64,
    pub ports: [Option<P3>; 4],
}

/// The connection points of a unit's slots in a row along plan x around `base`.
pub fn ports_along_x(base: P3, used: [bool; 4]) -> [Option<P3>; 4] {
    let pitch = rule("mep.hvac.port_pitch");
    let m = used.iter().filter(|u| **u).count() as f64;
    let mut out = [None; 4];
    let mut j = 0.0;
    for s in 0..4 {
        if used[s] {
            out[s] = Some((base.0 + (j - (m - 1.0) / 2.0) * pitch, base.1, base.2));
            j += 1.0;
        }
    }
    out
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum PClass {
    Plant,
    Riser,
    Main,
    Branch,
}

impl PClass {
    pub fn name(self) -> &'static str {
        match self {
            PClass::Plant => "plant",
            PClass::Riser => "riser",
            PClass::Main => "main",
            PClass::Branch => "branch",
        }
    }
}

/// One straight or bent piece of pipe, points from the plant side.
#[derive(Clone, Debug, PartialEq)]
pub struct Pipe {
    pub slot: usize,
    pub storey: usize,
    pub class: PClass,
    pub points: Vec<P3>,
    pub parent: Option<usize>,
    pub consumer: Option<usize>,
    /// m3/h, m/s, Pa/m.
    pub q: f64,
    pub v: f64,
    pub r: f64,
    pub dn: u32,
    pub od: f64,
    pub id: f64,
    pub ins: f64,
    pub len: f64,
    /// Pa: elbows along it and the fitting where it leaves its parent.
    pub fittings: f64,
}

impl Pipe {
    pub fn dp(&self) -> f64 {
        self.r * self.len + self.fittings
    }
    pub fn boxes(&self) -> Vec<Bx> {
        let h = self.od / 2.0 + self.ins;
        self.points.windows(2).map(|w| seg_box(w[0], w[1], h, h)).collect()
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum VKind {
    Isolation,
    Balancing,
    Control,
    Check,
}

impl VKind {
    pub fn name(self) -> &'static str {
        match self {
            VKind::Isolation => "isolation",
            VKind::Balancing => "balancing",
            VKind::Control => "control",
            VKind::Check => "check",
        }
    }
}

#[derive(Clone, Debug, PartialEq)]
pub struct Valve {
    pub kind: VKind,
    pub slot: usize,
    pub at: P3,
    pub dn: u32,
    pub dp: f64,
}

#[derive(Clone, Debug, PartialEq)]
pub struct PFit {
    /// `elbow`, `tee`, `reducer`.
    pub kind: &'static str,
    pub slot: usize,
    pub at: P3,
    pub dn: u32,
    pub zeta: f64,
    pub dp: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Hydro {
    pub pipes: Vec<Pipe>,
    pub valves: Vec<Valve>,
    pub fittings: Vec<PFit>,
    pub plant: Vec<Equip>,
    pub pumps: Vec<Equip>,
    /// Where a riser passes a slab: point and DN.
    pub sleeves: Vec<(P3, u32)>,
    /// Per circuit: flow m3/h, index circuit drop Pa, its consumer.
    pub flow: [f64; 2],
    pub index: [f64; 2],
    pub index_of: [Option<usize>; 2],
    /// Per consumer and slot: the pipe that reaches it.
    pub leaf: Vec<[Option<usize>; 4]>,
    pub faults: Vec<String>,
}

fn ins(slot: usize, roof: bool) -> f64 {
    match (circuit(slot), roof) {
        (_, true) => rule("mep.hvac.ins_outdoor"),
        (0, false) => rule("mep.hvac.ins_chw"),
        _ => rule("mep.hvac.ins_hw"),
    }
}

fn sizes() -> Vec<PipeSize> {
    PipeMaterial::Steel.sizes()
}

fn size_for(q: f64, t: f64) -> PipeSize {
    let split = rule("mep.hvac.pipe_dn_split") as u32;
    let (vs, vl) = (rule("mep.hvac.pipe_v_small"), rule("mep.hvac.pipe_v_large"));
    pick_pipe(q, t, rule("mep.hvac.pipe_r_max"), |dn| if dn <= split { vs } else { vl }, &sizes(), rule("mep.hvac.pipe_dn_min") as u32)
}

fn dedup(mut v: Vec<P3>) -> Vec<P3> {
    v.dedup_by(|a, b| dist(*a, *b) < 1e-6);
    // drop points in the middle of a straight line
    let mut out: Vec<P3> = vec![];
    for q in v {
        if out.len() >= 2 {
            let (a, b) = (out[out.len() - 2], out[out.len() - 1]);
            if turn(unit(a, b), unit(b, q)) < 1e-3 {
                out.pop();
            }
        }
        out.push(q);
    }
    out
}

struct Ctx<'a> {
    obst: &'a [(Bx, String)],
    equip: &'a [Equip],
    gap: f64,
}

impl Ctx<'_> {
    /// What a polyline of half size `h` would hit, ignoring the equipment in `skip`.
    fn hits(&self, pts: &[P3], h: f64, skip: &[usize], placed: &[(usize, Bx)], skip_pipe: &[usize]) -> Option<String> {
        for w in pts.windows(2) {
            let b = seg_box(w[0], w[1], h, h);
            if let Some(o) = self.obst.iter().find(|o| overlap(&b, &o.0, self.gap)) {
                return Some(o.1.clone());
            }
            for (i, e) in self.equip.iter().enumerate() {
                if !skip.contains(&i) && overlap(&b, &e.bbox(), self.gap) {
                    return Some(format!("{} {}", e.kind.name(), e.tag));
                }
            }
            if let Some((i, _)) = placed.iter().find(|(i, pb)| !skip_pipe.contains(i) && overlap(&b, pb, self.gap)) {
                return Some(format!("pipe {i}"));
            }
        }
        None
    }
}

/// The plant on the roof: units by kind and duty, in a row along y at the first clear place
/// from the west end, their ports on the west face.
fn place_plant(p: &GProject, kind: PlantKind, kw: [f64; 2], cx: &Ctx, out: &mut Hydro) -> Vec<(usize, P3)> {
    let n = p.storeys.len();
    let rb = bbox2(&p.roof.ring);
    let roof = p.roof.elevation;
    let mut units: Vec<(Kind, f64, f64, f64, Vec<usize>)> = vec![];
    let hgt = |k: &str| rule(&format!("mep.hvac.{k}_h"));
    match kind {
        PlantKind::HeatPump => {
            let m2 = rule("mep.hvac.hp_m2_kw");
            let slots: Vec<usize> = (0..4).filter(|s| kw[circuit(*s)] > 0.0).collect();
            units.push((Kind::HeatPump, kw[0].max(kw[1]), m2, hgt("hp"), slots));
        }
        PlantKind::ChillerBoiler => {
            if kw[0] > 0.0 {
                units.push((Kind::Chiller, kw[0], rule("mep.hvac.chiller_m2_kw"), hgt("chiller"), vec![0, 1]));
            }
            if kw[1] > 0.0 {
                units.push((Kind::Boiler, kw[1], rule("mep.hvac.boiler_m2_kw"), hgt("boiler"), vec![2, 3]));
            }
        }
    }
    let pitch = rule("mep.hvac.port_pitch");
    let plans: Vec<(f64, f64)> = units
        .iter()
        .map(|u| {
            let (l, w) = plant_plan(u.1, u.2);
            (l, w.max(u.4.len() as f64 * pitch + pitch))
        })
        .collect();
    let clear = rule("mep.hvac.plant_clear");
    let total_w: f64 = plans.iter().map(|q| q.1).sum::<f64>() + clear * (plans.len().max(1) - 1) as f64;
    let len_max = plans.iter().map(|q| q.0).fold(0.0, f64::max);
    let ymid = (rb[1] + rb[3]) / 2.0;
    let edge = rule("mep.hvac.plant_edge");
    let step = rule("mep.hvac.plant_step");
    let mut x = rb[0] + edge + rule("mep.hvac.riser_off") + rule("mep.hvac.plant_header");
    let mut found = None;
    while x + len_max <= rb[2] - edge + 1e-9 {
        let b = [x, ymid - total_w / 2.0, roof, x + len_max, ymid + total_w / 2.0, roof + units.iter().map(|u| u.3).fold(0.0, f64::max)];
        let free = !cx.obst.iter().any(|o| overlap(&b, &o.0, clear))
            && !cx.equip.iter().any(|e| overlap(&b, &e.bbox(), clear))
            && !p.storeys.last().is_some_and(|st| st.layout.shafts.iter().any(|s| {
                let sb = bbox2(&s.ring);
                overlap(&b, &[sb[0], sb[1], roof, sb[2], sb[3], roof + 1.0], clear)
            }));
        if free {
            found = Some(x);
            break;
        }
        x += step;
    }
    let Some(x0) = found else {
        out.faults.push("no clear place on the roof for the plant".into());
        return vec![];
    };
    let mut ports = vec![];
    let mut y = ymid - total_w / 2.0;
    let pz = rule("mep.hvac.pipe_roof_z");
    for (u, (l, w)) in units.iter().zip(&plans) {
        let mut e = Equip::new(u.0, n, (x0 + l / 2.0, y + w / 2.0, roof), 0.0, (*l, *w, u.3));
        let m = u.4.len() as f64;
        for (j, &s) in u.4.iter().enumerate() {
            let at = e.local(-l / 2.0, (j as f64 - (m - 1.0) / 2.0) * pitch, pz);
            e.ports.push(Port { name: SLOTS[s].name(), medium: SLOTS[s], at, dir: e.dir(-1.0, 0.0, 0.0), size: 0.0, flow: 0.0 });
            ports.push((s, at));
        }
        e.duty = vec![("kw", u.1)];
        e.tag = format!("{}{}", u.0.tag(), out.plant.len() + 1);
        out.plant.push(e);
        y += w + clear;
    }
    ports
}

/// The water network for `cons`: geometry, sizes, fittings, valves, pumps and the index circuit.
pub fn plan(p: &GProject, cons: &[Consumer], equip: &[Equip], obst: &[(Bx, String)], kind: PlantKind) -> Hydro {
    let mut out = Hydro { leaf: vec![[None; 4]; cons.len()], ..Default::default() };
    if cons.is_empty() {
        return out;
    }
    let hp = kind == PlantKind::HeatPump;
    let n = p.storeys.len();
    let cx = Ctx { obst, equip, gap: rule("services.gap") };
    let div = rule("mep.hvac.diversity");
    let kw = [0, 1].map(|c| cons.iter().map(|q| q.kw[c]).sum::<f64>() * div);
    let qc = |ci: usize, slot: usize| {
        let (t, dt) = temps(circuit(slot), hp);
        water_q(cons[ci].kw[circuit(slot)], t, dt)
    };
    let used: [bool; 4] = [0, 1, 2, 3].map(|s| cons.iter().any(|c| c.ports[s].is_some() && c.kw[circuit(s)] > 0.0));
    // the lane pitch from the largest pipe a slot can need
    let mut pitch: f64 = 0.0;
    for s in (0..4).filter(|s| used[*s]) {
        let all: f64 = (0..cons.len()).map(|i| qc(i, s)).sum();
        let ps = size_for(all, slot_t(s, hp));
        pitch = pitch.max(ps.od + 2.0 * ins(s, true));
    }
    pitch += rule("mep.hvac.pipe_gap");
    let ports = place_plant(p, kind, kw, &cx, &mut out);
    if ports.is_empty() {
        return out;
    }
    let roof = p.roof.elevation;
    let rb = bbox2(&p.roof.ring);
    let stub = rule("mep.hvac.stub");
    let zr = |s: usize| roof + rule("mep.hvac.pipe_roof_z") + s as f64 * pitch;
    // bundles: (side, low) of the storey consumers
    let mut bundles: Vec<(usize, bool, Vec<usize>)> = vec![];
    for (i, c) in cons.iter().enumerate().filter(|(_, c)| c.storey < n) {
        match bundles.iter_mut().find(|b| b.0 == c.side && b.1 == c.low) {
            Some(b) => b.2.push(i),
            None => bundles.push((c.side, c.low, vec![i])),
        }
    }
    // global riser index per (bundle, slot)
    let mut g_of: Vec<[Option<usize>; 4]> = vec![[None; 4]; bundles.len()];
    let mut g = 0;
    for (bi, b) in bundles.iter().enumerate() {
        for s in 0..4 {
            if b.2.iter().any(|&i| cons[i].ports[s].is_some() && cons[i].kw[circuit(s)] > 0.0) {
                g_of[bi][s] = Some(g);
                g += 1;
            }
        }
    }
    let lane_y = |side: usize, low: bool, j: usize, k: usize| {
        let sb = bbox2(&p.storeys[k].layout.slab);
        let off = if low { crate::WallRole::Exterior.thickness() + rule("mep.hvac.low_lane_off") } else { rule("mep.hvac.pipe_facade_off") };
        if side == 0 {
            sb[1] + off + j as f64 * pitch
        } else {
            sb[3] - off - j as f64 * pitch
        }
    };
    // the riser x clear of the structure on every storey
    let mut xr = rb[0] + rule("mep.hvac.riser_off");
    let solids_hit = |x0: f64| {
        bundles.iter().enumerate().any(|(bi, b)| {
            (0..4).filter_map(|s| g_of[bi][s].map(|g| (s, g))).enumerate().any(|(j, (s, g))| {
                let x = x0 + g as f64 * pitch;
                let y = lane_y(b.0, b.1, j, 0);
                let h = pitch / 2.0;
                let bx = [x - h, y - h, p.storeys[0].elevation, x + h, y + h, zr(s)];
                obst.iter().any(|o| overlap(&bx, &o.0, cx.gap))
            })
        })
    };
    while solids_hit(xr) && xr < rb[2] {
        xr += rule("mep.hvac.plant_step");
    }
    let mut pipes: Vec<Pipe> = vec![];
    let push = |pipes: &mut Vec<Pipe>, slot: usize, storey: usize, class: PClass, pts: Vec<P3>, parent: Option<usize>| {
        pipes.push(Pipe {
            slot,
            storey,
            class,
            points: dedup(pts),
            parent,
            consumer: None,
            q: 0.0,
            v: 0.0,
            r: 0.0,
            dn: 0,
            od: 0.0,
            id: 0.0,
            ins: 0.0,
            len: 0.0,
            fittings: 0.0,
        });
        pipes.len() - 1
    };
    // roof: per slot the stub from the port, a header west, a leg to each bundle's riser
    let mut riser_top: Vec<[Option<usize>; 4]> = vec![[None; 4]; bundles.len()];
    let mut corner: [Option<(usize, P3)>; 4] = [None; 4];
    for s in (0..4).filter(|s| used[*s]) {
        let Some(&(_, at)) = ports.iter().find(|q| q.0 == s) else { continue };
        let c0 = (at.0 - stub, at.1, zr(s));
        let st = push(&mut pipes, s, n, PClass::Plant, vec![at, (at.0 - stub, at.1, at.2), c0], None);
        corner[s] = Some((st, c0));
        let mut legs: Vec<(usize, f64)> = (0..bundles.len()).filter_map(|bi| g_of[bi][s].map(|g| (bi, xr + g as f64 * pitch))).collect();
        legs.sort_by(|a, b| b.1.total_cmp(&a.1));
        let mut prev = (st, c0);
        for (bi, x) in legs {
            let tee = (x, at.1, zr(s));
            let h = push(&mut pipes, s, n, PClass::Plant, vec![prev.1, tee], Some(prev.0));
            let j = (0..s).filter(|t| g_of[bi][*t].is_some()).count();
            let b = &bundles[bi];
            let leg = push(&mut pipes, s, n, PClass::Plant, vec![tee, (x, lane_y(b.0, b.1, j, n - 1), zr(s))], Some(h));
            riser_top[bi][s] = Some(leg);
            prev = (h, tee);
        }
    }
    // roof consumers: up from the header corner over the roof plant, across, down to the port
    let tall = equip.iter().filter(|e| e.storey == n).map(|e| e.at.2 + e.size.2).chain(out.plant.iter().map(|e| e.at.2 + e.size.2)).fold(roof, f64::max);
    for (ci, c) in cons.iter().enumerate().filter(|(_, c)| c.storey >= n) {
        for s in 0..4 {
            let (Some(q), Some((par, c0))) = (c.ports[s], corner[s]) else { continue };
            if c.kw[circuit(s)] <= 0.0 {
                continue;
            }
            let z = tall + cx.gap + pitch * (s as f64 + 1.0);
            let pts = vec![c0, (c0.0, c0.1, z), (q.0, c0.1, z), (q.0, q.1, z), q];
            let i = push(&mut pipes, s, n, PClass::Branch, pts, Some(par));
            pipes[i].consumer = Some(ci);
            out.leaf[ci][s] = Some(i);
        }
    }
    // risers and storey lanes
    let soffit = |k: usize| super::geo::soffit(p, k);
    for (bi, b) in bundles.iter().enumerate() {
        let slots: Vec<usize> = (0..4).filter(|s| g_of[bi][*s].is_some()).collect();
        let mut storeys: Vec<usize> = b.2.iter().map(|&i| cons[i].storey).collect();
        storeys.sort_unstable();
        storeys.dedup();
        storeys.reverse();
        let mut above: Vec<Option<(usize, P3)>> = slots.iter().map(|&s| riser_top[bi][s].map(|i| (i, *pipes[i].points.last().expect("points")))).collect();
        for &k in &storeys {
            let mine: Vec<usize> = b.2.iter().copied().filter(|&i| cons[i].storey == k).collect();
            let xs: Vec<f64> = mine.iter().flat_map(|&i| cons[i].ports.iter().flatten().map(|q| q.0)).collect();
            let xg = |j: usize| xr + g_of[bi][slots[j]].unwrap_or(0) as f64 * pitch;
            let x_lo = xs.iter().copied().fold(xg(0), f64::min) - stub;
            let x_hi = xs.iter().copied().fold(xg(slots.len() - 1), f64::max) + stub;
            let h = pitch / 2.0;
            // lane level: the highest clear of everything below the slab
            let zl = if b.1 {
                p.storeys[k].elevation + rule("mep.hvac.low_lane_z")
            } else {
                let mut z = soffit(k) - cx.gap - h;
                let floor = super::geo::ceiling(p, k) + h;
                // layer table: no lower than the ceiling face allows
                let lane = if slots.iter().any(|&s| s < 2) { "chilled" } else { "heating" };
                if let Some(d) = super::super::svc::depth(lane) {
                    z = z.min(soffit(k) - d).max(floor);
                }
                let clear = |z: f64| {
                    slots.iter().enumerate().all(|(j, _)| {
                        let y = lane_y(b.0, b.1, j, k);
                        cx.hits(&[(x_lo, y, z), (x_hi, y, z)], h, &[], &[], &[]).is_none()
                    })
                };
                while !clear(z) && z - rule("mep.hvac.lane_step") >= floor {
                    z -= rule("mep.hvac.lane_step");
                }
                if !clear(z) {
                    out.faults.push(format!("storey {k}: no clear level for the pipe lane on side {}", b.0));
                }
                z
            };
            for (j, &s) in slots.iter().enumerate() {
                let y = lane_y(b.0, b.1, j, k);
                let x = xg(j);
                let Some((par, top)) = above[j] else { continue };
                let rp = (x, y, zl);
                let riser = push(&mut pipes, s, k, PClass::Riser, vec![top, (x, y, top.2), rp], Some(par));
                above[j] = Some((riser, rp));
                // taps on the lane, by x either side of the riser
                let mut taps: Vec<(f64, usize)> = mine.iter().filter_map(|&i| cons[i].ports[s].filter(|_| cons[i].kw[circuit(s)] > 0.0).map(|q| (q.0, i))).collect();
                taps.sort_by(|a, c| a.0.total_cmp(&c.0));
                for dir in [-1.0, 1.0] {
                    let mut side: Vec<(f64, usize)> = taps.iter().copied().filter(|t| (t.0 - x) * dir > 0.0 || (dir > 0.0 && (t.0 - x).abs() < 1e-9)).collect();
                    if dir < 0.0 {
                        side.reverse();
                    }
                    let mut prev = (riser, rp);
                    for (tx, ci) in side {
                        let tp = (tx, y, zl);
                        let seg = push(&mut pipes, s, k, PClass::Main, vec![prev.1, tp], Some(prev.0));
                        let q = cons[ci].ports[s].expect("tap port");
                        let br = push(&mut pipes, s, k, PClass::Branch, vec![tp, (tx, y, q.2), q], Some(seg));
                        pipes[br].consumer = Some(ci);
                        out.leaf[ci][s] = Some(br);
                        prev = (seg, tp);
                    }
                }
            }
        }
    }
    // flows: each leaf carries its consumer, each parent the sum of its children
    for i in 0..pipes.len() {
        if let Some(ci) = pipes[i].consumer {
            let q = qc(ci, pipes[i].slot);
            let mut at = Some(i);
            while let Some(j) = at {
                pipes[j].q += q;
                at = pipes[j].parent;
            }
        }
    }
    for pp in pipes.iter_mut() {
        let t = slot_t(pp.slot, hp);
        let ps = size_for(pp.q, t);
        let (v, r) = pipe_friction(pp.q, ps.id(), t, ROUGH_STEEL);
        pp.dn = ps.dn;
        pp.od = ps.od;
        pp.id = ps.id();
        pp.v = v;
        pp.r = r;
        pp.ins = ins(pp.slot, pp.storey >= n);
        pp.len = super::geo::len(&pp.points);
    }
    // branches re-routed below the lane where the straight drop hits something
    route_branches(p, &mut pipes, &cx, &mut out);
    for pp in pipes.iter_mut() {
        pp.len = super::geo::len(&pp.points);
    }
    fittings(&mut pipes, hp, &mut out.fittings);
    for i in 0..pipes.len() {
        let pp = &pipes[i];
        let lim = if pp.dn <= rule("mep.hvac.pipe_dn_split") as u32 { rule("mep.hvac.pipe_v_small") } else { rule("mep.hvac.pipe_v_large") };
        if pp.v > lim + 1e-9 {
            out.faults.push(format!("{} DN{} {}: {:.2} m/s over {:.2}", SLOTS[pp.slot].name(), pp.dn, pp.class.name(), pp.v, lim));
        }
    }
    // sleeves where a riser passes a slab, the roof included
    for pp in pipes.iter().filter(|pp| pp.class == PClass::Riser || (pp.class == PClass::Plant && pp.storey >= n)) {
        for w in pp.points.windows(2) {
            let (lo, hi) = (w[0].2.min(w[1].2), w[0].2.max(w[1].2));
            for z in p.storeys.iter().skip(1).map(|s| s.elevation).chain([roof]) {
                if (w[0].0 - w[1].0).abs() < 1e-9 && (w[0].1 - w[1].1).abs() < 1e-9 && lo < z - super::super::svc::slab() && hi > z {
                    out.sleeves.push(((w[0].0, w[0].1, z), pp.dn));
                }
            }
        }
    }
    valves_and_pumps(cons, &pipes, hp, &mut out);
    out.pipes = pipes;
    clashes(&cx, &mut out);
    out
}

/// A branch drops at its tap to its port level and runs across; where that hits a duct or
/// the structure it drops lower first, in steps down to the ceiling, and climbs at the port.
fn route_branches(p: &GProject, pipes: &mut [Pipe], cx: &Ctx, out: &mut Hydro) {
    let n = p.storeys.len();
    let step = rule("mep.hvac.lane_step");
    let mut placed: Vec<(usize, Bx)> = vec![];
    for (i, pp) in pipes.iter().enumerate() {
        if pp.class != PClass::Branch {
            placed.extend(pp.boxes().into_iter().map(|b| (i, b)));
        }
    }
    for i in 0..pipes.len() {
        if pipes[i].class != PClass::Branch {
            continue;
        }
        let (storey, slot) = (pipes[i].storey, pipes[i].slot);
        let h = pipes[i].od / 2.0 + pipes[i].ins;
        let own: Vec<usize> = pipes[i].consumer.into_iter().collect();
        let (tp, q) = (pipes[i].points[0], *pipes[i].points.last().expect("points"));
        // the pieces joined at the tap touch it by design
        let skip: Vec<usize> = (0..pipes.len()).filter(|&j| j == i || pipes[j].points.iter().any(|x| dist(*x, tp) < 5e-3)).collect();
        if storey >= n {
            if let Some(what) = cx.hits(&pipes[i].points, h, &own, &placed, &skip) {
                out.faults.push(format!("roof {} branch hits {what}", SLOTS[slot].name()));
            }
            placed.extend(pipes[i].boxes().into_iter().map(|b| (i, b)));
            continue;
        }
        let floor = if q.2 < tp.2 { super::geo::ceiling(p, storey).min(q.2) - step } else { p.storeys[storey].elevation };
        let mut best = None;
        let mut zb = q.2;
        while zb >= floor - 1e-9 {
            let pts = dedup(vec![tp, (tp.0, tp.1, zb), (q.0, q.1, zb), q]);
            if cx.hits(&pts, h, &own, &placed, &skip).is_none() {
                best = Some(pts);
                break;
            }
            zb -= step;
        }
        match best {
            Some(pts) => pipes[i].points = pts,
            None => {
                let what = cx.hits(&pipes[i].points, h, &own, &placed, &skip).unwrap_or_default();
                out.faults.push(format!("storey {storey}: {} branch to ({:.2}, {:.2}) hits {what}", SLOTS[slot].name(), q.0, q.1));
            }
        }
        placed.extend(pipes[i].boxes().into_iter().map(|b| (i, b)));
    }
}

/// Elbows along each piece; at each joint a tee (by the velocity ratio) or a reducer.
fn fittings(pipes: &mut [Pipe], hp: bool, out: &mut Vec<PFit>) {
    let r_d = rule("mep.hvac.pipe_bend");
    let a = rule("mep.hvac.takeoff_a");
    let half = rule("mep.hvac.pipe_reducer_angle");
    let mut kids: Vec<Vec<usize>> = vec![vec![]; pipes.len()];
    for (i, pp) in pipes.iter().enumerate() {
        if let Some(q) = pp.parent {
            kids[q].push(i);
        }
    }
    for i in 0..pipes.len() {
        let pp = &pipes[i];
        let pd = water_rho(slot_t(pp.slot, hp)) * pp.v * pp.v / 2.0;
        let mut loss = 0.0;
        for k in 1..pp.points.len().saturating_sub(1) {
            let ang = turn(unit(pp.points[k - 1], pp.points[k]), unit(pp.points[k], pp.points[k + 1]));
            if ang > 1e-3 {
                let z = elbow_zeta(r_d, ang);
                loss += z * pd;
                out.push(PFit { kind: "elbow", slot: pp.slot, at: pp.points[k], dn: pp.dn, zeta: z, dp: z * pd });
            }
        }
        pipes[i].fittings = loss;
    }
    for pi in 0..pipes.len() {
        let ks = kids[pi].clone();
        if ks.is_empty() {
            continue;
        }
        let pr = pipes[pi].clone();
        let m = pr.points.len();
        let din = unit(pr.points[m - 2], pr.points[m - 1]);
        let start = |c: &Pipe| unit(c.points[0], c.points[1]);
        let rho = water_rho(slot_t(pr.slot, hp));
        let pd = rho * pr.v * pr.v / 2.0;
        let flow = flow_side(pr.slot);
        if ks.len() == 1 {
            let c = &pipes[ks[0]];
            let ang = turn(din, start(c));
            let (kind, z, dpz) = if ang > 1e-3 {
                let z = elbow_zeta(r_d, ang);
                ("elbow", z, z * pd.max(rho * c.v * c.v / 2.0))
            } else if c.dn != pr.dn {
                let (a1, a2) = (PI * pr.id * pr.id / 4.0, PI * c.id * c.id / 4.0);
                let z = contraction_zeta(a1.max(a2), a1.min(a2), half);
                ("reducer", z, z * rho * pr.v.max(c.v).powi(2) / 2.0)
            } else {
                continue;
            };
            out.push(PFit { kind, slot: pr.slot, at: pr.points[m - 1], dn: c.dn, zeta: z, dp: dpz });
            pipes[ks[0]].fittings += dpz;
            continue;
        }
        let straight = ks.iter().copied().find(|&c| turn(din, start(&pipes[c])) < 1e-3);
        let vs = straight.map_or(0.0, |s| pipes[s].v);
        let qb: f64 = ks.iter().filter(|&&c| Some(c) != straight).map(|&c| pipes[c].q).sum();
        for &c in &ks {
            let cp = &pipes[c];
            let is_s = Some(c) == straight;
            let z = if flow {
                let (zb, zs) = tee_diverging(pr.v, cp.v, vs, a);
                if is_s { zs } else { zb }
            } else if is_s {
                tee_converging(pr.v, 0.0, qb / pr.q.max(1e-9), a).1
            } else {
                tee_converging(pr.v, cp.v, cp.q / pr.q.max(1e-9), a).0
            };
            let dpz = z * pd;
            pipes[c].fittings += dpz;
            if !is_s {
                out.push(PFit { kind: "tee", slot: pr.slot, at: pr.points[m - 1], dn: pr.dn, zeta: z, dp: dpz });
            }
        }
    }
}

/// Valves at every coil and lane, the index circuit of each water system and its pump.
fn valves_and_pumps(cons: &[Consumer], pipes: &[Pipe], hp: bool, out: &mut Hydro) {
    let auth = rule("mep.hvac.valve_authority");
    let bal = rule("mep.hvac.balancing_dp");
    let path = |i: usize| {
        let mut sum = 0.0;
        let mut at = Some(i);
        while let Some(j) = at {
            sum += pipes[j].dp();
            at = pipes[j].parent;
        }
        sum
    };
    for (ci, c) in cons.iter().enumerate() {
        for circ in 0..2 {
            let (Some(f), Some(r)) = (out.leaf[ci][2 * circ], out.leaf[ci][2 * circ + 1]) else { continue };
            let ctrl = auth / (1.0 - auth) * c.coil_dp;
            let pf = &pipes[f];
            let pr = &pipes[r];
            let end = |p: &Pipe| *p.points.last().expect("points");
            out.valves.push(Valve { kind: VKind::Isolation, slot: 2 * circ, at: end(pf), dn: pf.dn, dp: 0.0 });
            out.valves.push(Valve { kind: VKind::Isolation, slot: 2 * circ + 1, at: end(pr), dn: pr.dn, dp: 0.0 });
            out.valves.push(Valve { kind: VKind::Control, slot: 2 * circ + 1, at: end(pr), dn: pr.dn, dp: ctrl });
            out.valves.push(Valve { kind: VKind::Balancing, slot: 2 * circ + 1, at: end(pr), dn: pr.dn, dp: bal });
            let dp = path(f) + path(r) + c.coil_dp + ctrl + bal + rule("mep.hvac.plant_dp");
            if dp > out.index[circ] {
                out.index[circ] = dp;
                out.index_of[circ] = Some(ci);
            }
        }
    }
    // a pair of valves where each storey lane leaves its riser
    for pp in pipes.iter().filter(|pp| pp.class == PClass::Riser) {
        let at = *pp.points.last().expect("points");
        out.valves.push(Valve { kind: VKind::Isolation, slot: pp.slot, at, dn: pp.dn, dp: 0.0 });
        if !flow_side(pp.slot) {
            out.valves.push(Valve { kind: VKind::Balancing, slot: pp.slot, at, dn: pp.dn, dp: bal });
        }
    }
    let (eff, margin) = (rule("mep.hvac.pump_eff"), rule("mep.hvac.pump_margin"));
    for circ in 0..2 {
        let Some(head) = pipes.iter().find(|pp| pp.slot == 2 * circ && pp.parent.is_none()) else { continue };
        out.flow[circ] = head.q;
        let dp = out.index[circ] * (1.0 + margin);
        let kw = shaft_kw(head.q, dp, eff);
        let (a, b) = (head.points[0], head.points[1]);
        let d = unit(a, b);
        let l = rule("mep.hvac.pump_len");
        let mid = ((a.0 + b.0) / 2.0, (a.1 + b.1) / 2.0, a.2 - head.od);
        let mut e = Equip::new(Kind::Pump, head.storey, mid, d.1.atan2(d.0), (l, head.od * 2.0, head.od * 2.0));
        let half = l / 2.0;
        e.ports = vec![
            Port { name: "Suction", medium: SLOTS[2 * circ], at: e.local(-half, 0.0, head.od), dir: e.dir(-1.0, 0.0, 0.0), size: head.id, flow: head.q },
            Port { name: "Discharge", medium: SLOTS[2 * circ], at: e.local(half, 0.0, head.od), dir: e.dir(1.0, 0.0, 0.0), size: head.id, flow: head.q },
        ];
        e.duty = vec![("flow_m3h", head.q), ("head_kpa", dp / 1000.0), ("shaft_kw", kw), ("motor_kw", motor_kw(kw * (1.0 + margin)))];
        e.tag = format!("{}{}", Kind::Pump.tag(), circ + 1);
        for kind in [VKind::Isolation, VKind::Check, VKind::Isolation] {
            out.valves.push(Valve { kind, slot: 2 * circ, at: a, dn: head.dn, dp: 0.0 });
        }
        out.pumps.push(e);
    }
}

/// Every piece of pipe against the structure, ducts, equipment and the other pipes; joined
/// pieces and the unit a branch ends at are not clashes.
fn clashes(cx: &Ctx, out: &mut Hydro) {
    let ends = |p: &Pipe| [p.points[0], *p.points.last().expect("points")];
    let near = |a: &Pipe, b: &Pipe| {
        ends(a).iter().any(|x| b.points.iter().any(|y| dist(*x, *y) < 5e-3))
            || ends(b).iter().any(|x| a.points.iter().any(|y| dist(*x, *y) < 5e-3))
    };
    let pipes = &out.pipes;
    let mut found = vec![];
    for (i, a) in pipes.iter().enumerate() {
        let h = a.od / 2.0 + a.ins;
        let own: Vec<usize> = a.consumer.into_iter().collect();
        if let Some(what) = cx.hits(&a.points, h, &own, &[], &[]) {
            found.push(format!("{} {} pipe {i} hits {what}", SLOTS[a.slot].name(), a.class.name()));
        }
        for (j, b) in pipes.iter().enumerate().skip(i + 1) {
            if near(a, b) {
                continue;
            }
            let hit = a.boxes().iter().any(|x| b.boxes().iter().any(|y| overlap(x, y, 0.0)));
            if hit {
                found.push(format!("pipes {i} and {j} cross"));
            }
        }
        for e in out.plant.iter().chain(&out.pumps) {
            if a.boxes().iter().any(|x| overlap(x, &grow(&e.bbox(), -cx.gap), 0.0)) && !a.points.iter().any(|q| e.ports.iter().any(|pt| dist(*q, pt.at) < 5e-3)) && e.kind != Kind::Pump {
                found.push(format!("pipe {i} passes through {}", e.tag));
            }
        }
    }
    out.faults.extend(found);
}
