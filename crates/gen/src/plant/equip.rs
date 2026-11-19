// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Parametric geometry of each unit as a family: shells, heads, legs, saddles, casings, motors,
//! burners, with every nozzle a neck, a flange and a connector at its face. The body boxes the
//! pipes keep clear of and the maintenance clearances by the rules come with it.

use std::f64::consts::FRAC_PI_2;
use std::fmt::Write;

use ddc_geom::component::doc::mm;
use ddc_geom::math::{v3, V3};

use crate::project::rules::rule;

use super::route;
use super::size::{self, ItemKind};
use super::{Clear, EqKind, Equip, Level, Line, Spot};

/// Neck of a nozzle beyond the flange radius, and how deep it sits into the shell, m.
const NECK: f64 = 0.1;
const EMBED: f64 = 0.02;
/// Segments of a head profile.
const HEAD_N: usize = 6;
/// A 2:1 ellipsoidal head is a quarter of the diameter high; a cone roof a tenth.
const HEAD_K: f64 = 0.25;
const ROOF_K: f64 = 0.1;
/// Legs: section as a share of the diameter (with a least size) and their place on the
/// diagonals.
const LEG_K: f64 = 0.04;
const LEG_MIN: f64 = 0.06;
const LEG_AT: f64 = 0.6;
/// A manhole on the front of a vessel from this diameter: its bore and how far it stands out.
const MANHOLE_FROM: f64 = 0.8;
const MANHOLE_D: f64 = 0.5;
const MANHOLE_OUT: f64 = 0.25;
/// Jacket of a reactor: gap to the shell and the share of the shell it covers.
const JACKET_GAP: f64 = 0.05;
const JACKET_FROM: f64 = 0.1;
const JACKET_TO: f64 = 0.8;
/// Agitator drive on a vessel top: radius and height from the diameter.
const DRIVE_R: (f64, f64) = (0.1, 0.05);
const DRIVE_H: (f64, f64) = (0.35, 0.15);
/// Side nozzles keep this share of the straight height from its ends.
const SIDE_K: f64 = 0.15;
/// Side and top nozzles spread over this share of the radius.
const SPREAD: f64 = 0.6;
/// Top nozzle slots lean toward the platform side within these shares of the radius.
const SLOT_MIN: f64 = 0.3;
const SLOT_MAX: f64 = 0.7;
/// Pump: plinth height, casing radius from the discharge bore (with a least radius), coupling
/// length, motor diameter from the rating (kW) and length per diameter.
const PUMP_BASE: f64 = 0.15;
const CASING_K: f64 = 1.6;
const CASING_MIN: f64 = 0.1;
const COUPLING: f64 = 0.12;
const MOTOR_D: (f64, f64) = (0.16, 0.3);
const MOTOR_L: f64 = 2.2;
const PUMP_CLEAR: f64 = 0.05;
/// Boiler: frame height, burner length and size from the shell diameter, smoke box depth.
const BOILER_BASE: f64 = 0.35;
const BURNER_L: (f64, f64) = (0.4, 0.3);
const BURNER_W: f64 = 0.5;
const SMOKE_MIN: f64 = 0.4;
const SMOKE_PAD: f64 = 0.15;
const SPIGOT: f64 = 0.3;
/// Shell-and-tube: channel length from the diameter, rear head depth, saddle place and width,
/// nozzle distance from the shell ends.
const CHANNEL_L: (f64, f64) = (0.3, 0.5);
const REAR_K: f64 = 0.25;
const REAR_R: f64 = 0.85;
const SADDLE_AT: f64 = 0.3;
const SADDLE_W: f64 = 0.15;
const SADDLE_Y: f64 = 0.8;
const END_OFF: f64 = 0.35;
/// Terminal sleeve through the wall and its gap around the pipe.
pub(super) const WALL_T: f64 = 0.3;
const SLEEVE_GAP: f64 = 0.02;
/// Drain sump: half the pit size, the pit below the inlet, wall thickness.
const SUMP_A: f64 = 0.4;
const SUMP_BELOW: f64 = 0.6;
const SUMP_WALL: f64 = 0.1;

