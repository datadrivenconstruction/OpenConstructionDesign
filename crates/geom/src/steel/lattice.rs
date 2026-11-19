//! Lattice frames: a compact node/member graph (16 bytes per member) with truss and box-lattice
//! generators (Warren, Pratt, K, X bracing; curved chords; tapering), joint derivation with
//! riveted or bolted gussets, and fastener expansion into instanced sets.

use super::rivet::{self, FastInst, FastenerSet, RivetFinding, RivetGroup};
use super::sections;
use crate::math::{v3, V3};
use std::collections::HashMap;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
#[repr(u8)]
pub enum MemRole {
    Chord,
    Diagonal,
    Strut,
    PlanBrace,
    Girder,
    Beam,
    Column,
    Arch,
    /// A guy or stay: tension only, a round bar or strand to a ground anchor.
    Cable,
}

impl MemRole {
    pub fn key(self) -> &'static str {
        match self {
            MemRole::Chord => "chord",
            MemRole::Diagonal => "diagonal",
            MemRole::Strut => "strut",
            MemRole::PlanBrace => "plan_brace",
            MemRole::Girder => "girder",
            MemRole::Beam => "beam",
            MemRole::Column => "column",
            MemRole::Arch => "arch",
            MemRole::Cable => "cable",
        }
    }
    /// Built-up members carry stitch rivets along their length.
    pub fn built_up(self) -> bool {
        matches!(self, MemRole::Chord | MemRole::Girder | MemRole::Arch | MemRole::Diagonal)
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(C)]
pub struct Mem {
    pub a: u32,
    pub b: u32,
    pub sec: u16,
    pub role: MemRole,
    /// Plane normal of the lattice face the member lies in, i8-quantised (gusset plane).
    pub n: [i8; 3],
    _pad: u8,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pattern {
    Warren,
    Pratt,
    K,
    X,
}

impl Pattern {
    pub fn parse(s: &str) -> Option<Pattern> {
        match s.trim() {
            "warren" => Some(Pattern::Warren),
            "pratt" => Some(Pattern::Pratt),
            "k" | "K" => Some(Pattern::K),
            "x" | "X" => Some(Pattern::X),
            _ => None,
        }
    }
}

/// Section names per role of one lattice.
#[derive(Clone, Copy, Debug)]
pub struct Secs<'a> {
    pub chord: &'a str,
    pub diag: &'a str,
    pub strut: &'a str,
}

/// A masonry pier under one leg: a truncated prism, widest at its foot.
#[derive(Clone, Debug, PartialEq)]
pub struct Pier {
    pub x: f64,
    pub y: f64,
    pub z_top: f64,
    pub z_foot: f64,
    pub half_top: f64,
    pub half_foot: f64,
}

/// A guard rail run: points (x, y, z from the level), height of the top rail, post spacing and
/// infill spacing; the model builds it as a railing element.
#[derive(Clone, Debug, PartialEq)]
pub struct Guard {
    pub path: Vec<(f64, f64, f64)>,
    pub height: f64,
    pub post_spacing: f64,
    pub infill_spacing: f64,
}

/// A stair up to the deck with its guards on both sides (spacings as for a `Guard`).
#[derive(Clone, Debug, PartialEq)]
pub struct Stairway {
    pub def: crate::stair::StairDef,
    pub height: f64,
    pub post_spacing: f64,
    pub infill_spacing: f64,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct Frame {
    /// Masonry piers under the legs; the model builds them as footings.
    pub piers: Vec<Pier>,
    /// Guard rails the generator places along walkways; the model builds them as railings.
    pub guards: Vec<Guard>,
    /// Approach ramps (z from the level); the model builds them as ramps.
    pub ramps: Vec<crate::ramp::RampDef>,
    /// Approach stairs where a ramp would be too long; the model builds them as stairs.
    pub stairs: Vec<Stairway>,
    pub nodes: Vec<V3>,
    pub members: Vec<Mem>,
    pub sections: Vec<String>,
    index: HashMap<(i64, i64, i64), u32>,
    pairs: HashMap<(u32, u32), u32>,
    /// Members refused as shorter than 1 mm.
    pub refused_zero: usize,
    /// Floors the frame carries, on its floor beams.
    pub decks: Vec<Deck>,
    /// Stair flights inside the frame, bottom up.
    pub flights: Vec<Flight>,
    /// Mast lifts: a vertical run each.
    pub lifts: Vec<LiftRun>,
    /// Insulator strings hanging from the cross-arms of a power-line tower.
    pub insulators: Vec<InsulatorString>,
    /// The rising chords carry the frame down to the ground (tower legs): columns, not bracing.
    pub chords_are_columns: bool,
}

/// A square floor centred on the frame axis: top at `z`, half-width `half`, an open centre of
/// half-width `open` (0: none) and openings (x0, y0, x1, y1) where flights pass.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct Deck {
    pub z: f64,
    pub half: f64,
    pub open: f64,
    pub holes: Vec<[f64; 4]>,
}

