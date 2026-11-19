// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Preliminary structural check and member sizing of long-span lattices (bridges, roofs,
//! space frames). The caller builds a [`Lat`] (nodes, members by group, supports, loads of
//! three cases: permanent, imposed, wind), [`design`] solves it with `ddc-fem`, steps every
//! group of members up or down a section ladder until each member is under the utilisation
//! rule, then stiffens the named groups until deflection and (for footbridges) the first
//! vertical frequency pass. Every number that steers this is a `span.*` rules of `rules.json` entry with a
//! range and a reason; a script may override a value inside its range. Results are a
//! preliminary design aid, not a design to any code.

use std::collections::{HashMap, HashSet};
use std::f64::consts::PI;
use web_time::Instant;

use ddc_fem::{solve, Combo, Frame, Load, LoadCase, Material, Member, Options, Restraint, Section, Support};

use crate::values::V3;

const E: f64 = 210e9;
const G: f64 = 81e9;
const DENSITY: f64 = 7850.0;
const GRAV: f64 = 9.81;
/// Longest the sizing may run, s; it then keeps the sections reached.
pub(crate) const BUDGET: f64 = 30.0;
/// Analysis time one script run may spend in all its checks together; it is also what the CPU limit forgives.
pub(crate) const RUN_CAP: f64 = 30.0;
/// Rounds of stiffening spent on the frequency alone.
const FREQ_STEPS: usize = 4;

/// One steering value: id, default, allowed range, unit, and the reason it exists.
pub(crate) struct Rule {
    pub id: String,
    pub value: f64,
    pub lo: f64,
    pub hi: f64,
    pub unit: String,
    pub why: String,
}

/// The `span.*` entries of the generator's rule file: that file is where every rule, its
/// range, unit and reason (English here, English and Russian in the UI text) is kept.
pub(crate) fn rules() -> &'static [Rule] {
    static R: std::sync::OnceLock<Vec<Rule>> = std::sync::OnceLock::new();
    R.get_or_init(|| {
        let v: serde_json::Value = serde_json::from_str(include_str!("../../gen/data/rules.json")).expect("rules.json");
        let num = |d: &serde_json::Value, k: &str| d[k].as_f64().unwrap_or_else(|| panic!("rules.json: {d} has no {k}"));
        v["rules"]
            .as_array()
            .expect("rules.json: rules")
            .iter()
            .filter_map(|d| {
                let id = d["id"].as_str()?.strip_prefix("span.")?;
                Some(Rule {
                    id: id.to_string(),
                    value: num(d, "default"),
                    lo: num(d, "min"),
                    hi: num(d, "max"),
                    unit: d["unit"].as_str().unwrap_or("").to_string(),
                    why: d["why"].as_str().unwrap_or("").to_string(),
                })
            })
            .collect()
    })
}

/// Rule values with the script's overrides applied.
pub(crate) struct Rules(HashMap<String, f64>);

impl Rules {
    pub fn new() -> Rules {
        Rules(rules().iter().map(|r| (r.id.clone(), r.value)).collect())
    }

    /// Sets `id` to `v` when `id` is a rule and `v` is inside its range.
    pub fn set(&mut self, id: &str, v: f64) -> Result<(), String> {
        let Some(r) = rules().iter().find(|r| r.id == id) else {
            let all: Vec<&str> = rules().iter().map(|r| r.id.as_str()).collect();
            return Err(format!("`{id}` is not a rule (rules: {})", all.join(", ")));
        };
        if !(v.is_finite() && v >= r.lo && v <= r.hi) {
            return Err(format!("rule `{id}` = {v} is outside its range {} to {} {}", r.lo, r.hi, r.unit));
        }
        self.0.insert(r.id.clone(), v);
        Ok(())
    }

    pub fn get(&self, id: &str) -> f64 {
        self.0.get(id).copied().unwrap_or_else(|| rules().iter().find(|r| r.id == id).map_or(0.0, |r| r.value))
    }
}

pub(crate) fn why_of(id: &str) -> &'static str {
    rules().iter().find(|r| r.id == id).map_or("", |r| r.why.as_str())
}

// ---- sections ----