/// A family text with its solids and the body boxes they fill.
pub(super) struct F {
    pub s: String,
    n: usize,
    boxes: Vec<(V3, V3)>,
}

fn lohi(a: V3, b: V3) -> (V3, V3) {
    (v3(a.x.min(b.x), a.y.min(b.y), a.z.min(b.z)), v3(a.x.max(b.x), a.y.max(b.y), a.z.max(b.z)))
}

impl F {
    fn new(e: &Equip) -> F {
        F::named(&e.family_name(), "equipment", e.kind.ifc())
    }
    /// An empty family of `category` exported as `ifc`.
    pub(super) fn named(name: &str, category: &str, ifc: &str) -> F {
        let mut s = String::new();
        let _ = writeln!(s, "family {name}\ncategory {category}\nhost none\nifc {ifc}");
        F { s, n: 0, boxes: vec![] }
    }
    fn name(&mut self, base: &str) -> String {
        self.n += 1;
        format!("{base}{}", self.n)
    }
    /// A box solid; `body` adds it to the boxes pipes keep clear of.
    pub(super) fn block(&mut self, base: &str, a: V3, b: V3, mat: &str, body: bool) {
        let (lo, hi) = lohi(a, b);
        let n = self.name(base);
        let _ = writeln!(
            self.s,
            "extrude {n} rect({}, {}, {}, {}) z({}, {}) mat(\"{mat}\")",
            mm(lo.x),
            mm(lo.y),
            mm(hi.x),
            mm(hi.y),
            mm(lo.z),
            mm(hi.z)
        );
        if body {
            self.boxes.push((lo, hi));
        }
    }
    /// A cylinder of radius `r` between `a` and `b`, which differ along one axis.
    pub(super) fn cyl(&mut self, base: &str, a: V3, b: V3, r: f64, mat: &str, body: bool) {
        let n = self.name(base);
        let d = b - a;
        let line = if d.x.abs() > 1e-9 {
            format!("circle({}, {}, {}) dir(x) z({}, {})", mm(a.y), mm(a.z), mm(r), mm(a.x.min(b.x)), mm(a.x.max(b.x)))
        } else if d.y.abs() > 1e-9 {
            format!("circle({}, {}, {}) dir(y) z({}, {})", mm(a.x), mm(a.z), mm(r), mm(a.y.min(b.y)), mm(a.y.max(b.y)))
        } else {
            format!("circle({}, {}, {}) z({}, {})", mm(a.x), mm(a.y), mm(r), mm(a.z.min(b.z)), mm(a.z.max(b.z)))
        };
        let _ = writeln!(self.s, "extrude {n} {line} mat(\"{mat}\")");
        if body {
            let pad = v3(r, r, r);
            let (lo, hi) = lohi(a, b);
            let flat = |v: f64, p: f64| if v.abs() > 1e-9 { 0.0 } else { p };
            let pad = v3(flat(d.x, pad.x), flat(d.y, pad.y), flat(d.z, pad.z));
            self.boxes.push((lo - pad, hi + pad));
        }
    }
    /// A solid of revolution about the vertical axis through (0, 0) from an (r, z) profile.
    pub(super) fn revolve(&mut self, base: &str, pts: &[(f64, f64)], mat: &str) {
        let n = self.name(base);
        let p: Vec<String> = pts.iter().map(|(r, z)| format!("{}, {}", mm(r.max(0.0)), mm(*z))).collect();
        let _ = writeln!(self.s, "revolve {n} poly({}) axis(0, 0) angle(360deg) mat(\"{mat}\")", p.join(", "));
    }
    /// A round bar of radius `r` along any straight `a`..`b`.
    pub(super) fn bar(&mut self, base: &str, a: V3, b: V3, r: f64, mat: &str) {
        let n = self.name(base);
        let _ = writeln!(
            self.s,
            "sweep {n} circle(0, 0, {}) path({}, {}, {}, {}, {}, {}) mat(\"{mat}\")",
            mm(r),
            mm(a.x),
            mm(a.y),
            mm(a.z),
            mm(b.x),
            mm(b.y),
            mm(b.z)
        );
    }
    pub(super) fn connector(&mut self, name: &str, kind: &str, at: V3, dir: V3, d: f64) {
        let _ = writeln!(
            self.s,
            "connector {name} {kind} at({}, {}, {}) dir({}, {}, {}) size({})",
            mm(at.x),
            mm(at.y),
            mm(at.z),
            dir.x,
            dir.y,
            dir.z,
            mm(d)
        );
    }
}