/// A two-flight (U) stair from `z0` to `z1`: plan box (x0, y0, x1, y1), rising along +x or -x.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Flight {
    pub z0: f64,
    pub z1: f64,
    pub plan: [f64; 4],
    pub up_x: f64,
}

/// An insulator string hanging from `(at, z)` (its top) down `length`.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct InsulatorString {
    pub at: (f64, f64),
    pub z: f64,
    pub length: f64,
}

/// A mast lift from `z0` to `z1`, the car and mast centred at `at` inside the plan box
/// (x0, y0, x1, y1), the mast on the +y side.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct LiftRun {
    pub at: (f64, f64),
    pub z0: f64,
    pub z1: f64,
    pub plan: [f64; 4],
}

/// Tread, riser limit, flight width and gap between the two flights of a [`Flight`], m.
pub const FLIGHT: (f64, f64, f64, f64) = (0.28, 0.18, 1.2, 0.2);

impl Flight {
    /// Plan box of a U stair centred at `c` for a rise `h`, rising along `dir` (+1 or -1 on x).
    pub fn at(c: (f64, f64), z0: f64, z1: f64, dir: f64) -> Flight {
        let (tread, riser, width, gap) = FLIGHT;
        let risers = ((z1 - z0) / riser).ceil();
        let run = (risers / 2.0).ceil() * tread + width;
        let side = 2.0 * width + gap;
        Flight { z0, z1, plan: [c.0 - run / 2.0, c.1 - side / 2.0, c.0 + run / 2.0, c.1 + side / 2.0], up_x: dir.signum() }
    }
}

fn key(p: V3) -> (i64, i64, i64) {
    ((p.x * 1e3).round() as i64, (p.y * 1e3).round() as i64, (p.z * 1e3).round() as i64)
}

fn qn(n: V3) -> [i8; 3] {
    let l = n.len();
    if l < 1e-12 {
        return [0, 0, 0];
    }
    let q = |x: f64| ((x / l) * 127.0).round() as i8;
    [q(n.x), q(n.y), q(n.z)]
}

pub fn lerp(a: V3, b: V3, t: f64) -> V3 {
    a + (b - a) * t
}

impl Frame {
    pub fn new() -> Frame {
        Frame::default()
    }

    pub fn node(&mut self, p: V3) -> u32 {
        let k = key(p);
        if let Some(&i) = self.index.get(&k) {
            return i;
        }
        let i = self.nodes.len() as u32;
        self.nodes.push(p);
        self.index.insert(k, i);
        i
    }

    pub fn sec(&mut self, name: &str) -> u16 {
        if let Some(i) = self.sections.iter().position(|s| s == name) {
            return i as u16;
        }
        self.sections.push(name.to_string());
        (self.sections.len() - 1) as u16
    }

    /// Adds a member between two points; duplicates and zero-length members are skipped.
    pub fn add(&mut self, a: V3, b: V3, sec: &str, role: MemRole, normal: V3) -> Option<u32> {
        if (b - a).len() < 1e-3 {
            self.refused_zero += 1;
            return None;
        }
        let (ia, ib) = (self.node(a), self.node(b));
        if ia == ib {
            self.refused_zero += 1;
            return None;
        }
        let pk = (ia.min(ib), ia.max(ib));
        if let Some(&m) = self.pairs.get(&pk) {
            return Some(m);
        }
        let s = self.sec(sec);
        let m = self.members.len() as u32;
        self.members.push(Mem { a: ia, b: ib, sec: s, role, n: qn(normal), _pad: 0 });
        self.pairs.insert(pk, m);
        Some(m)
    }

    pub fn polyline(&mut self, pts: &[V3], sec: &str, role: MemRole, normal: V3) {
        for w in pts.windows(2) {
            self.add(w[0], w[1], sec, role, normal);
        }
    }

