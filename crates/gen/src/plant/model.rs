// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! The plant as model elements: a family and a placement for every unit, every item on a
//! nozzle stack (reducers, valves, strainers, meters, gauges), every pipe support, the steel of
//! each platform, stair, ladder and support, and each flue; the pipes as straight pipes of one
//! size, cut at every bend, tee and reducer and at the longest standard length, meeting end to
//! end and at the nozzle stacks' connectors.

use std::collections::BTreeMap;
use std::f64::consts::PI;

use ddc_geom::math::{v3, V3};
use ddc_geom::mep::SystemKind;

use crate::project::rules::rule;

use super::equip::F;
use super::layout::{Lay, STEEL_R};
use super::route::Piping;
use super::size::{self, ItemKind, Spec};
use super::topo::Topo;
use super::{End, SteelKind, Svc};

/// Two points closer than this are one node, m.
const NODE: f64 = 1e-4;
/// Wall of a clamp band and of a toe board, as a share of the steel envelope radius.
const PLATE: f64 = 1.0 / 3.0;

/// A family the plant places; `own` families belong to this plant alone (its units, steel and
/// flues) and are renamed per plant, the others (stack items, clamps) are shared by size.
#[derive(Clone, Debug, PartialEq)]
pub struct KitFamily {
    pub name: String,
    pub text: String,
    pub own: bool,
}

/// One placed element: its family, insertion point (z from the floor), turn about z, and the
/// connectors that join a line.
#[derive(Clone, Debug, PartialEq)]
pub struct KitComp {
    /// Stable within a plant: the same unit or item keeps it across revisions.
    pub key: String,
    pub family: String,
    pub at: V3,
    /// Radians, counterclockwise.
    pub rotation: f64,
    pub ports: Vec<(String, usize)>,
}

/// One straight pipe of one size (z from the floor).
#[derive(Clone, Debug, PartialEq)]
pub struct KitRun {
    pub line: usize,
    pub dn: u32,
    /// Two points: one straight pipe of at most `plant.pipe.max_len`; runs of one line meet at
    /// their ends, at the bends, tees, reducers and the nozzle stacks.
    pub points: Vec<V3>,
    /// Fall in % along the points when the pipe it was cut from falls evenly, else 0.
    pub fall: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Kit {
    pub families: Vec<KitFamily>,
    pub comps: Vec<KitComp>,
    pub runs: Vec<KitRun>,
    /// Load-bearing steel: platform and stand posts, deck and rack beams, rack portals.
    pub cols: Vec<KitMember>,
    pub beams: Vec<KitMember>,
    /// Containment walls (a bund round liquid storage), straight, from the floor.
    pub walls: Vec<KitWall>,
    /// Strip footings below the floor: the ring under a storage tank's shell.
    pub footings: Vec<KitFooting>,
}

/// A column (`a` at its foot, `b` at its top) or a beam by its axis.
#[derive(Clone, Debug, PartialEq)]
pub struct KitMember {
    pub key: String,
    pub a: V3,
    pub b: V3,
}

/// A straight wall by its two ends, with its height.
#[derive(Clone, Debug, PartialEq)]
pub struct KitWall {
    pub key: String,
    pub a: V3,
    pub b: V3,
    pub height: f64,
}

/// A strip footing along `a`..`b` (an arc where `bulge`, tan of a quarter of its angle, is
/// not 0, counterclockwise when positive), its top at `a.z` from the floor.
#[derive(Clone, Debug, PartialEq)]
pub struct KitFooting {
    pub key: String,
    pub a: V3,
    pub b: V3,
    pub bulge: f64,
    pub width: f64,
    pub depth: f64,
}

/// The model's system kind of a service. The model has no fuel gas or process kinds: gas goes
/// as heating (the burners it feeds), process and chemical lines as cold water; the line
/// number and spec keep the service.
pub fn system_kind(s: Svc) -> SystemKind {
    match s {
        Svc::Hws | Svc::Hwr | Svc::Ng => SystemKind::Heating,
        Svc::Dr => SystemKind::Sanitary,
        Svc::Mw | Svc::Rw | Svc::Tw | Svc::Bw | Svc::Ch | Svc::Pr | Svc::Cws | Svc::Cwr => SystemKind::ColdWater,
    }
}

/// Which way a stack piece runs: along +x turned about z, straight up, or straight down.
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
enum Orient {
    H,
    U,
    D,
}

impl Orient {
    fn of(d: V3) -> (Orient, f64) {
        if d.z > 0.5 {
            (Orient::U, 0.0)
        } else if d.z < -0.5 {
            (Orient::D, 0.0)
        } else {
            (Orient::H, d.y.atan2(d.x))
        }
    }
    fn axis(self) -> V3 {
        match self {
            Orient::H => v3(1.0, 0.0, 0.0),
            Orient::U => v3(0.0, 0.0, 1.0),
            Orient::D => v3(0.0, 0.0, -1.0),
        }
    }
    /// Out of the pipe toward the handwheel or dial.
    fn up(self) -> V3 {
        match self {
            Orient::H => v3(0.0, 0.0, 1.0),
            _ => v3(1.0, 0.0, 0.0),
        }
    }
    fn code(self) -> &'static str {
        match self {
            Orient::H => "H",
            Orient::U => "U",
            Orient::D => "D",
        }
    }
}