/// Bore, flange diameter and thickness, and neck projection of nozzle `i` of `e`.
struct NozzleSize {
    od: f64,
    fl: f64,
    ft: f64,
    proj: f64,
}

fn nozzle_size(e: &Equip, i: usize, lines: &[Line]) -> NozzleSize {
    let n = &e.nozzles[i];
    let l = &lines[n.line];
    let fl = size::flange_od(n.dn, l.pn);
    NozzleSize { od: size::pipe(l.spec, n.dn).od, fl, ft: size::flange_t(n.dn), proj: NECK + fl / 2.0 }
}

/// Builds nozzle `i` with its face at `face` facing `dir`, its neck from `root` (on the shell).
fn put_nozzle(f: &mut F, e: &mut Equip, i: usize, lines: &[Line], root: V3, face: V3, dir: V3) {
    let z = nozzle_size(e, i, lines);
    let mat = lines[e.nozzles[i].line].spec.mat();
    let start = root - dir * EMBED;
    let neck_end = face - dir * z.ft;
    f.cyl("Neck", start, neck_end, z.od / 2.0, mat, false);
    f.cyl("Flange", neck_end, face, z.fl / 2.0, mat, false);
    let r = z.fl / 2.0;
    let (lo, hi) = lohi(root, face);
    let pad = v3(if dir.x.abs() > 0.5 { 0.0 } else { r }, if dir.y.abs() > 0.5 { 0.0 } else { r }, if dir.z.abs() > 0.5 { 0.0 } else { r });
    f.boxes.push((lo - pad, hi + pad));
    let name = e.nozzles[i].name;
    f.connector(name, "pipe", face, dir, z.od);
    e.nozzles[i].at = face;
    e.nozzles[i].dir = dir;
}

/// Height of the operating point above the pipe axis for an item on a pipe running along `dir`:
/// a handwheel stands up from a horizontal pipe and out from a vertical one.
pub(super) fn op_rise(item: ItemKind, dir: V3, fl: f64) -> f64 {
    if dir.z.abs() > 0.5 {
        0.0
    } else {
        item.stem(fl)
    }
}

/// Where the operated items on the stubs of a unit's nozzles are worked from (local).
pub(super) fn ops(e: &Equip, lines: &[Line]) -> Vec<V3> {
    let mut out = vec![];
    for n in &e.nozzles {
        let l = &lines[n.line];
        let fl = size::flange_od(n.pipe_dn, l.pn);
        for p in size::stack(n.dn, n.pipe_dn, &n.items, l.spec, l.pn) {
            if let Some(k) = p.item.filter(|k| k.operated()) {
                let centre = n.at + n.dir * (p.at + p.len / 2.0);
                out.push(centre + v3(0.0, 0.0, op_rise(k, n.dir, fl)));
            }
        }
    }
    out
}

/// Largest operating rise of the operated items a nozzle carries on a horizontal stub.
fn side_rise(e: &Equip, i: usize, lines: &[Line]) -> f64 {
    let n = &e.nozzles[i];
    let fl = size::flange_od(n.pipe_dn, lines[n.line].pn);
    n.items.iter().filter(|k| k.operated()).map(|k| k.stem(fl)).fold(0.0, f64::max)
}

/// Offset of top slots toward the platform side, m from the axis, for a shell of radius `r`
/// with `keep` m free at the axis.
fn slot(r: f64, keep: f64) -> f64 {
    let want = r + rule("plant.platform.gap") - 0.8 * rule("plant.access.reach_h");
    want.clamp(SLOT_MIN * r, SLOT_MAX * r).max(keep.min(0.85 * r))
}