    /// Bracing of one quadrilateral panel: chord A from a0 to a1, chord B from b0 to b1
    /// (chords themselves are added by the caller). `odd` flips the Warren direction.
    pub fn panel(&mut self, a0: V3, a1: V3, b0: V3, b1: V3, pat: Pattern, odd: bool, s: Secs, strut_end: bool) {
        let n = (a1 - a0).cross(b0 - a0);
        let d = s.diag;
        match pat {
            Pattern::Warren => {
                if odd {
                    self.add(a1, b0, d, MemRole::Diagonal, n);
                } else {
                    self.add(a0, b1, d, MemRole::Diagonal, n);
                }
            }
            Pattern::Pratt => {
                self.add(a0, b1, d, MemRole::Diagonal, n);
                self.add(a0, b0, s.strut, MemRole::Strut, n);
            }
            Pattern::K => {
                // Chevron in a closed panel: both diagonals meet the middle of the top strut.
                let m = lerp(a1, b1, 0.5);
                self.add(a0, m, d, MemRole::Diagonal, n);
                self.add(b0, m, d, MemRole::Diagonal, n);
                if strut_end {
                    self.add(a1, m, s.strut, MemRole::Strut, n);
                    self.add(m, b1, s.strut, MemRole::Strut, n);
                    return;
                }
            }
            Pattern::X => {
                let c = lerp(lerp(a0, b0, 0.5), lerp(a1, b1, 0.5), 0.5);
                self.add(a0, c, d, MemRole::Diagonal, n);
                self.add(c, b1, d, MemRole::Diagonal, n);
                self.add(b0, c, d, MemRole::Diagonal, n);
                self.add(c, a1, d, MemRole::Diagonal, n);
            }
        }
        if strut_end {
            self.add(a1, b1, s.strut, MemRole::Strut, n);
        }
    }

    /// Planar truss between two chord polylines of equal point count (curved chords allowed).
    pub fn truss(&mut self, ca: &[V3], cb: &[V3], pat: Pattern, s: Secs, chord_role: MemRole) {
        assert_eq!(ca.len(), cb.len(), "truss chords need the same panel points");
        let n = if ca.len() > 1 { (ca[1] - ca[0]).cross(cb[0] - ca[0]) } else { v3(0.0, 0.0, 1.0) };
        if pat == Pattern::K {
            // K chords run through the mid nodes.
            let mut a2 = vec![ca[0]];
            for w in ca.windows(2) {
                a2.push(lerp(w[0], w[1], 0.5));
                a2.push(w[1]);
            }
            self.polyline(&a2, s.chord, chord_role, n);
        } else {
            self.polyline(ca, s.chord, chord_role, n);
        }
        self.polyline(cb, s.chord, chord_role, n);
        self.add(ca[0], cb[0], s.strut, MemRole::Strut, n);
        for i in 0..ca.len() - 1 {
            if pat == Pattern::K {
                let ma = lerp(ca[i], ca[i + 1], 0.5);
                self.add(ma, cb[i], s.diag, MemRole::Diagonal, n);
                self.add(ma, cb[i + 1], s.diag, MemRole::Diagonal, n);
                self.add(ca[i + 1], cb[i + 1], s.strut, MemRole::Strut, n);
            } else {
                self.panel(ca[i], ca[i + 1], cb[i], cb[i + 1], pat, i % 2 == 1, s, true);
            }
        }
    }

    /// Four-chord box lattice from levels of four corners (counter-clockwise): chords, one
    /// lattice per face, and a plan diagonal every `plan_every` levels.
    pub fn box_lattice(&mut self, levels: &[[V3; 4]], pat: Pattern, s: Secs, plan_every: usize, plan_sec: &str) {
        for c in 0..4 {
            let line: Vec<V3> = levels.iter().map(|l| l[c]).collect();
            let out = line[0] - (levels[0][0] + levels[0][2]) * 0.5;
            self.polyline(&line, s.chord, MemRole::Chord, out);
        }
        for (k, w) in levels.windows(2).enumerate() {
            for f in 0..4 {
                let g = (f + 1) % 4;
                self.panel(w[0][f], w[1][f], w[0][g], w[1][g], pat, (k + f) % 2 == 1, s, true);
            }
            if plan_every > 0 && k % plan_every == 0 {
                let l = &w[1];
                self.add(l[0], l[2], plan_sec, MemRole::PlanBrace, v3(0.0, 0.0, 1.0));
            }
        }
        for f in 0..4 {
            let l = &levels[0];
            self.add(l[f], l[(f + 1) % 4], s.strut, MemRole::Strut, v3(0.0, 0.0, 1.0));
        }
    }