/// A section of a ladder: its text for `prof_text`, constants in metres.
#[derive(Clone, Debug)]
pub(crate) struct Sec {
    pub text: String,
    pub a: f64,
    pub iy: f64,
    pub iz: f64,
    pub j: f64,
    pub wy: f64,
    pub wz: f64,
    /// Overall depth of the section, m.
    pub h: f64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Ladder {
    Chs,
    Round,
    Beam,
}

fn chs(d_mm: f64, t_mm: f64) -> Sec {
    let (d, t) = (d_mm / 1e3, t_mm / 1e3);
    let di = d - 2.0 * t;
    let a = PI / 4.0 * (d * d - di * di);
    let i = PI / 64.0 * (d.powi(4) - di.powi(4));
    Sec { text: format!("CHS {d_mm}x{t_mm}"), a, iy: i, iz: i, j: 2.0 * i, wy: i / (d / 2.0), wz: i / (d / 2.0), h: d }
}

fn round(d_mm: f64) -> Sec {
    let d = d_mm / 1e3;
    let i = PI / 64.0 * d.powi(4);
    Sec { text: format!("round {d_mm}"), a: PI / 4.0 * d * d, iy: i, iz: i, j: 2.0 * i, wy: i / (d / 2.0), wz: i / (d / 2.0), h: d }
}

/// Welded I section `h` x `b` with the web and flange of the `I hxb` profile text.
fn beam(h_mm: f64, b_mm: f64) -> Sec {
    let (h, b) = (h_mm / 1e3, b_mm / 1e3);
    let tw = (h_mm / 40.0).max(6.0) / 1e3;
    let tf = (h_mm / 25.0).max(8.0) / 1e3;
    let hw = h - 2.0 * tf;
    Sec {
        text: format!("I {h_mm}x{b_mm}"),
        a: 2.0 * b * tf + hw * tw,
        iy: (b * h.powi(3) - (b - tw) * hw.powi(3)) / 12.0,
        iz: (2.0 * tf * b.powi(3) + hw * tw.powi(3)) / 12.0,
        j: (2.0 * b * tf.powi(3) + hw * tw.powi(3)) / 3.0,
        wy: (b * h.powi(3) - (b - tw) * hw.powi(3)) / 12.0 / (h / 2.0),
        wz: (2.0 * tf * b.powi(3) + hw * tw.powi(3)) / 12.0 / (b / 2.0),
        h,
    }
}

/// Sections of a ladder, smallest first.
pub(crate) fn ladder(l: Ladder) -> Vec<Sec> {
    match l {
        Ladder::Chs => [
            (60.3, 3.0), (76.1, 3.0), (88.9, 4.0), (114.3, 4.0), (139.7, 5.0), (168.3, 6.0), (193.7, 6.0),
            (219.1, 8.0), (244.5, 8.0), (273.0, 10.0), (323.9, 10.0), (355.6, 12.0), (406.4, 12.0),
            (457.0, 14.0), (508.0, 16.0), (559.0, 18.0), (610.0, 20.0), (711.0, 22.0), (813.0, 25.0),
            (914.0, 28.0), (1016.0, 32.0), (1219.0, 36.0),
        ]
        .iter()
        .map(|&(d, t)| chs(d, t))
        .collect(),
        Ladder::Round => [
            20.0, 24.0, 30.0, 36.0, 42.0, 50.0, 60.0, 70.0, 80.0, 90.0, 100.0, 120.0, 140.0, 160.0, 180.0, 200.0, 250.0,
        ]
        .iter()
        .map(|&d| round(d))
        .collect(),
        Ladder::Beam => [
            300.0, 400.0, 500.0, 600.0, 700.0, 800.0, 1000.0, 1200.0, 1500.0, 1800.0, 2200.0, 2600.0, 3000.0,
        ]
        .iter()
        .map(|&h| beam(h, (h / 2.0).clamp(200.0, 600.0)))
        .collect(),
    }
}

/// Properties of the profile a script gave. Closed-form shapes only; the others (angles,
/// channels, tees, free outlines) return None and the group is reported as not checked.
pub(crate) fn sec_of_profile(p: &ddc_ops::Profile) -> Option<Sec> {
    use ddc_ops::Profile as P;
    let sec = |a: f64, iy: f64, iz: f64, j: f64, h: f64, b: f64| Sec {
        text: "given".into(),
        a,
        iy,
        iz,
        j,
        wy: iy / (h / 2.0),
        wz: iz / (b / 2.0),
        h,
    };
    match p {
        P::Rect { w, h } => {
            let (w, h) = (w.to_m(), h.to_m());
            let (iy, iz) = (w * h.powi(3) / 12.0, h * w.powi(3) / 12.0);
            Some(sec(w * h, iy, iz, iy + iz, h, w))
        }
        P::Circle { r } => {
            let d = 2.0 * r.to_m();
            let i = PI / 64.0 * d.powi(4);
            Some(sec(PI / 4.0 * d * d, i, i, 2.0 * i, d, d))
        }
        P::Ring { r_out, r_in } => {
            let (d, di) = (2.0 * r_out.to_m(), 2.0 * r_in.to_m());
            let i = PI / 64.0 * (d.powi(4) - di.powi(4));
            Some(sec(PI / 4.0 * (d * d - di * di), i, i, 2.0 * i, d, d))
        }
        P::Tube { w, h, t } => {
            let (w, h, t) = (w.to_m(), h.to_m(), t.to_m());
            let (wi, hi) = (w - 2.0 * t, h - 2.0 * t);
            let iy = (w * h.powi(3) - wi * hi.powi(3)) / 12.0;
            let iz = (h * w.powi(3) - hi * wi.powi(3)) / 12.0;
            Some(sec(w * h - wi * hi, iy, iz, iy + iz, h, w))
        }
        P::I { h, b, tw, tf } | P::H { h, b, tw, tf } => {
            let (h, b, tw, tf) = (h.to_m(), b.to_m(), tw.to_m(), tf.to_m());
            let hw = h - 2.0 * tf;
            Some(sec(
                2.0 * b * tf + hw * tw,
                (b * h.powi(3) - (b - tw) * hw.powi(3)) / 12.0,
                (2.0 * tf * b.powi(3) + hw * tw.powi(3)) / 12.0,
                (2.0 * b * tf.powi(3) + hw * tw.powi(3)) / 3.0,
                h,
                b,
            ))
        }
        _ => None,
    }
}

/// A bolted gusset joint at the end of a rod: bolts of one size in a line, and the plate.
#[derive(Clone, Debug, PartialEq)]
pub(crate) struct Joint {
    pub bolts: usize,
    pub bolt: String,
    /// Bolt diameter and spacing along the rod, m.
    pub d: f64,
    pub pitch: f64,
    pub class: String,
    /// Plate thickness, width and length along the rod, m.
    pub t: f64,
    pub w: f64,
    pub l: f64,
}

impl Joint {
    pub fn text(&self) -> String {
        format!(
            "{} x {} {} on PL {:.0}x{:.0}x{:.0}",
            self.bolts,
            self.bolt,
            self.class,
            self.t * 1e3,
            self.w * 1e3,
            self.l * 1e3
        )
    }
}

/// The joint that carries axial force `n` (N) through a gusset plate. Bolts: the smallest of
/// the catalogue that needs at most six in a line, each taking 0.6 f_ub A_s / 1.25 in shear
/// (single shear, thread in the plane). Plate: thinnest of the catalogue whose net section
/// takes `n` at the yield strength (never under 10 mm, so a bolt bar through it is a member), widened until it is at most 25 mm thick.
pub(crate) fn joint(n: f64, r: &Rules) -> Joint {
    let cat = ddc_geom::steel::catalog::catalog();
    let class = cat.class("8.8").map_or(("8.8", 800e6), |c| ("8.8", c.fub));
    let fb = |b: &ddc_geom::steel::catalog::BoltSize| 0.6 * class.1 * b.area / 1.25;
    let need = |b: &ddc_geom::steel::catalog::BoltSize| ((n / fb(b)).ceil() as usize).max(2);
    let b = cat.bolts.iter().find(|b| need(b) <= 6).unwrap_or(&cat.bolts[cat.bolts.len() - 1]);
    let k = need(b);
    let fy = r.get("fy") * 1e6;
    let mut w = 4.0 * b.hole;
    let mut t = cat.plate_t((n / (fy * (w - b.hole))).max(0.010));
    while t > 0.025 && w < 0.6 {
        w += 0.02;
        t = cat.plate_t((n / (fy * (w - b.hole))).max(0.010));
    }
    Joint { bolts: k, bolt: b.name(), d: b.d, pitch: 3.0 * b.hole, class: class.0.into(), t, w, l: (k - 1) as f64 * 3.0 * b.hole + 4.0 * b.hole }
}

/// Ratio of load effect to resistance of one member. `ef` are the end forces in local axes
/// (nodes on the member); the axial force is tension-positive.
fn utilisation(s: &Sec, len: f64, ef: &[[f64; 6]; 2], tension_only: bool, r: &Rules) -> f64 {
    let fy = r.get("fy") * 1e6;
    // axial force of the more stressed end, tension positive (end j: +x pulls the member)
    let (n0, n1) = (-ef[0][0], ef[1][0]);
    let n = if n0.abs() > n1.abs() { n0 } else { n1 };
    let my = ef[0][4].abs().max(ef[1][4].abs());
    let mz = ef[0][5].abs().max(ef[1][5].abs());
    let mom = my / (s.wy * fy) + mz / (s.wz * fy);
    if n >= 0.0 {
        return n / (s.a * fy) + mom;
    }
    if tension_only {
        return mom;
    }
    let rg = (s.iy.min(s.iz) / s.a).sqrt();
    let lam = r.get("eff_len") * len / rg / (PI * (E / fy).sqrt());
    let phi = 0.5 * (1.0 + r.get("buckling_alpha") * (lam - 0.2) + lam * lam);
    let chi = (1.0 / (phi + (phi * phi - lam * lam).max(0.0).sqrt())).min(1.0);
    -n / (chi * s.a * fy) + mom
}

/// Smallest section of `l` (starting at `from`) that carries compression `n` (N) together
/// with a transverse force `h` (N) at the top of a cantilever of height `len`.
pub(crate) fn size_column(l: Ladder, n: f64, h: f64, len: f64, r: &Rules) -> usize {
    let lad = ladder(l);
    let ef = [[n, 0.0, 0.0, 0.0, h * len, 0.0], [-n, 0.0, 0.0, 0.0, 0.0, 0.0]];
    // pinned base cantilever: eff_len doubles
    let mut rr = Rules(r.0.clone());
    let _ = rr.set("eff_len", (2.0 * r.get("eff_len")).min(2.0));
    (0..lad.len())
        .find(|&i| utilisation(&lad[i], len, &ef, false, &rr) <= r.get("util_max"))
        .unwrap_or(lad.len() - 1)
}

// ---- the lattice ----

type Key = (i64, i64, i64);

fn key(p: V3) -> Key {
    ((p.x * 1e4).round() as i64, (p.y * 1e4).round() as i64, (p.z * 1e4).round() as i64)
}

/// A group of members sharing one section, picked from a ladder.
#[derive(Clone, Debug)]
pub(crate) struct Group {
    pub name: &'static str,
    pub ladder: Ladder,
    pub idx: usize,
    /// The script gave the section; sizing leaves it (its properties are not known here).
    pub fixed: bool,
    /// Cables and rods: slack under compression, no buckling check.
    pub tension: bool,
    /// Properties of the section the script gave, when its text could be read.
    pub custom: Option<Sec>,
}

impl Group {
    pub fn new(name: &'static str, ladder: Ladder, tension: bool) -> Group {
        let n = self::ladder(ladder).len();
        let start = match ladder {
            Ladder::Chs => 0.45,
            Ladder::Round => 0.3,
            Ladder::Beam => 0.35,
        };
        Group { name, ladder, idx: ((n - 1) as f64 * start).round() as usize, fixed: false, tension, custom: None }
    }
}

struct LMem {
    a: usize,
    b: usize,
    g: usize,
    pin: bool,
}

/// Nodes, members, supports and loads of one analysis.
#[derive(Default)]
pub(crate) struct Lat {
    pub nodes: Vec<V3>,
    ix: HashMap<Key, usize>,
    mems: Vec<LMem>,
    seen: HashSet<(usize, usize)>,
    sup: Vec<(usize, [bool; 6])>,
    /// Nodal loads, N, global axes: permanent, imposed, wind.
    loads: [Vec<(usize, [f64; 3])>; 3],
}

impl Lat {
    pub fn node(&mut self, p: V3) -> usize {
        let k = key(p);
        if let Some(&i) = self.ix.get(&k) {
            return i;
        }
        self.nodes.push(p);
        self.ix.insert(k, self.nodes.len() - 1);
        self.nodes.len() - 1
    }