/// `k` positions spread evenly over `-a..a` (one in the middle).
fn spread(k: usize, a: f64) -> Vec<f64> {
    if k <= 1 {
        vec![0.0]
    } else {
        (0..k).map(|i| -a + 2.0 * a * i as f64 / (k - 1) as f64).collect()
    }
}

/// Least distance between the axes of neighbouring parallel stubs of nozzles `a` and `b`.
fn pitch(e: &Equip, a: usize, b: usize, lines: &[Line]) -> f64 {
    let ea = route::env_of(&lines[e.nozzles[a].line]);
    let eb = route::env_of(&lines[e.nozzles[b].line]);
    ea + eb + rule("plant.pipe.gap")
}

pub(super) fn shape(eqs: &mut [Equip], i: usize, lines: &[Line]) {
    let e = &mut eqs[i];
    let mut f = F::new(e);
    let side = rule("plant.clear.side");
    match e.kind {
        EqKind::Pump => pump(&mut f, e, lines),
        EqKind::Boiler => boiler(&mut f, e, lines),
        EqKind::Exchanger => exchanger(&mut f, e, lines),
        EqKind::Terminal => terminal(&mut f, e, lines),
        EqKind::Sump => sump(&mut f, e, lines),
        _ => vessel(&mut f, e, lines),
    }
    e.clear = match e.kind {
        EqKind::Pump => {
            let c = rule("plant.clear.pump");
            Clear { front: c, back: c, left: c, right: c }
        }
        EqKind::Boiler => Clear {
            front: rule("plant.clear.boiler_front"),
            back: rule("plant.clear.boiler_rear"),
            left: side,
            right: side,
        },
        EqKind::Exchanger => Clear {
            front: rule("plant.clear.front"),
            back: side,
            left: rule("plant.clear.tube_pull") * e.dims.l,
            right: side,
        },
        EqKind::Terminal | EqKind::Sump => Clear::default(),
        _ => Clear { front: rule("plant.clear.front"), back: side, left: side, right: side },
    };
    e.boxes = f.boxes;
    e.family = f.s;
}