fn key(p: V3) -> (i64, i64, i64) {
    let k = |x: f64| (x / NODE).round() as i64;
    (k(p.x), k(p.y), k(p.z))
}

/// A cylinder along a family axis from `a` (on it) for `len`, radius `r`.
fn along(f: &mut F, base: &str, a: V3, axis: V3, len: f64, r: f64, mat: &str) {
    if len > 1e-9 && r > 1e-9 {
        f.cyl(base, a, a + axis * len, r, mat, false);
    }
}

/// A stack piece's family: a reducer (`item` None) from `dn_a` to `dn_b`, or an item of `dn_b`.
fn piece_family(item: Option<ItemKind>, dn_a: u32, dn_b: u32, spec: Spec, pn: u32, o: Orient, len: f64) -> KitFamily {
    let code = item.map_or("RED", |k| k.code());
    // the name says what it is, so a search finds valves and flanged joints by their words
    let what = item.map_or("reducer".to_string(), |k| k.name().replace(' ', "_"));
    let flanged = if item.is_some_and(|k| k.flanges() >= 1) { "_flanged" } else { "" };
    let name = format!("Plant_{what}{flanged}_{code}_{}_DN{dn_a}x{dn_b}_PN{pn}_{}", spec.code(), o.code());
    let ifc = item.map_or("IfcPipeFitting TRANSITION", |k| k.ifc());
    let mut f = F::named(&name, "equipment", ifc);
    let mat = spec.mat();
    let (oa, ob) = (size::pipe(spec, dn_a).od, size::pipe(spec, dn_b).od);
    let ax = o.axis();
    let up = o.up();
    let fl = size::flange_od(dn_b, pn);
    let ft = size::flange_t(dn_b);
    let zero = v3(0.0, 0.0, 0.0);
    match item {
        None => {
            // stepped halves: the line keeps one axis through a concentric body
            along(&mut f, "Big", zero, ax, len / 2.0, oa / 2.0, mat);
            along(&mut f, "Small", ax * (len / 2.0), ax, len / 2.0, ob / 2.0, mat);
        }
        Some(k) if len > 1e-9 => {
            let flanged = k.flanges();
            let body_r = (ob / 2.0).max(fl * 0.35);
            if flanged >= 1 {
                along(&mut f, "Flange", zero, ax, ft, fl / 2.0, mat);
            }
            if flanged >= 2 {
                along(&mut f, "Flange", ax * (len - ft), ax, ft, fl / 2.0, mat);
            }
            let inner = if flanged >= 2 { len - 2.0 * ft } else { len - ft };
            let start = if flanged >= 1 { ax * ft } else { zero };
            along(&mut f, "Body", start, ax, inner.max(len * 0.2), if flanged == 0 { fl / 2.0 } else { body_r }, mat);
            let mid = ax * (len / 2.0);
            if k.operated() {
                let stem = k.stem(fl);
                along(&mut f, "Stem", mid, up, stem, ob / 8.0, "Steel");
                along(&mut f, "Handwheel", mid + up * stem, up, ft, stem / 3.0, "Steel");
            } else if k == ItemKind::Psv {
                along(&mut f, "Outlet", mid, up, fl, ob / 2.0, mat);
            }
        }
        // a tapping: the gauge or thermometer stands on the pipe, the pipe runs through
        Some(k) => {
            let stem = k.stem(fl);
            along(&mut f, "Tapping", zero, up, stem, ob / 6.0, "Steel");
            along(&mut f, "Dial", up * stem, up, ft, fl / 3.0, "Steel");
        }
    }
    if len > 1e-9 {
        f.connector("A", "pipe", zero, ax * -1.0, oa);
        f.connector("B", "pipe", ax * len, ax, ob);
    }
    KitFamily { name, text: with_type(f.s), own: false }
}