    pub fn find(&self, p: V3) -> Option<usize> {
        self.ix.get(&key(p)).copied()
    }

    /// A member of group `g`; pinned members carry axial force only.
    pub fn member(&mut self, a: V3, b: V3, g: usize, pin: bool) {
        let (i, j) = (self.node(a), self.node(b));
        if i == j || !self.seen.insert((i.min(j), i.max(j))) {
            return;
        }
        self.mems.push(LMem { a: i, b: j, g, pin });
    }

    /// Restrains the translations flagged in `t` (x, y, z) and, with `rot`, the rotations.
    pub fn support(&mut self, p: V3, t: [bool; 3], rot: bool) {
        let n = self.node(p);
        self.sup.push((n, [t[0], t[1], t[2], rot, rot, rot]));
    }

    /// Adds force `f` (N) at `p` to load case `case` (0 permanent, 1 imposed, 2 wind).
    pub fn load(&mut self, case: usize, p: V3, f: [f64; 3]) {
        let n = self.node(p);
        self.loads[case].push((n, f));
    }

    pub fn members(&self) -> usize {
        self.mems.len()
    }

    /// Nodes that one member ends in and nothing holds: an analysis model with any is not a
    /// structure.
    pub fn free_ends(&self) -> usize {
        let mut deg = vec![0usize; self.nodes.len()];
        for m in &self.mems {
            deg[m.a] += 1;
            deg[m.b] += 1;
        }
        let held: HashSet<usize> = self.sup.iter().map(|s| s.0).collect();
        (0..self.nodes.len()).filter(|n| deg[*n] == 1 && !held.contains(n)).count()
    }