/// Vertical vessels and tanks on legs: shell, heads (ellipsoidal, or a flat bottom and a cone
/// roof for a tank), legs, a manhole, a jacket and drive where the kind has them.
fn vessel(f: &mut F, e: &mut Equip, lines: &[Line]) {
    let d = e.dims.d;
    let r = d / 2.0;
    let h = e.dims.h;
    let tank = matches!(e.kind, EqKind::Tank | EqKind::Dosing);
    let legs = rule("plant.vessel.legs");
    let (hb, ht) = if tank { (0.0, ROOF_K * d) } else { (HEAD_K * d, HEAD_K * d) };
    let z0 = legs + hb;
    let z1 = z0 + h;
    let ztop = z1 + ht;
    let mut prof: Vec<(f64, f64)> = vec![];
    if tank {
        prof.extend([(0.0, legs), (r, legs)]);
    } else {
        for k in 0..=HEAD_N {
            let t = k as f64 / HEAD_N as f64 * FRAC_PI_2;
            prof.push((r * t.sin(), z0 - hb * t.cos()));
        }
    }
    if tank {
        prof.extend([(r, z1), (0.0, ztop)]);
    } else {
        for k in 0..=HEAD_N {
            let t = FRAC_PI_2 - k as f64 / HEAD_N as f64 * FRAC_PI_2;
            prof.push((r * t.sin(), z1 + ht * t.cos()));
        }
    }
    f.revolve("Shell", &prof, "Steel");
    f.boxes.push((v3(-r, -r, legs), v3(r, r, ztop)));
    let leg = (LEG_K * d).max(LEG_MIN);
    let la = LEG_AT * r;
    for (sx, sy) in [(-1.0, -1.0), (1.0, -1.0), (1.0, 1.0), (-1.0, 1.0)] {
        let c = v3(sx * la, sy * la, 0.0);
        f.block("Leg", c - v3(leg / 2.0, leg / 2.0, 0.0), c + v3(leg / 2.0, leg / 2.0, z0), "Steel", true);
    }
    let mut keep = 0.0;
    if !tank && d >= MANHOLE_FROM {
        let zm = (z0 + z1) / 2.0;
        f.cyl("Manhole", v3(0.0, -r * 0.9, zm), v3(0.0, -r - MANHOLE_OUT, zm), MANHOLE_D / 2.0, "Steel", true);
    }
    let mut jacket = None;
    if e.kind == EqKind::Reactor {
        let (ja, jb) = (z0 + JACKET_FROM * h, z0 + JACKET_TO * h);
        f.cyl("Jacket", v3(0.0, 0.0, ja), v3(0.0, 0.0, jb), r + JACKET_GAP, "Steel", true);
        jacket = Some((ja, jb));
    }
    if matches!(e.kind, EqKind::Reactor | EqKind::Dosing) {
        let dr = DRIVE_R.0 + DRIVE_R.1 * d;
        let dh = DRIVE_H.0 + DRIVE_H.1 * d;
        f.cyl("Drive", v3(0.0, 0.0, ztop - EMBED), v3(0.0, 0.0, ztop + dh), dr, "Motor", true);
        keep = dr;
    }
    // top nozzles: a column toward the platform side (+x), a second one opposite if crowded
    let tops: Vec<usize> = (0..e.nozzles.len()).filter(|&k| e.nozzles[k].spot == Spot::Top).collect();
    if !tops.is_empty() {
        let fl_max = tops.iter().map(|&k| nozzle_size(e, k, lines).fl).fold(0.0, f64::max);
        let xs = slot(r, keep + fl_max / 2.0 + NECK);
        let need = tops.windows(2).map(|w| pitch(e, w[0], w[1], lines)).fold(0.0, f64::max);
        let fits = |k: usize| k <= 1 || 2.0 * SPREAD * r / (k - 1) as f64 >= need;
        let cols: Vec<Vec<usize>> = if fits(tops.len()) {
            vec![tops.clone()]
        } else {
            let (a, b): (Vec<(usize, usize)>, Vec<(usize, usize)>) = tops.iter().copied().enumerate().partition(|(k, _)| k % 2 == 0);
            vec![a.into_iter().map(|p| p.1).collect(), b.into_iter().map(|p| p.1).collect()]
        };
        for (c, col) in cols.iter().enumerate() {
            let x = if c == 0 { xs } else { -xs };
            for (k, y) in col.iter().zip(spread(col.len(), SPREAD * r)) {
                let z = nozzle_size(e, *k, lines);
                let rho = (x * x + y * y).sqrt().min(r);
                let zs = if tank { z1 + ht * (1.0 - rho / r) } else { z1 + ht * (1.0 - (rho / r).powi(2)).max(0.0).sqrt() };
                put_nozzle(f, e, *k, lines, v3(x, y, zs), v3(x, y, ztop + z.proj), v3(0.0, 0.0, 1.0));
            }
        }
    }
    // side nozzles by side and level, spread along the shell or stacked if crowded
    let reach = rule("plant.access.reach");
    for spot in [Spot::Left, Spot::Right, Spot::Back] {
        for level in [Level::Low, Level::Mid, Level::High] {
            let ks: Vec<usize> = (0..e.nozzles.len())
                .filter(|&k| e.nozzles[k].spot == spot && e.nozzles[k].level == level && !bottom(e, k))
                .collect();
            if ks.is_empty() {
                continue;
            }
            let (za, zb) = jacket.filter(|_| matches!(spot, Spot::Left | Spot::Right)).unwrap_or((z0, z1));
            let rs = if jacket.is_some() && spot != Spot::Back { r + JACKET_GAP } else { r };
            let need = ks.windows(2).map(|w| pitch(e, w[0], w[1], lines)).fold(0.0, f64::max);
            let fits = ks.len() <= 1 || 2.0 * SPREAD * r / (ks.len() - 1) as f64 >= need;
            for (j, &k) in ks.iter().enumerate() {
                let z = nozzle_size(e, k, lines);
                let m = (SIDE_K * (zb - za)).max(z.fl / 2.0 + NECK);
                let mut zz = match level {
                    Level::Low => za + m,
                    Level::Mid => (za + zb) / 2.0,
                    Level::High => zb - m,
                };
                let t = if fits { spread(ks.len(), SPREAD * r)[j] } else { 0.0 };
                if !fits {
                    zz += match level {
                        Level::High => -(j as f64) * need,
                        _ => j as f64 * need,
                    };
                }
                // an operated valve on a side stub stays within reach of the floor
                let rise = side_rise(e, k, lines);
                if rise > 0.0 {
                    zz = zz.min(reach - rise).max(za + m);
                }
                let along = (rs * rs - t * t).max(0.0).sqrt();
                let (root, face, dir) = match spot {
                    Spot::Left => (v3(-along, t, zz), v3(-rs - z.proj, t, zz), v3(-1.0, 0.0, 0.0)),
                    Spot::Right => (v3(along, t, zz), v3(rs + z.proj, t, zz), v3(1.0, 0.0, 0.0)),
                    _ => (v3(t, along, zz), v3(t, rs + z.proj, zz), v3(0.0, 1.0, 0.0)),
                };
                put_nozzle(f, e, k, lines, root, face, dir);
            }
        }
    }
    for k in 0..e.nozzles.len() {
        if bottom(e, k) {
            let z = nozzle_size(e, k, lines);
            put_nozzle(f, e, k, lines, v3(0.0, 0.0, legs), v3(0.0, 0.0, legs - z.proj), v3(0.0, 0.0, -1.0));
        }
    }
}