pub(super) fn with_type(mut s: String) -> String {
    s.push_str("type Standard\n");
    s
}

/// The stack on each nozzle as placed pieces, and where each nozzle's pipe starts.
fn stacks(t: &Topo, kit: &mut Kit, fams: &mut BTreeMap<String, KitFamily>) -> BTreeMap<End, (V3, V3, f64)> {
    let mut starts = BTreeMap::new();
    for (li, l) in t.lines.iter().enumerate() {
        for e in l.all_ends() {
            let eq = &t.eqs[e.eq];
            let n = &eq.nozzles[e.nz];
            let (fa, fd) = eq.nozzle_world(e.nz);
            let (o, rotation) = Orient::of(fd);
            let pieces = size::stack(n.dn, n.pipe_dn, &n.items, l.spec, l.pn);
            let mut total: f64 = 0.0;
            for (k, s) in pieces.iter().enumerate() {
                let dn_a = if s.item.is_none() { n.dn } else { s.dn };
                let fam = piece_family(s.item, dn_a, s.dn, l.spec, l.pn, o, s.len);
                let ports = if s.len > 1e-9 { vec![("A".to_string(), li), ("B".to_string(), li)] } else { vec![] };
                kit.comps.push(KitComp {
                    key: format!("stack:{}:{}:{k}", eq.tag, n.name),
                    family: fam.name.clone(),
                    at: fa + fd * s.at,
                    rotation,
                    ports,
                });
                fams.entry(fam.name.clone()).or_insert(fam);
                total = total.max(s.at + s.len);
            }
            starts.insert(e, (fa, fd, total));
        }
    }
    starts
}

/// The corners of a polyline: points on a straight between their neighbours dropped.
fn straight(pts: Vec<V3>) -> Vec<V3> {
    let mut out: Vec<V3> = Vec::with_capacity(pts.len());
    for p in pts {
        if let [.., a, b] = out.as_slice() {
            let (u, w) = (*b - *a, p - *b);
            let cross = v3(u.y * w.z - u.z * w.y, u.z * w.x - u.x * w.z, u.x * w.y - u.y * w.x);
            if cross.len() < 1e-9 * u.len().max(1.0) * w.len().max(1.0) && u.dot(w) > 0.0 {
                out.pop();
            }
        }
        out.push(p);
    }
    out
}

/// The fall, % down along the points, when every horizontal piece falls by the same amount;
/// zero for a pipe that is level or falls unevenly.
fn fall_pct(pts: &[V3]) -> f64 {
    let mut pct: Option<f64> = None;
    for w in pts.windows(2) {
        let run = (w[1].x - w[0].x).hypot(w[1].y - w[0].y);
        let dz = w[1].z - w[0].z;
        if run < 1e-6 || dz.abs() > run {
            continue;
        }
        let f = -dz / run * 100.0;
        match pct {
            None => pct = Some(f),
            Some(p) if (p - f).abs() > 1e-3 => return 0.0,
            _ => {}
        }
    }
    pct.filter(|p| *p > 1e-6).unwrap_or(0.0)
}