    pub fn length(&self, m: &Mem) -> f64 {
        (self.nodes[m.b as usize] - self.nodes[m.a as usize]).len()
    }

    /// Connected components of the member graph (union-find).
    pub fn components(&self) -> usize {
        self.parts().len()
    }

    /// The members of each connected part, the largest part first.
    pub fn parts(&self) -> Vec<Vec<usize>> {
        let mut p: Vec<u32> = (0..self.nodes.len() as u32).collect();
        fn find(p: &mut [u32], mut x: u32) -> u32 {
            while p[x as usize] != x {
                p[x as usize] = p[p[x as usize] as usize];
                x = p[x as usize];
            }
            x
        }
        for m in &self.members {
            let (a, b) = (find(&mut p, m.a), find(&mut p, m.b));
            if a != b {
                p[a as usize] = b;
            }
        }
        let mut by_root: HashMap<u32, Vec<usize>> = HashMap::new();
        for (i, m) in self.members.iter().enumerate() {
            by_root.entry(find(&mut p, m.a)).or_default().push(i);
        }
        let mut out: Vec<Vec<usize>> = by_root.into_values().collect();
        out.sort_by(|a, b| b.len().cmp(&a.len()).then(a[0].cmp(&b[0])));
        out
    }

    /// Where a part lies and what it is made of, for a refusal that names the loose part.
    fn part_text(&self, ms: &[usize]) -> String {
        let (mut lo, mut hi) = (V3 { x: f64::MAX, y: f64::MAX, z: f64::MAX }, V3 { x: f64::MIN, y: f64::MIN, z: f64::MIN });
        let mut roles: Vec<&str> = vec![];
        for &i in ms {
            let m = &self.members[i];
            for n in [m.a, m.b] {
                let q = self.nodes[n as usize];
                (lo.x, lo.y, lo.z) = (lo.x.min(q.x), lo.y.min(q.y), lo.z.min(q.z));
                (hi.x, hi.y, hi.z) = (hi.x.max(q.x), hi.y.max(q.y), hi.z.max(q.z));
            }
            if !roles.contains(&m.role.key()) {
                roles.push(m.role.key());
            }
        }
        format!(
            "{} members ({}) in x {:.1}..{:.1}, y {:.1}..{:.1}, z {:.1}..{:.1} m",
            ms.len(),
            roles.join(", "),
            lo.x,
            hi.x,
            lo.y,
            hi.y,
            lo.z,
            hi.z
        )
    }

    /// Structural validity: no zero-length members, every section known, one component.
    pub fn validate(&self) -> Vec<String> {
        let mut out = vec![];
        for (i, m) in self.members.iter().enumerate() {
            if self.length(m) < 1e-3 {
                out.push(format!("member {i}: zero length"));
            }
        }
        for s in &self.sections {
            if sections::find(s).is_none() {
                out.push(format!("unknown section '{s}'"));
            }
        }
        let parts = self.parts();
        if parts.len() > 1 {
            let loose: Vec<String> = parts[1..].iter().take(3).map(|ms| self.part_text(ms)).collect();
            out.push(format!("{} disconnected parts; apart from the main one: {}", parts.len(), loose.join("; ")));
        }
        out
    }

    pub fn bytes(&self) -> usize {
        self.nodes.capacity() * std::mem::size_of::<V3>() + self.members.capacity() * std::mem::size_of::<Mem>()
    }