/// A low drain leaves from the bottom.
fn bottom(e: &Equip, k: usize) -> bool {
    let n = &e.nozzles[k];
    n.svc == super::Svc::Dr && n.level == Level::Low && n.spot != Spot::Pit
}

/// Boiler shell axis height and the shell, burner and smoke box sizes.
pub(super) struct BoilerDims {
    pub zc: f64,
    pub r: f64,
    pub l: f64,
    pub lb: f64,
    pub bw: f64,
    pub sb: f64,
}

pub(super) fn boiler_dims(e: &Equip) -> BoilerDims {
    let r = e.dims.d / 2.0;
    let flue = e.flue.unwrap_or_default();
    BoilerDims {
        zc: BOILER_BASE + r,
        r,
        l: e.dims.l,
        lb: BURNER_L.0 + BURNER_L.1 * e.dims.d,
        bw: BURNER_W * e.dims.d,
        sb: (flue + SMOKE_PAD).max(SMOKE_MIN),
    }
}

/// Top of the flue spigot of a boiler, local.
pub(super) fn flue_spigot(e: &Equip) -> V3 {
    let b = boiler_dims(e);
    v3(0.0, b.l / 2.0 + b.sb / 2.0, b.zc + b.r + SPIGOT)
}

/// Fire-tube boiler, shell along y: frame, shell, burner at the front, smoke box and flue
/// spigot at the rear; flow and return on top toward the platform side, gas on the burner.
fn boiler(f: &mut F, e: &mut Equip, lines: &[Line]) {
    let b = boiler_dims(e);
    let (r, l) = (b.r, b.l);
    for sx in [-1.0, 1.0] {
        let x = sx * LEG_AT * r;
        f.block("Frame", v3(x - LEG_MIN, -l / 2.0, 0.0), v3(x + LEG_MIN, l / 2.0, BOILER_BASE), "Steel", true);
    }
    f.cyl("Shell", v3(0.0, -l / 2.0, b.zc), v3(0.0, l / 2.0, b.zc), r, "Steel", true);
    f.block("Burner", v3(-b.bw / 2.0, -l / 2.0 - b.lb, b.zc - b.bw / 2.0), v3(b.bw / 2.0, -l / 2.0, b.zc + b.bw / 2.0), "Burner", true);
    f.block("SmokeBox", v3(-r, l / 2.0, BOILER_BASE), v3(r, l / 2.0 + b.sb, b.zc + r), "Steel", true);
    let sp = flue_spigot(e);
    let fd = e.flue.unwrap_or_default();
    f.cyl("Spigot", v3(sp.x, sp.y, b.zc + r - EMBED), sp, fd / 2.0, "Steel", true);
    f.connector("FLUE", "duct", sp, v3(0.0, 0.0, 1.0), fd);
    let x = slot(r, 0.0);
    for k in 0..e.nozzles.len() {
        let z = nozzle_size(e, k, lines);
        let n = &e.nozzles[k];
        match (n.spot, n.level) {
            (Spot::Top, lv) => {
                let y = if lv == Level::High { l / 4.0 } else { -l / 4.0 };
                let zs = b.zc + (r * r - x * x).max(0.0).sqrt();
                put_nozzle(f, e, k, lines, v3(x, y, zs), v3(x, y, b.zc + r + z.proj), v3(0.0, 0.0, 1.0));
            }
            (Spot::Gas, _) => {
                let y = -l / 2.0 - b.lb / 2.0;
                put_nozzle(f, e, k, lines, v3(-b.bw / 2.0, y, b.zc), v3(-b.bw / 2.0 - z.proj, y, b.zc), v3(-1.0, 0.0, 0.0));
            }
            _ => {
                let y = l / 4.0;
                let zs = b.zc - r;
                put_nozzle(f, e, k, lines, v3(0.0, y, zs), v3(0.0, y, BOILER_BASE - z.proj), v3(0.0, 0.0, -1.0));
            }
        }
    }
}