/// A chained run as straight pipes: one per leg between bends, a long leg in equal pieces of
/// at most `plant.pipe.max_len`. Neighbours share their end point exactly, so the network
/// joins them as fittings.
fn cut(r: KitRun, out: &mut Vec<KitRun>) {
    let max = rule("plant.pipe.max_len");
    let whole = fall_pct(&r.points);
    for w in r.points.windows(2) {
        let (a, b) = (w[0], w[1]);
        let n = (((b - a).len() / max) - 1e-9).ceil().max(1.0) as usize;
        let at = |k: usize| if k == n { b } else { a + (b - a) * (k as f64 / n as f64) };
        for k in 0..n {
            let points = vec![at(k), at(k + 1)];
            let fall = if whole > 0.0 { fall_pct(&points) } else { 0.0 };
            out.push(KitRun { line: r.line, dn: r.dn, points, fall });
        }
    }
}

/// The pipes of each line as runs: stubs cut back to their stacks, then chained through every
/// point two pieces of one size share.
fn runs(t: &Topo, p: &Piping, starts: &BTreeMap<End, (V3, V3, f64)>) -> Result<Vec<KitRun>, String> {
    let mut out = vec![];
    for li in 0..t.lines.len() {
        let mut segs: Vec<(V3, V3, u32)> = vec![];
        for s in p.segs.iter().filter(|s| s.line == li) {
            let (mut a, mut b) = (s.a, s.b);
            if let Some(e) = s.stub {
                let (fa, fd, total) = starts[&e];
                let cut = fa + fd * total;
                if (a - fa).len() < NODE {
                    a = cut;
                } else if (b - fa).len() < NODE {
                    b = cut;
                } else {
                    return Err(format!("the stub of {} does not start at its nozzle", t.lines[li].number()));
                }
                if (b - a).dot(if (s.a - fa).len() < NODE { fd } else { fd * -1.0 }) < -NODE {
                    return Err(format!("the stack on line {} is longer than its stub", t.lines[li].number()));
                }
            }
            if (b - a).len() > NODE {
                segs.push((a, b, s.dn));
            }
        }
        let mut at: BTreeMap<(i64, i64, i64), Vec<usize>> = BTreeMap::new();
        for (i, s) in segs.iter().enumerate() {
            at.entry(key(s.0)).or_default().push(i);
            at.entry(key(s.1)).or_default().push(i);
        }
        let through = |k: &(i64, i64, i64)| {
            let v = &at[k];
            v.len() == 2 && segs[v[0]].2 == segs[v[1]].2
        };
        let mut used = vec![false; segs.len()];
        let walk = |from: (i64, i64, i64), first: usize, used: &mut Vec<bool>| {
            let mut pts = vec![];
            let (mut node, mut i) = (from, first);
            let start = if key(segs[i].0) == node { segs[i].0 } else { segs[i].1 };
            pts.push(start);
            loop {
                used[i] = true;
                let s = segs[i];
                let next = if key(s.0) == node { s.1 } else { s.0 };
                pts.push(next);
                node = key(next);
                if !through(&node) {
                    break;
                }
                match at[&node].iter().copied().find(|&j| !used[j]) {
                    Some(j) => i = j,
                    None => break,
                }
            }
            KitRun { line: li, dn: segs[first].2, points: straight(pts), fall: 0.0 }
        };
        let ends: Vec<(i64, i64, i64)> = at.keys().copied().filter(|k| !through(k)).collect();
        for n in ends {
            for &i in &at[&n].clone() {
                if !used[i] {
                    let r = walk(n, i, &mut used);
                    cut(r, &mut out);
                }
            }
        }
        // a closed loop has no end: none is routed, but nothing is left behind if one were
        for i in 0..segs.len() {
            if !used[i] {
                let r = walk(key(segs[i].0), i, &mut used);
                cut(r, &mut out);
            }
        }
    }
    Ok(out)
}