    /// Steel mass, kg (7850 kg/m3; wrought iron is within 2 %).
    pub fn mass(&self) -> f64 {
        let areas: Vec<f64> = self.sections.iter().map(|s| sections::find(s).map(|x| sections::area(&x)).unwrap_or(0.0)).collect();
        self.members.iter().map(|m| areas[m.sec as usize] * self.length(m)).sum::<f64>() * 7850.0
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum JointKind {
    /// Column foot: base plate with anchor bolts.
    Base,
    /// Lattice node: gusset plate, one fastener group per member end.
    Gusset,
    /// Two collinear chord pieces: cover plates.
    Splice,
    /// Two diagonals crossing: one small group through both.
    Crossing,
}

/// A derived joint: kind, node, gusset size (along, across, thickness) and one fastener group
/// per connected member end.
#[derive(Clone, Debug, PartialEq)]
pub struct Joint {
    pub kind: JointKind,
    pub node: u32,
    pub plate: (f64, f64, f64),
    pub groups: Vec<(u32, RivetGroup)>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct JointReport {
    pub joints: usize,
    pub by_kind: Vec<(JointKind, usize)>,
    pub findings: Vec<(u32, RivetFinding)>,
    /// Distinct gusset plates after rounding to 10 mm: the instancing prototypes.
    pub plate_protos: usize,
}

/// Fastener demand from the section area: allowable member force over one rivet in double
/// shear (period wrought-iron working stresses, 100 MPa members, 80 MPa rivet shear).
pub(crate) fn rivets_for(area: f64, d: f64) -> usize {
    let force = 0.4 * area * 100e6;
    let cap = 2.0 * std::f64::consts::PI * d * d / 4.0 * 80e6;
    ((force / cap).ceil() as usize).clamp(2, 24)
}

pub(crate) fn thick(s: &str) -> f64 {
    match sections::find(s) {
        Some(crate::member::Shape::Angle { t, .. }) | Some(crate::member::Shape::Pipe { t, .. }) | Some(crate::member::Shape::Tube { t, .. }) => t,
        Some(crate::member::Shape::I { tf, .. }) | Some(crate::member::Shape::Channel { tf, .. }) | Some(crate::member::Shape::Tee { tf, .. }) => tf,
        Some(crate::member::Shape::Rect { b, h }) => b.min(h),
        _ => 0.010,
    }
}

/// Derives every joint of the frame: bases at nodes on z = `ground`, crossings where only
/// diagonals meet in pairs, splices where two chords meet in line, gussets elsewhere.
pub fn joints(f: &Frame, ground: f64) -> (Vec<Joint>, JointReport) {
    let mut inc: Vec<Vec<u32>> = vec![vec![]; f.nodes.len()];
    for (i, m) in f.members.iter().enumerate() {
        inc[m.a as usize].push(i as u32);
        inc[m.b as usize].push(i as u32);
    }
    let area: Vec<f64> = f.sections.iter().map(|s| sections::find(s).map(|x| sections::area(&x)).unwrap_or(1e-3)).collect();
    let tk: Vec<f64> = f.sections.iter().map(|s| thick(s)).collect();
    let mut out = vec![];
    let mut rep = JointReport::default();
    let mut protos = std::collections::HashSet::new();
    let mut kinds: HashMap<JointKind, usize> = HashMap::new();
    for (n, ms) in inc.iter().enumerate() {
        if ms.len() < 2 && f.nodes[n].z > ground + 1e-6 {
            continue;
        }
        if ms.is_empty() {
            continue;
        }
        let roles: Vec<MemRole> = ms.iter().map(|&m| f.members[m as usize].role).collect();
        let dir = |m: u32| {
            let mm = &f.members[m as usize];
            let (a, b) = (f.nodes[mm.a as usize], f.nodes[mm.b as usize]);
            if mm.a as usize == n { (b - a).normalized() } else { (a - b).normalized() }
        };
        let kind = if f.nodes[n].z <= ground + 1e-6 {
            JointKind::Base
        } else if ms.len() == 4 && roles.iter().all(|r| *r == MemRole::Diagonal) {
            JointKind::Crossing
        } else if ms.len() == 2 && roles.iter().all(|r| *r == MemRole::Chord) && dir(ms[0]).dot(dir(ms[1])) < -0.995 {
            JointKind::Splice
        } else {
            JointKind::Gusset
        };
        let tmax = ms.iter().map(|&m| tk[f.members[m as usize].sec as usize]).fold(0.0, f64::max);
        let tp = (tmax.max(0.008) * 1000.0 / 2.0).ceil() * 2.0 / 1000.0; // gusset at least as thick as the thickest leg, even mm
        let mut groups = vec![];
        let (mut along, mut across) = (0.0f64, 0.0f64);
        for &m in ms {
            let mm = &f.members[m as usize];
            let t = tk[mm.sec as usize];
            let probe = rivet::catalog().for_ply(t.min(tp));
            let nr = match kind {
                JointKind::Crossing => 1,
                _ => rivets_for(area[mm.sec as usize], probe.d),
            };
            let lines = if nr > 6 { 2 } else { 1 };
            let g = RivetGroup::layout(nr, lines, t.min(tp), t + tp);
            let (fa, fc) = g.footprint();
            along = along.max(fa + 0.05);
            across = across.max(fc);
            for fd in g.check() {
                rep.findings.push((n as u32, fd));
            }
            groups.push((m, g));
        }
        let plate = match kind {
            JointKind::Base => (0.6, 0.6, 0.030),
            JointKind::Crossing => (0.0, 0.0, 0.0),
            _ => (2.0 * along, (2.0 * across).max(along), tp),
        };
        if plate.0 > 0.0 {
            protos.insert(((plate.0 * 100.0).round() as i64, (plate.1 * 100.0).round() as i64, (plate.2 * 1e3).round() as i64));
        }
        *kinds.entry(kind).or_default() += 1;
        out.push(Joint { kind, node: n as u32, plate, groups });
    }
    rep.joints = out.len();
    let mut bk: Vec<(JointKind, usize)> = kinds.into_iter().collect();
    bk.sort_by_key(|x| format!("{:?}", x.0));
    rep.by_kind = bk;
    rep.plate_protos = protos.len();
    (out, rep)
}

/// Stitch-rivet lines along built-up members (angles riveted to plates): two lines per member.
pub const STITCH_LINES: usize = 2;

/// Expands joints and member stitching into instanced fastener sets, one per rivet size.
pub fn fasteners(f: &Frame, joints: &[Joint], stitch: bool) -> Vec<FastenerSet> {
    let cat = rivet::catalog();
    let mut sets: Vec<FastenerSet> = cat.sizes.iter().enumerate().map(|(i, _)| FastenerSet { size: i, origin: V3::default(), items: vec![] }).collect();
    let normal = |m: &Mem, u: V3| {
        let n = v3(m.n[0] as f64, m.n[1] as f64, m.n[2] as f64);
        let n = if n.len() < 1e-9 || n.normalized().cross(u).len() < 0.1 {
            let t = if u.z.abs() < 0.9 { v3(0.0, 0.0, 1.0) } else { v3(1.0, 0.0, 0.0) };
            u.cross(t)
        } else {
            n
        };
        n.normalized()
    };
    let mut put = |d: f64, p: V3, ax: V3, grip: f64| {
        if let Some((i, _)) = cat.size(d) {
            sets[i].items.push(FastInst::new(p, V3::default(), ax, grip));
        }
    };
    for j in joints {
        let o = f.nodes[j.node as usize];
        for (m, g) in &j.groups {
            let mm = &f.members[*m as usize];
            let far = if mm.a == j.node { mm.b } else { mm.a };
            let u = (f.nodes[far as usize] - o).normalized();
            let n = normal(mm, u);
            let v = n.cross(u).normalized();
            let (_, across) = g.footprint();
            let start = if j.kind == JointKind::Crossing { 0.0 } else { 0.08 };
            for (a, c) in g.local_points() {
                let p = o + u * (start + a - if j.kind == JointKind::Crossing { g.end } else { 0.0 }) + v * (c - across / 2.0);
                put(g.d, p, n, g.grip);
            }
        }
    }
    if stitch {
        for mm in &f.members {
            if !mm.role.built_up() {
                continue;
            }
            let (a, b) = (f.nodes[mm.a as usize], f.nodes[mm.b as usize]);
            let len = (b - a).len();
            let t = thick(&f.sections[mm.sec as usize]);
            let s = cat.for_ply(t);
            let pitch = (cat.rules.stitch_t * t).min(cat.rules.p_max_abs).max(cat.rules.p_min * s.d0);
            let u = (b - a) * (1.0 / len);
            let n = normal(mm, u);
            let v = n.cross(u).normalized();
            let clear = 0.3; // joint groups own the member ends
            let k = ((len - 2.0 * clear) / pitch).floor();
            if k < 1.0 {
                continue;
            }
            for line in 0..STITCH_LINES {
                let off = (line as f64 - 0.5) * 2.5 * s.d0;
                for i in 0..=k as usize {
                    put(s.d, a + u * (clear + i as f64 * pitch) + v * off, n, 2.0 * t);
                }
            }
        }
    }
    sets.retain(|s| !s.items.is_empty());
    sets
}