/// End-suction pump along y: plinth, casing with the suction eye at the back (+y) and the
/// discharge on top, coupling guard and motor toward the front.
fn pump(f: &mut F, e: &mut Equip, lines: &[Line]) {
    let dis = e.nozzles.iter().position(|n| n.spot == Spot::Discharge).unwrap_or(0);
    let suc = e.nozzles.iter().position(|n| n.spot == Spot::Suction).unwrap_or(0);
    let rc = (CASING_K * nozzle_size(e, dis, lines).od.max(nozzle_size(e, suc, lines).od)).max(CASING_MIN);
    let dm = MOTOR_D.0 * e.dims.kw.max(size::motor_kw(0.0)).powf(MOTOR_D.1);
    let lm = MOTOR_L * dm;
    let zc = PUMP_BASE + rc.max(dm / 2.0) + PUMP_CLEAR;
    // y from the motor end to the suction eye, centred on the origin
    let total = lm + COUPLING + 2.0 * rc;
    let y0 = -total / 2.0;
    let yc = y0 + lm + COUPLING + rc;
    let w = 2.0 * rc.max(dm / 2.0) + 2.0 * PUMP_CLEAR;
    f.block("Plinth", v3(-w / 2.0, y0, 0.0), v3(w / 2.0, -y0, PUMP_BASE), "Concrete", true);
    f.cyl("Casing", v3(0.0, yc - rc, zc), v3(0.0, yc + rc, zc), rc, "Cast iron", true);
    f.cyl("Motor", v3(0.0, y0, zc), v3(0.0, y0 + lm, zc), dm / 2.0, "Motor", true);
    let g = dm.min(2.0 * rc) / 2.0;
    f.block("Guard", v3(-g, y0 + lm, zc - g), v3(g, y0 + lm + COUPLING, zc + g), "Steel", true);
    let zs = nozzle_size(e, suc, lines);
    put_nozzle(f, e, suc, lines, v3(0.0, yc + rc, zc), v3(0.0, yc + rc + zs.proj, zc), v3(0.0, 1.0, 0.0));
    let zd = nozzle_size(e, dis, lines);
    put_nozzle(f, e, dis, lines, v3(0.0, yc, zc + rc), v3(0.0, yc, zc + rc + zd.proj), v3(0.0, 0.0, 1.0));
}