/// The part of an owner's steel a family holds: decks, handrails, ladders, stairs, hanger rods.
/// Posts and beams are not in a family: they are columns and beams of the model.
fn steel_part(k: SteelKind, platform: bool) -> Option<&'static str> {
    Some(match k {
        SteelKind::Post | SteelKind::Beam => return None,
        SteelKind::Rail if platform => "platform_handrail",
        SteelKind::KneeRail if platform => "platform_knee_rail",
        SteelKind::Rail | SteelKind::KneeRail => "handrail",
        SteelKind::Toe if platform => "platform_toe_board",
        SteelKind::Toe => "platform_deck",
        SteelKind::Stile | SteelKind::Rung | SteelKind::Hoop => "ladder",
        SteelKind::Stringer | SteelKind::Tread => "stair",
        SteelKind::Rod => "hanger_rod",
    })
}

/// The steel of each owner (a platform with its access, the rack, a line's supports): posts
/// and beams as columns and beams, the rest as one family per part placed at the origin, with
/// platform decks and toe boards as plates.
fn steel(lay: &Lay, kit: &mut Kit, fams: &mut BTreeMap<String, KitFamily>) {
    let mut by: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
    for (i, s) in lay.steel.iter().enumerate() {
        by.entry(s.of.as_str()).or_default().push(i);
    }
    for p in &lay.platforms {
        by.entry(p.tag.as_str()).or_default();
    }
    for (of, members) in by {
        let plat = lay.platforms.iter().find(|p| p.tag == of);
        let tag = of.replace(['-', ' '], "_");
        for (n, &i) in members.iter().enumerate() {
            let s = &lay.steel[i];
            if (s.b - s.a).len() <= NODE {
                continue;
            }
            let key = format!("{tag}:{n}");
            match s.kind {
                SteelKind::Post => kit.cols.push(KitMember { key: format!("col:{key}"), a: s.a, b: s.b }),
                SteelKind::Beam => kit.beams.push(KitMember { key: format!("beam:{key}"), a: s.a, b: s.b }),
                _ => {}
            }
        }
        let mut parts: BTreeMap<&str, Vec<usize>> = BTreeMap::new();
        for &i in members.iter().filter(|&&i| (lay.steel[i].b - lay.steel[i].a).len() > NODE) {
            if let Some(part) = steel_part(lay.steel[i].kind, plat.is_some()) {
                parts.entry(part).or_default().push(i);
            }
        }
        if plat.is_some() {
            parts.entry("platform_deck").or_default();
        }
        for (part, ids) in parts {
            let name = format!("{part}_{tag}");
            let mut f = F::named(&name, "generic", "IfcMember NOTDEFINED");
            if let (Some(p), "platform_deck") = (plat, part) {
                f.block("Deck", v3(p.lo.x, p.lo.y, p.deck - 2.0 * STEEL_R), v3(p.hi.x, p.hi.y, p.deck), "Steel", false);
            }
            let treads: Vec<V3> = ids.iter().map(|&i| &lay.steel[i]).filter(|s| s.kind == SteelKind::Tread).map(|s| (s.a + s.b) * 0.5).collect();
            let going = treads.windows(2).map(|w| v3(w[1].x - w[0].x, w[1].y - w[0].y, 0.0).len()).fold(f64::INFINITY, f64::min);
            for &i in &ids {
                let s = &lay.steel[i];
                match s.kind {
                    SteelKind::Toe => {
                        let deck = plat.map_or(s.a.z.min(s.b.z), |p| p.deck);
                        let t = STEEL_R * PLATE;
                        let n = v3(-(s.b - s.a).y, (s.b - s.a).x, 0.0).normalized() * t;
                        f.block("Toe", v3(s.a.x, s.a.y, deck) - n, v3(s.b.x, s.b.y, s.a.z) + n, "Steel", false);
                    }
                    SteelKind::Tread if going.is_finite() => {
                        let u = s.b - s.a;
                        let n = v3(-u.y, u.x, 0.0).normalized() * (going / 2.0);
                        f.block("Tread", s.a - n - v3(0.0, 0.0, 2.0 * STEEL_R), s.b + n, "Steel", false);
                    }
                    k if (s.b - s.a).len() > NODE => f.bar(k.name().replace(' ', "_").as_str(), s.a, s.b, STEEL_R, "Steel"),
                    _ => {}
                }
            }
            let fam = KitFamily { name: name.clone(), text: with_type(f.s), own: true };
            kit.comps.push(KitComp { key: format!("steel:{part}:{of}"), family: name.clone(), at: v3(0.0, 0.0, 0.0), rotation: 0.0, ports: vec![] });
            fams.insert(name, fam);
        }
    }
}