    /// Members of group `g`.
    pub fn count_of(&self, g: usize) -> usize {
        self.mems.iter().filter(|m| m.g == g).count()
    }
}

/// What the deflection and frequency criteria look at.
pub(crate) struct Crit {
    pub nodes: Vec<V3>,
    /// Limit of the deflection, m.
    pub limit: f64,
    /// Deflection under permanent plus imposed load instead of imposed alone.
    pub total: bool,
    /// Groups stiffened when a criterion fails.
    pub stiff: Vec<usize>,
    /// Lowest first vertical frequency, Hz; 0 = not required.
    pub f_min: f64,
}

#[derive(Debug, Default)]
pub(crate) struct Design {
    /// Members and free ends of the analysis model, members per group.
    pub members: usize,
    pub free_ends: usize,
    /// Largest axial force of each group, N: what its joints are sized for.
    pub force: Vec<f64>,
    pub per_group: Vec<usize>,
    /// Groups whose given section could not be checked; the check does not pass while any remain.
    pub unchecked: Vec<String>,
    /// Strength and deflection pass.
    pub ok: bool,
    /// The first vertical frequency meets the requirement (true when none is set).
    pub vib_ok: bool,
    /// The analysis could not run; the groups keep their start sections.
    pub failed: Option<String>,
    pub util: Vec<f64>,
    pub util_max: f64,
    pub defl: f64,
    pub limit: f64,
    pub f1: f64,
    pub mass: f64,
    pub solves: usize,
    /// Wall time spent solving, s.
    pub secs: f64,
    /// Largest factored support reaction per support node: (node index, [Fx, Fy, Fz] N).
    pub react: Vec<(usize, [f64; 3])>,
    pub notes: Vec<String>,
}

fn frame(lat: &Lat, groups: &[Group], lads: &[Vec<Sec>], r: &Rules) -> Frame {
    let mut cases = vec![LoadCase::default(), LoadCase::default(), LoadCase::default()];
    cases[0].name = "permanent".into();
    cases[1].name = "imposed".into();
    cases[2].name = "wind".into();
    cases[0].loads.push(Load::SelfWeight { dir: [0.0, 0.0, -1.0], g: GRAV });
    for (c, ls) in lat.loads.iter().enumerate() {
        for (n, f) in ls {
            cases[c].loads.push(Load::Node { node: *n, f: [f[0], f[1], f[2], 0.0, 0.0, 0.0] });
        }
    }
    let (gd, gl, gw, psi) = (r.get("gamma_dead"), r.get("gamma_live"), r.get("gamma_wind"), r.get("psi"));
    let combo = |name: &str, f: &[(usize, f64)]| Combo { name: name.into(), factors: f.to_vec() };
    Frame {
        nodes: lat.nodes.iter().map(|p| [p.x, p.y, p.z]).collect(),
        materials: vec![Material { e: E, g: G, density: DENSITY }],
        sections: groups
            .iter()
            .enumerate()
            .map(|(i, g)| {
                let s = &lads[i][g.idx];
                Section::new(s.a, s.iy, s.iz, s.j)
            })
            .collect(),
        members: lat
            .mems
            .iter()
            .map(|m| {
                let mm = Member::new(m.a, m.b, 0, m.g);
                if m.pin {
                    mm.pin_end(0).pin_end(1)
                } else {
                    mm
                }
            })
            .collect(),
        supports: lat
            .sup
            .iter()
            .map(|(n, f)| {
                Support::new(*n, std::array::from_fn(|i| if f[i] { Restraint::Fixed } else { Restraint::Free }))
            })
            .collect(),
        cases,
        combos: vec![
            combo("imposed", &[(1, 1.0)]),
            combo("uls1", &[(0, gd), (1, gl)]),
            combo("uls2", &[(0, gd), (2, gw), (1, gl * psi)]),
            combo("uls3", &[(0, 1.0), (2, gw)]),
            combo("total", &[(0, 1.0), (1, 1.0)]),
        ],
        envelopes: vec![],
    }
}

struct Result_ {
    util: Vec<f64>,
    /// Largest axial force of each group under the strength combinations, N.
    nmax: Vec<f64>,
    defl: f64,
    f1: f64,
    react: Vec<(usize, [f64; 3])>,
}

fn analyse(lat: &Lat, groups: &[Group], lads: &[Vec<Sec>], crit: &Crit, r: &Rules) -> Result<Result_, String> {
    let fr = frame(lat, groups, lads, r);
    let sol = solve(&fr, &Options { stations: 2, ..Options::default() }).map_err(|e| e.to_string())?;
    let mut util = vec![0.0f64; groups.len()];
    let mut nmax = vec![0.0f64; groups.len()];
    for c in 1..=3 {
        let res = &sol.combos[c];
        for (k, m) in lat.mems.iter().enumerate() {
            let len = lat.nodes[m.a].sub(lat.nodes[m.b]).len();
            let s = &lads[m.g][groups[m.g].idx];
            let u = utilisation(s, len, &res.end_forces[k], groups[m.g].tension, r);
            util[m.g] = util[m.g].max(u);
            let ef = &res.end_forces[k];
            nmax[m.g] = nmax[m.g].max(ef[0][0].abs().max(ef[1][0].abs()));
        }
    }
    let pick = if crit.total { &sol.combos[4] } else { &sol.combos[0] };
    let mut defl = 0.0f64;
    let mut dead = 0.0f64;
    for p in &crit.nodes {
        if let Some(n) = lat.find(*p) {
            defl = defl.max(pick.disp[n][2].abs());
            dead = dead.max(-sol.cases[0].disp[n][2]);
        }
    }
    let f1 = if dead > 1e-9 { 0.18 * (GRAV / dead).sqrt() } else { f64::INFINITY };
    let mut react: Vec<(usize, [f64; 3])> = vec![];
    for c in 1..=3 {
        for (n, f) in &sol.combos[c].reactions {
            let v = [f[0], f[1], f[2]];
            match react.iter_mut().find(|x| x.0 == *n) {
                Some(x) => {
                    if v[2].abs() > x.1[2].abs() {
                        x.1 = v;
                    }
                }
                None => react.push((*n, v)),
            }
        }
    }
    Ok(Result_ { util, nmax, defl, f1, react })
}

fn used(lat: &Lat, g: usize) -> bool {
    lat.mems.iter().any(|m| m.g == g)
}

/// Total member length of group `g`, m.
fn len_of(lat: &Lat, g: usize) -> f64 {
    lat.mems.iter().filter(|m| m.g == g).map(|m| lat.nodes[m.a].sub(lat.nodes[m.b]).len()).sum()
}

/// Sizes the groups of `lat` (their `idx` is set) and checks the criteria.
pub(crate) fn design(lat: &Lat, groups: &mut [Group], crit: &Crit, r: &Rules, budget: f64) -> Design {
    let lads: Vec<Vec<Sec>> = groups
        .iter()
        .map(|g| match &g.custom {
            Some(c) if g.fixed => vec![c.clone()],
            _ => ladder(g.ladder),
        })
        .collect();
    for (g, gr) in groups.iter_mut().enumerate() {
        if lads[g].len() == 1 {
            gr.idx = 0;
        }
    }
    let umax = r.get("util_max");
    let mut d = Design { limit: crit.limit, ..Design::default() };
    d.members = lat.members();
    d.free_ends = lat.free_ends();
    d.per_group = (0..groups.len()).map(|g| lat.count_of(g)).collect();
    for (gi, g) in groups.iter().enumerate() {
        if g.fixed && g.custom.is_none() && used(lat, gi) {
            d.unchecked.push(g.name.to_string());
            d.notes.push(format!(
                "{}: the given section has no closed-form properties here (angle, channel, tee or free outline), so it was not checked; the check stands for the other groups only",
                g.name
            ));
        }
    }
    let mut last: Option<Result_> = None;
    let t0 = Instant::now();
    macro_rules! run {
        () => {
            match analyse(lat, groups, &lads, crit, r) {
                Ok(x) => {
                    d.solves += 1;
                    d.secs = t0.elapsed().as_secs_f64();
                    x
                }
                Err(e) => {
                    d.failed = Some(e);
                    return d;
                }
            }
        };
    }
    // 1. scale each group's section to its utilisation (up and down), a few rounds
    for round in 0..8 {
        if d.secs > budget {
            break;
        }
        let x = run!();
        let mut changed = false;
        for (g, gr) in groups.iter_mut().enumerate() {
            if gr.fixed || !used(lat, g) {
                continue;
            }
            let u = x.util[g];
            let s = &lads[g][gr.idx];
            let want = if u > umax {
                s.a * u / umax * 1.02
            } else if u < 0.5 && round < 5 {
                s.a * u / 0.8
            } else {
                continue;
            };
            let n = lads[g].iter().position(|c| c.a >= want).unwrap_or(lads[g].len() - 1);
            let n = if u > umax { n.max(gr.idx + 1).min(lads[g].len() - 1) } else { n.min(gr.idx) };
            if n != gr.idx {
                gr.idx = n;
                changed = true;
            }
        }
        last = Some(x);
        if !changed {
            break;
        }
    }
    // 2. strength and stiffness to a fixed point; sections only grow from here. The
    // frequency drives at most FREQ_STEPS extra rounds: past them the footbridge is flagged
    // for a dynamic check rather than stiffened without end.
    let mut fsteps = 0;
    // one group a stiffening round, so more rounds than when every stiff group stepped
    for _ in 0..80 {
        if d.secs > budget {
            d.notes.push("sizing stopped at its time budget; the sections reached are kept".into());
            break;
        }
        let x = run!();
        let mut grew = false;
        for (g, gr) in groups.iter_mut().enumerate() {
            if !gr.fixed && used(lat, g) && x.util[g] > umax && gr.idx + 1 < lads[g].len() {
                gr.idx += 1;
                grew = true;
            }
        }
        if grew {
            last = Some(x);
            continue;
        }
        let defl_ok = x.defl <= crit.limit;
        let freq_ok = crit.f_min <= 0.0 || x.f1 >= crit.f_min;
        // FREQ_STEPS rungs of every stiff group, now taken one group at a time
        if defl_ok && (freq_ok || fsteps >= FREQ_STEPS * crit.stiff.len().max(1)) {
            last = Some(x);
            break;
        }
        if defl_ok {
            fsteps += 1;
        }
        // stiffen the group whose next rung closes most of the shortfall per tonne added: a
        // truss may gain more from its web than from its chords
        let short = |y: &Result_| if y.defl > crit.limit { y.defl / crit.limit } else { crit.f_min / y.f1.max(1e-9) };
        let s0 = short(&x);
        let mut best: Option<(usize, f64)> = None;
        for g in 0..groups.len() {
            if groups[g].fixed || !used(lat, g) || groups[g].idx + 1 >= lads[g].len() || d.secs > budget {
                continue;
            }
            let dm = (lads[g][groups[g].idx + 1].a - lads[g][groups[g].idx].a) * len_of(lat, g) * DENSITY;
            groups[g].idx += 1;
            let tried = analyse(lat, groups, &lads, crit, r);
            groups[g].idx -= 1;
            d.solves += 1;
            d.secs = t0.elapsed().as_secs_f64();
            if let Ok(y) = tried {
                let gain = (s0 - short(&y)) / dm.max(1e-6);
                if gain > 0.0 && best.is_none_or(|b| gain > b.1) {
                    best = Some((g, gain));
                }
            }
        }
        let mut moved = false;
        if let Some((g, _)) = best {
            groups[g].idx += 1;
            moved = true;
        } else {
            for &g in &crit.stiff {
                if g < groups.len() && !groups[g].fixed && used(lat, g) && groups[g].idx + 1 < lads[g].len() {
                    groups[g].idx += 1;
                    moved = true;
                }
            }
        }
        last = Some(x);
        if !moved {
            break;
        }
    }
    // 3. back off: a group goes one rung lighter wherever strength, stiffness and the vibration
    // state reached above still hold, so the stiffening rounds cannot leave a design heavier
    // than a stricter one that never needed them. A rung taken early for stiffness can be
    // spare once later rungs land elsewhere: back off to a fixed point, not a few rounds.
    if let Some(mut cur) = last.take() {
        let f_ref = if crit.f_min > 0.0 && cur.f1 >= crit.f_min { crit.f_min } else { cur.f1 };
        for _ in 0..lads.iter().map(Vec::len).max().unwrap_or(0) {
            let mut trimmed = false;
            for g in 0..groups.len() {
                if d.secs > budget {
                    break;
                }
                if groups[g].fixed || !used(lat, g) || groups[g].idx == 0 {
                    continue;
                }
                groups[g].idx -= 1;
                let kept = match analyse(lat, groups, &lads, crit, r) {
                    Ok(x) => {
                        d.solves += 1;
                        let ok = x.util.iter().all(|u| *u <= umax + 1e-9) && x.defl <= crit.limit && x.f1 >= f_ref - 1e-9;
                        ok.then_some(x)
                    }
                    Err(_) => None,
                };
                d.secs = t0.elapsed().as_secs_f64();
                match kept {
                    Some(x) => {
                        cur = x;
                        trimmed = true;
                    }
                    None => groups[g].idx += 1,
                }
            }
            if !trimmed {
                break;
            }
        }
        last = Some(cur);
    }
    let x = last.unwrap_or_else(|| Result_ { util: vec![0.0; groups.len()], nmax: vec![0.0; groups.len()], defl: 0.0, f1: 0.0, react: vec![] });
    d.util_max = x.util.iter().cloned().fold(0.0, f64::max);
    d.ok = d.util_max <= umax + 1e-9 && x.defl <= crit.limit && d.unchecked.is_empty();
    d.vib_ok = crit.f_min <= 0.0 || x.f1 >= crit.f_min;
    if d.util_max > umax + 1e-9 {
        d.notes.push(format!("strength: largest ratio {:.2} stays over the {umax} rule at the largest section", d.util_max));
    }
    if x.defl > crit.limit {
        d.notes.push("stiffness: the deflection limit is not met at the largest sections; add depth or a support".into());
    }
    if !d.vib_ok {
        d.notes.push(format!(
            "vibration: first vertical frequency {:.1} Hz is under {:.1} Hz; add depth, or check the dynamic response and add damping",
            x.f1, crit.f_min
        ));
    }
    d.defl = x.defl;
    d.f1 = x.f1;
    d.react = x.react;
    d.util = x.util;
    d.force = x.nmax;
    d.mass = lat
        .mems
        .iter()
        .map(|m| lads[m.g][groups[m.g].idx].a * lat.nodes[m.a].sub(lat.nodes[m.b]).len() * DENSITY)
        .sum();
    d
}

/// Text of the section of group `g` for `prof_text`.
pub(crate) fn text_of(g: &Group) -> String {
    ladder(g.ladder)[g.idx].text.clone()
}

/// Depth of the section of group `g`, m.
pub(crate) fn depth_of(g: &Group) -> f64 {
    ladder(g.ladder)[g.idx].h
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rel(a: f64, b: f64) -> f64 {
        (a - b).abs() / b.abs()
    }

    /// A cantilever under a tip load deflects P L^3 / 3 E I, and the check reads it back.
    #[test]
    fn cantilever_tip_deflection_matches_closed_form() {
        let (l, p) = (6.0, 20e3);
        let mut lat = Lat::default();
        lat.member(V3::new(0.0, 0.0, 0.0), V3::new(l, 0.0, 0.0), 0, false);
        lat.support(V3::new(0.0, 0.0, 0.0), [true, true, true], true);
        lat.load(1, V3::new(l, 0.0, 0.0), [0.0, 0.0, -p]);
        let mut gs = vec![Group::new("beam", Ladder::Beam, false)];
        gs[0].fixed = true;
        let s = ladder(Ladder::Beam)[gs[0].idx].clone();
        let crit = Crit { nodes: vec![V3::new(l, 0.0, 0.0)], limit: 1.0, total: false, stiff: vec![], f_min: 0.0 };
        let d = design(&lat, &mut gs, &crit, &Rules::new(), BUDGET);
        assert!(d.failed.is_none(), "{:?}", d.failed);
        assert!(rel(d.defl, p * l.powi(3) / (3.0 * E * s.iy)) < 1e-6, "{} vs {}", d.defl, p * l.powi(3) / (3.0 * E * s.iy));
    }

    /// A hanger in tension carries factored load over its area; the ratio is the same
    /// closed-form number, so the sign of the axial force is right.
    #[test]
    fn hanger_ratio_is_the_factored_tension_over_resistance() {
        let (l, f) = (5.0, 40e3);
        let (top, bot) = (V3::new(0.0, 0.0, l), V3::new(0.0, 0.0, 0.0));
        let mut lat = Lat::default();
        lat.member(top, bot, 0, true);
        lat.support(top, [true, true, true], true);
        // a bar has no lateral stiffness: the loaded end is held sideways, free to move down
        lat.support(bot, [true, true, false], true);
        lat.load(1, bot, [0.0, 0.0, -f]);
        let mut gs = vec![Group::new("hanger", Ladder::Round, true)];
        gs[0].fixed = true;
        let s = ladder(Ladder::Round)[gs[0].idx].clone();
        let r = Rules::new();
        let crit = Crit { nodes: vec![bot], limit: 1.0, total: false, stiff: vec![], f_min: 0.0 };
        let d = design(&lat, &mut gs, &crit, &r, BUDGET);
        assert!(d.failed.is_none(), "{:?}", d.failed);
        let w = s.a * l * DENSITY * GRAV;
        let want = (r.get("gamma_dead") * w + r.get("gamma_live") * f) / (s.a * r.get("fy") * 1e6);
        assert!(rel(d.util[0], want) < 1e-3, "{} vs {want}", d.util[0]);
    }

    /// Sizing grows a strut until it passes, and a stricter rule gives a heavier section.
    #[test]
    fn strut_grows_until_it_passes_and_a_stricter_rule_costs_steel() {
        let build = |util: f64| {
            let mut lat = Lat::default();
            let (a, b) = (V3::new(0.0, 0.0, 0.0), V3::new(0.0, 0.0, 8.0));
            lat.member(a, b, 0, true);
            lat.support(a, [true, true, true], true);
            lat.support(b, [true, true, false], true);
            lat.load(1, b, [0.0, 0.0, -900e3]);
            let mut r = Rules::new();
            r.set("util_max", util).unwrap();
            let mut gs = vec![Group::new("strut", Ladder::Chs, false)];
            let crit = Crit { nodes: vec![b], limit: 1.0, total: false, stiff: vec![], f_min: 0.0 };
            let d = design(&lat, &mut gs, &crit, &r, BUDGET);
            (d, gs[0].idx)
        };
        let (d1, i1) = build(0.95);
        let (d2, i2) = build(0.5);
        assert!(d1.failed.is_none() && d1.ok && d1.util_max <= 0.95 + 1e-9, "{d1:?}");
        assert!(d2.ok && d2.util_max <= 0.5 + 1e-9, "{d2:?}");
        assert!(i2 > i1 && d2.mass > d1.mass, "{i1} {i2} {} {}", d1.mass, d2.mass);
    }

    #[test]
    fn rules_refuse_unknown_names_and_values_out_of_range() {
        let mut r = Rules::new();
        assert!(r.set("nonsense", 1.0).unwrap_err().contains("not a rule"));
        assert!(r.set("util_max", 2.0).unwrap_err().contains("outside its range"));
        assert!(r.set("fy", 460.0).is_ok() && r.get("fy") == 460.0);
        for rule in rules() {
            assert!(rule.value >= rule.lo && rule.value <= rule.hi, "{}", rule.id);
            assert!(rule.why.len() > 20, "{} needs a reason", rule.id);
        }
        assert_eq!(rules().len(), 21);
        // each rule has a title and a description in both languages of the UI
        for (lang, text) in [
            ("en", include_str!("../../ui/data/i18n/en/gen_rules.json")),
            ("ru", include_str!("../../ui/data/i18n/ru/gen_rules.json")),
        ] {
            let t: serde_json::Value = serde_json::from_str(text).unwrap();
            for rule in rules() {
                for part in ["title", "desc"] {
                    let k = format!("gen.rule.span.{}.{part}", rule.id);
                    assert!(t[&k].as_str().is_some_and(|x| !x.is_empty()), "{lang}: {k}");
                }
            }
        }
    }
}