/// Horizontal shell-and-tube exchanger, shell along x on saddles: channel at -x (the bundle
/// pulls out that way), rear head at +x; nozzles on top lean to the front platform side.
fn exchanger(f: &mut F, e: &mut Equip, lines: &[Line]) {
    let d = e.dims.d;
    let r = d / 2.0;
    let l = e.dims.l;
    let lc = CHANNEL_L.0 + CHANNEL_L.1 * d;
    let lr = REAR_K * d;
    let x0 = -(lc + l + lr) / 2.0;
    let zc = rule("plant.vessel.legs") + r;
    f.cyl("Channel", v3(x0, 0.0, zc), v3(x0 + lc, 0.0, zc), r, "Steel", true);
    f.cyl("Shell", v3(x0 + lc, 0.0, zc), v3(x0 + lc + l, 0.0, zc), r, "Steel", true);
    f.cyl("Rear", v3(x0 + lc + l, 0.0, zc), v3(x0 + lc + l + lr, 0.0, zc), REAR_R * r, "Steel", true);
    for k in [-1.0, 1.0] {
        let x = x0 + lc + l / 2.0 + k * SADDLE_AT * l;
        f.block("Saddle", v3(x - SADDLE_W / 2.0, -SADDLE_Y * r, 0.0), v3(x + SADDLE_W / 2.0, SADDLE_Y * r, zc), "Steel", true);
    }
    let y = -slot(r, 0.0);
    for k in 0..e.nozzles.len() {
        let z = nozzle_size(e, k, lines);
        let off = END_OFF.max(z.fl / 2.0 + NECK);
        let n = &e.nozzles[k];
        let top = |x: f64| (v3(x, y, zc + (r * r - y * y).max(0.0).sqrt()), v3(x, y, zc + r + z.proj), v3(0.0, 0.0, 1.0));
        let (root, face, dir) = match n.name {
            "TUO" => top(x0 + lc / 2.0),
            "SHI" => top(x0 + lc + l - off),
            "SHO" => top(x0 + lc + off),
            "TUI" => (v3(x0 + lc / 2.0, r, zc), v3(x0 + lc / 2.0, r + z.proj, zc), v3(0.0, 1.0, 0.0)),
            _ => {
                let x = x0 + lc + l / 2.0;
                (v3(x, 0.0, zc - r), v3(x, 0.0, zc - r - z.proj), v3(0.0, 0.0, -1.0))
            }
        };
        put_nozzle(f, e, k, lines, root, face, dir);
    }
}

/// Terminal point: a sleeve through the wall (local +y) and the flange inside the hall.
fn terminal(f: &mut F, e: &mut Equip, lines: &[Line]) {
    let z = nozzle_size(e, 0, lines);
    let mat = lines[e.nozzles[0].line].spec.mat();
    f.cyl("Sleeve", v3(0.0, 0.0, 0.0), v3(0.0, WALL_T, 0.0), z.od / 2.0 + SLEEVE_GAP, mat, false);
    put_nozzle(f, e, 0, lines, v3(0.0, 0.0, 0.0), v3(0.0, -z.proj, 0.0), v3(0.0, -1.0, 0.0));
}

/// Inlet depth of the sump below the floor: the drain layer, less the fall the drain takes.
pub(super) fn sump_inlet(e: &Equip, lines: &[Line]) -> f64 {
    -(rule("plant.drain.cover") + route::env_of(&lines[e.nozzles[0].line]) + e.dims.l)
}

/// Drain sump: a pit below the floor with its inlet on the west wall.
fn sump(f: &mut F, e: &mut Equip, lines: &[Line]) {
    let zi = sump_inlet(e, lines);
    let z = nozzle_size(e, 0, lines);
    let a = SUMP_A.max(z.fl);
    let bot = zi - SUMP_BELOW;
    let t = SUMP_WALL;
    f.block("Pit", v3(-a - t, -a - t, bot - t), v3(a + t, a + t, 0.0), "Concrete", true);
    let n = f.name("Void");
    let _ = writeln!(f.s, "extrude {n} rect({}, {}, {}, {}) z({}, {}) void", mm(-a), mm(-a), mm(a), mm(a), mm(bot), mm(0.0));
    put_nozzle(f, e, 0, lines, v3(-a - t, 0.0, zi), v3(-a - t - z.proj, 0.0, zi), v3(-1.0, 0.0, 0.0));
}