/// A clamp on each support: a band round the pipe envelope along the pipe.
fn supports(p: &Piping, kit: &mut Kit, fams: &mut BTreeMap<String, KitFamily>) {
    for (i, s) in p.supports.iter().enumerate() {
        let along_pipe = p
            .segs
            .iter()
            .filter(|g| g.line == s.line)
            .map(|g| (g, super::check::seg_box(g.a, g.b, s.at, s.at)))
            .min_by(|x, y| x.1.total_cmp(&y.1))
            .map_or(v3(1.0, 0.0, 0.0), |(g, _)| g.b - g.a);
        let (ax, axis) = if along_pipe.z.abs() > along_pipe.x.abs().max(along_pipe.y.abs()) {
            ("Z", v3(0.0, 0.0, 1.0))
        } else if along_pipe.x.abs() >= along_pipe.y.abs() {
            ("X", v3(1.0, 0.0, 0.0))
        } else {
            ("Y", v3(0.0, 1.0, 0.0))
        };
        let band = STEEL_R * PLATE;
        let name = format!("Plant_Support_{}_E{}_{ax}", s.kind.name().replace(' ', "_"), (s.env * 1000.0).round() as i64);
        let fam = fams.entry(name.clone()).or_insert_with(|| {
            let mut f = F::named(&name, "generic", "IfcPipeFitting NOTDEFINED");
            let a = axis * -STEEL_R;
            f.cyl("Band", a, a + axis * (2.0 * STEEL_R), s.env + band, "Steel", false);
            KitFamily { name: name.clone(), text: with_type(f.s), own: false }
        });
        kit.comps.push(KitComp { key: format!("support:{i}"), family: fam.name.clone(), at: s.at, rotation: 0.0, ports: vec![] });
    }
}

/// Every element of a designed plant.
pub(super) fn of(t: &Topo, lay: &Lay, p: &Piping) -> Result<Kit, String> {
    let mut kit = Kit::default();
    let mut fams: BTreeMap<String, KitFamily> = BTreeMap::new();
    for e in &t.eqs {
        let name = e.family_name();
        let ports = e.nozzles.iter().map(|n| (n.name.to_string(), n.line)).collect();
        kit.comps.push(KitComp {
            key: format!("unit:{}", e.tag),
            family: name.clone(),
            at: e.at,
            rotation: f64::from(e.rot % 4) * PI / 2.0,
            ports,
        });
        fams.insert(name.clone(), KitFamily { name, text: with_type(e.family.clone()), own: true });
    }
    let starts = stacks(t, &mut kit, &mut fams);
    kit.runs = runs(t, p, &starts)?;
    supports(p, &mut kit, &mut fams);
    steel(lay, &mut kit, &mut fams);
    if let Some(b) = &lay.bund {
        let c = [v3(b.lo.x, b.lo.y, 0.0), v3(b.hi.x, b.lo.y, 0.0), v3(b.hi.x, b.hi.y, 0.0), v3(b.lo.x, b.hi.y, 0.0)];
        for k in 0..4 {
            kit.walls.push(KitWall { key: format!("wall:bund:{k}"), a: c[k], b: c[(k + 1) % 4], height: b.h });
        }
    }
    for fl in &lay.flues {
        let name = format!("Flue_{}", fl.tag.replace('-', "_"));
        let mut f = F::named(&name, "generic", "IfcChimney NOTDEFINED");
        f.cyl("Flue", fl.at, v3(fl.at.x, fl.at.y, fl.top), fl.d / 2.0, "Steel", false);
        kit.comps.push(KitComp { key: format!("flue:{}", fl.tag), family: name.clone(), at: v3(0.0, 0.0, 0.0), rotation: 0.0, ports: vec![] });
        fams.insert(name.clone(), KitFamily { name, text: with_type(f.s), own: true });
    }
    kit.families = fams.into_values().collect();
    Ok(kit)
}
