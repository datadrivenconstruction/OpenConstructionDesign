//! Hierarchical lattices: a tower lattice whose long members are lattices themselves, whose
//! chords may again be laced pairs (up to three lattice levels). Identical members are built
//! once as prototypes in their own axes and placed by rigid transforms, so a structure of
//! millions of pieces holds only its distinct members. Every choice (lattice or rolled, bracing
//! pattern, widths, setbacks, splices) comes from [`HierRules`] and is logged as a [`Decision`].

use super::lattice::{rivets_for, thick, Frame, MemRole, Pattern};
use super::plate::{self, End, Plate, PlateKind};
use super::sections;
use super::tower::{self, TowerParams};
use crate::math::{v3, V3};
use std::collections::{BTreeMap, HashMap};
use std::sync::OnceLock;

/// The numbers the hierarchy is decided by; the generator fills them from the rule set
/// (`steel.*` in rules.json), the defaults here are the built-in values of those rules.
#[derive(Clone, Debug, PartialEq)]
pub struct HierRules {
    /// Steps towards the heavier height band of every piece section; set by the wind sizing.
    pub upsize: usize,
    /// Lattice levels including the tower lattice itself (1..=3).
    pub levels: usize,
    /// Tower members at least this long are built as lattices.
    pub min_len: f64,
    /// Width of a latticed member over its length, capped by `max_width`.
    pub width_ratio: f64,
    pub max_width: f64,
    /// Narrower lattices are not built: the piece stays a rolled section.
    pub min_width: f64,
    /// Width of a laced chord over the width of the member it belongs to.
    pub child_ratio: f64,
    /// Lacing panel length over lattice width.
    pub panel_aspect: f64,
    /// Lattices at least this wide take X (double) lacing, narrower ones single Warren lacing.
    pub x_min_width: f64,
    /// Panels shorter than this share of their width take K bracing.
    pub k_ratio: f64,
    pub girder_pattern: Pattern,
    pub arch_pattern: Pattern,
    /// Member ends stop this share of the widest member short of a tower node.
    pub setback: f64,
    /// Thinnest gusset plate.
    pub plate_min: f64,
    /// Longest chord piece between splices.
    pub piece_max: f64,
    /// Panel height of the tower lattice when its members are lattices (levels >= 2).
    pub tower_panel: f64,
    /// Platform floor beam spacing (and platform girder panel) when levels >= 2.
    pub floor_grid: f64,
}

impl Default for HierRules {
    fn default() -> Self {
        HierRules {
            upsize: 0,
            levels: 3,
            min_len: 4.0,
            width_ratio: 0.07,
            max_width: 1.2,
            min_width: 0.15,
            child_ratio: 0.3,
            panel_aspect: 1.0,
            x_min_width: 0.8,
            k_ratio: 0.4,
            girder_pattern: Pattern::Pratt,
            arch_pattern: Pattern::Warren,
            setback: 0.6,
            plate_min: 0.008,
            piece_max: 8.0,
            tower_panel: 8.0,
            floor_grid: 4.0,
        }
    }
}

impl HierRules {
    pub fn check(&self) -> Result<(), String> {
        let ok = (1..=3).contains(&self.levels)
            && self.min_len > 0.0
            && self.width_ratio > 0.0
            && self.max_width >= self.min_width
            && self.min_width > 0.0
            && self.child_ratio > 0.0
            && self.child_ratio < 1.0
            && self.panel_aspect > 0.1
            && self.setback >= 0.0
            && self.plate_min > 0.0
            && self.piece_max > 0.5
            && self.tower_panel >= 1.0
            && self.floor_grid >= 0.5;
        if ok {
            Ok(())
        } else {
            Err("lattice rules out of range: levels 1..3, widths and ratios positive, child ratio below 1".into())
        }
    }
}

/// Rigid placement: origin and orthonormal axes (z along the member).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Xf {
    pub o: V3,
    pub x: V3,
    pub y: V3,
    pub z: V3,
}

impl Xf {
    pub const ID: Xf = Xf { o: v3(0.0, 0.0, 0.0), x: v3(1.0, 0.0, 0.0), y: v3(0.0, 1.0, 0.0), z: v3(0.0, 0.0, 1.0) };
    pub fn p(&self, q: V3) -> V3 {
        self.o + self.d(q)
    }
    pub fn d(&self, q: V3) -> V3 {
        self.x * q.x + self.y * q.y + self.z * q.z
    }
    /// The child's placement in this one's parent axes.
    pub fn then(&self, c: &Xf) -> Xf {
        Xf { o: self.p(c.o), x: self.d(c.x), y: self.d(c.y), z: self.d(c.z) }
    }
}

/// One physical piece: working line a→b, section y axis `up`, section, role, lattice level.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Leaf {
    pub a: V3,
    pub b: V3,
    pub up: V3,
    pub sec: u16,
    pub role: MemRole,
    pub level: u8,
    /// Face of the lattice it braces (0..3 box faces, 4 the plane of a laced pair), NO_FACE for chords.
    pub face: u8,
    /// An angle whose heel sits on the working line (chords); otherwise the section is centred.
    pub heel: bool,
}

pub const NO_FACE: u8 = 255;

impl Leaf {
    pub fn len(&self) -> f64 {
        (self.b - self.a).len()
    }
}

/// A latticed member in its own axes: z from 0 to `len`, chords on the corners of a `width`
/// square (box) or at x = ±width/2 (a laced pair).
#[derive(Clone, Debug, PartialEq)]
pub struct Proto {
    pub level: u8,
    pub len: f64,
    pub width: f64,
    pub planar: bool,
    pub pattern: Pattern,
    /// The rule that chose the pattern.
    pub rule: &'static str,
    pub panels: usize,
    pub leaves: Vec<Leaf>,
    pub plates: Vec<Plate>,
    /// Laced chords: prototype and placement.
    pub kids: Vec<(u32, Xf)>,
    /// Chord area of all chords together, m2 (sizes the parent's gussets).
    pub chord_area: f64,
    /// Thickness of the chord plies the parent's gussets rivet into.
    pub chord_t: f64,
    /// Steel of the whole member with its laced chords, kg.
    pub mass: f64,
    /// Distance between the chord (branch) axes across the member; `width` less the branch reach.
    pub axis_w: f64,
}

/// A tower member built as a lattice.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Root {
    pub proto: u32,
    pub xf: Xf,
    /// Its member in the tower lattice.
    pub member: u32,
}

/// One logged choice: the rule, its value, a why-template key with its slots, and how many
/// members it decided.
#[derive(Clone, Debug, PartialEq)]
pub struct Decision {
    pub rule: &'static str,
    pub value: f64,
    pub key: &'static str,
    pub args: Vec<(&'static str, String)>,
    pub count: usize,
}

#[derive(Clone, Debug, PartialEq)]
pub struct Hier {
    /// The tower parameters as built (panels set by the rules).
    pub params: TowerParams,
    /// Level 1: the tower lattice.
    pub skeleton: Frame,
    pub sections: Vec<String>,
    pub protos: Vec<Proto>,
    pub roots: Vec<Root>,
    /// Tower members that stay rolled sections, in world axes.
    pub leaves: Vec<Leaf>,
    /// Joints of the tower lattice, in world axes.
    pub plates: Vec<Plate>,
    pub decisions: Vec<Decision>,
    /// Pieces refused as shorter than 1 mm.
    pub refused_zero: usize,
    /// Rivet layout findings (spacing rules, fasteners outside plates).
    pub findings: usize,
    /// Setback of each tower node (its latticed members stop that far from it).
    pub setback: Vec<f64>,
}

/// Totals over every placement.
#[derive(Clone, Copy, Debug, Default, PartialEq)]
pub struct Totals {
    pub pieces: usize,
    /// Pieces by lattice level 1..3 (index 0 unused).
    pub by_level: [usize; 4],
    pub plates: usize,
    pub plates_by_kind: [usize; 5],
    pub rivets: usize,
    /// Prototype placements (each a latticed member).
    pub placements: usize,
    pub mass: f64,
}

impl Totals {
    fn add(&mut self, o: &Totals, k: usize) {
        self.pieces += o.pieces * k;
        for i in 0..4 {
            self.by_level[i] += o.by_level[i] * k;
        }
        self.plates += o.plates * k;
        for i in 0..5 {
            self.plates_by_kind[i] += o.plates_by_kind[i] * k;
        }
        self.rivets += o.rivets * k;
        self.placements += o.placements * k;
        self.mass += o.mass * k as f64;
    }
    pub fn max_level(&self) -> usize {
        (1..4).rev().find(|&l| self.by_level[l] > 0).unwrap_or(0)
    }
}

fn kind_ix(k: PlateKind) -> usize {
    PlateKind::ALL.iter().position(|x| *x == k).unwrap_or(0)
}

struct Row {
    level: u8,
    role: String,
    z_to: f64,
    section: String,
}

fn member_rows() -> &'static [Row] {
    static T: OnceLock<Vec<Row>> = OnceLock::new();
    T.get_or_init(|| {
        include_str!("../../data/steel/lattice_members.csv")
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#') && !l.starts_with("level,"))
            .map(|l| {
                let c: Vec<&str> = l.split(',').map(str::trim).collect();
                assert!(c.len() == 4 && sections::find(c[3]).is_some(), "lattice_members.csv: bad row {l}");
                Row { level: c[0].parse().expect("level"), role: c[1].into(), z_to: c[2].parse().expect("z_to"), section: c[3].into() }
            })
            .collect()
    })
}

/// Section of a piece by lattice level, role (`chord` | `lacing`) and the height of its tower
/// member; the band index is part of the prototype key.
pub fn piece_section(level: u8, role: &str, z: f64, upsize: usize) -> (&'static str, u8) {
    let rows: Vec<&Row> = member_rows().iter().filter(|r| r.level == level.clamp(2, 3) && r.role == role).collect();
    let i = rows.iter().position(|r| z <= r.z_to).unwrap_or(rows.len() - 1).saturating_sub(upsize);
    (rows[i].section.as_str(), i as u8)
}

/// Option order of the pattern rules (`steel.lat.girder_pattern`, `steel.lat.arch_pattern`).
pub const PATTERN_OPTIONS: [Pattern; 4] = [Pattern::Warren, Pattern::Pratt, Pattern::K, Pattern::X];

pub fn pat_key(p: Pattern) -> &'static str {
    match p {
        Pattern::Warren => "warren",
        Pattern::Pratt => "pratt",
        Pattern::K => "k",
        Pattern::X => "x",
    }
}

/// Bracing of a lattice by rule: girders and arches by their own rules, squat panels K, wide
/// lattices X (double lacing), narrow ones Warren (single lacing).
pub fn pick_pattern(r: &HierRules, role: MemRole, width: f64, panel: f64) -> (Pattern, &'static str) {
    match role {
        MemRole::Girder => (r.girder_pattern, "steel.lat.girder_pattern"),
        MemRole::Arch => (r.arch_pattern, "steel.lat.arch_pattern"),
        _ if panel < r.k_ratio * width => (Pattern::K, "steel.lat.k_ratio"),
        _ if width >= r.x_min_width => (Pattern::X, "steel.lat.x_min_width"),
        _ => (Pattern::Warren, "steel.lat.x_min_width"),
    }
}

type Key = (u8, bool, i64, i64, u8, u8);

struct Sec {
    area: f64,
    t: f64,
    flat: bool,
}

struct B<'a> {
    r: &'a HierRules,
    names: Vec<String>,
    info: Vec<Sec>,
    protos: Vec<Proto>,
    index: HashMap<Key, u32>,
    why: BTreeMap<(&'static str, String), Decision>,
    refused: usize,
    findings: usize,
}

const X: V3 = v3(1.0, 0.0, 0.0);
const Y: V3 = v3(0.0, 1.0, 0.0);
const Z: V3 = v3(0.0, 0.0, 1.0);

fn mm(x: f64) -> i64 {
    (x * 1e3).round() as i64
}

fn fmt(x: f64) -> String {
    let s = format!("{x:.2}");
    s.trim_end_matches('0').trim_end_matches('.').to_string()
}

fn role_ix(r: MemRole) -> u8 {
    r as u8
}

impl<'a> B<'a> {
    fn new(r: &'a HierRules) -> Self {
        B { r, names: vec![], info: vec![], protos: vec![], index: HashMap::new(), why: BTreeMap::new(), refused: 0, findings: 0 }
    }

    fn sec(&mut self, name: &str) -> u16 {
        if let Some(i) = self.names.iter().position(|s| s == name) {
            return i as u16;
        }
        let shape = sections::find(name);
        let area = shape.as_ref().map_or(0.0, sections::area);
        let flat = matches!(shape, Some(crate::member::Shape::Rect { .. }));
        self.names.push(name.to_string());
        self.info.push(Sec { area, t: thick(name), flat });
        (self.names.len() - 1) as u16
    }

    /// Logs one decided member under (key, detail); equal details add up.
    fn note(&mut self, rule: &'static str, value: f64, key: &'static str, detail: String, args: Vec<(&'static str, String)>) {
        self.why
            .entry((key, detail))
            .and_modify(|d| d.count += 1)
            .or_insert(Decision { rule, value, key, args, count: 1 });
    }

    fn leaf(&mut self, out: &mut Vec<Leaf>, l: Leaf) {
        if (l.b - l.a).len() < 1e-3 {
            self.refused += 1;
            return;
        }
        out.push(l);
    }

    /// Farthest distance of any piece of `p` from its axis (in plan), section included.
    fn reach(&self, p: &Proto) -> f64 {
        p.leaves
            .iter()
            .map(|l| {
                let r = sections::find(&self.names[l.sec as usize]).map_or(0.05, |s| s.size().0.max(s.size().1));
                [l.a, l.b].iter().map(|q| q.x.abs().max(q.y.abs())).fold(0.0, f64::max) + if l.heel { r } else { r / 2.0 }
            })
            .fold(0.0, f64::max)
    }

    /// The prototype of a latticed member, built once per distinct key.
    fn proto(&mut self, level: u8, planar: bool, len: f64, width: f64, role: MemRole, z: f64) -> u32 {
        let (_, band) = piece_section(level, "chord", z, self.r.upsize);
        let key: Key = (level, planar, mm(len), mm(width), role_ix(role), band);
        if let Some(&i) = self.index.get(&key) {
            return i;
        }
        let p = self.build(level, planar, mm(len) as f64 / 1e3, mm(width) as f64 / 1e3, role, z);
        let i = self.protos.len() as u32;
        self.protos.push(p);
        self.index.insert(key, i);
        i
    }

    fn build(&mut self, level: u8, planar: bool, len: f64, w: f64, role: MemRole, z: f64) -> Proto {
        let r = self.r;
        let n = ((len / (w * r.panel_aspect)).round() as usize).max(2);
        let pz: Vec<f64> = (0..=n).map(|i| len * i as f64 / n as f64).collect();
        let (pat, rule) = pick_pattern(r, role, w, len / n as f64);
        let corners: Vec<(f64, f64)> = if planar { vec![(1.0, 0.0), (-1.0, 0.0)] } else { vec![(1.0, 1.0), (-1.0, 1.0), (-1.0, -1.0), (1.0, -1.0)] };
        let mut leaves = vec![];
        let mut kids = vec![];
        let wc = w * r.child_ratio;
        let laced = (level as usize) < r.levels && wc >= r.min_width - 1e-9 && len >= 2.0 * wc;
        // Laced chords are branches centred on the corner lines of a smaller square: the branch
        // lies wholly inside the outer width `w`, and the face bracing ends on the branch axes.
        let kid = laced.then(|| self.proto(level + 1, true, len, wc, MemRole::Chord, z));
        let wa = match kid {
            Some(k) => w - 2.0 * self.reach(&self.protos[k as usize].clone()),
            None => w,
        };
        let at = |c: (f64, f64), zz: f64| v3(c.0 * wa / 2.0, c.1 * wa / 2.0, zz);
        let (chord_area, chord_t, chord_half);
        if let Some(kid) = kid {
            for &c in &corners {
                kids.push((kid, Xf { o: at(c, 0.0), x: X, y: Y, z: Z }));
            }
            let k = &self.protos[kid as usize];
            chord_area = corners.len() as f64 * k.chord_area;
            chord_t = k.chord_t;
            chord_half = wc / 2.0;
        } else {
            let (name, _) = piece_section(level, "chord", z, self.r.upsize);
            let s = self.sec(name);
            for &c in &corners {
                // angle heel on the corner line, legs inward along the faces (laced pairs: outward)
                let up = if planar {
                    if c.0 > 0.0 { -Y } else { -X }
                } else if c.0 == c.1 {
                    X * -c.0
                } else {
                    Y * -c.1
                };
                for i in 0..n {
                    self.leaf(&mut leaves, Leaf { a: at(c, pz[i]), b: at(c, pz[i + 1]), up, sec: s, role: MemRole::Chord, level, face: NO_FACE, heel: true });
                }
            }
            chord_area = corners.len() as f64 * self.info[s as usize].area;
            chord_t = self.info[s as usize].t;
            chord_half = sections::find(name).map_or(0.05, |sh| sh.size().0.max(sh.size().1)) / 2.0;
        }
        // lacing on every face
        let (lname, _) = piece_section(level, "lacing", z, self.r.upsize);
        let ls = self.sec(lname);
        let faces: Vec<(usize, usize)> = if planar { vec![(0, 1)] } else { (0..4).map(|f| (f, (f + 1) % 4)).collect() };
        for (fi, &(f, g)) in faces.iter().enumerate() {
            let (cf, cg) = (corners[f], corners[g]);
            let nrm = if planar { Y } else { v3(cf.0 + cg.0, cf.1 + cg.1, 0.0).normalized() };
            let face = if planar { 4 } else { fi as u8 };
            let strut = matches!(pat, Pattern::Pratt | Pattern::K);
            let mut add = |b: &mut Self, a: V3, e: V3, role: MemRole| {
                let up = nrm.cross((e - a).normalized()).normalized();
                b.leaf(&mut leaves, Leaf { a, b: e, up, sec: ls, role, level, face, heel: false });
            };
            add(self, at(cf, 0.0), at(cg, 0.0), MemRole::Strut);
            for i in 0..n {
                let (a0, a1, b0, b1) = (at(cf, pz[i]), at(cf, pz[i + 1]), at(cg, pz[i]), at(cg, pz[i + 1]));
                match pat {
                    Pattern::Warren => {
                        if (i + fi) % 2 == 1 {
                            add(self, a1, b0, MemRole::Diagonal);
                        } else {
                            add(self, a0, b1, MemRole::Diagonal);
                        }
                    }
                    Pattern::Pratt => add(self, a0, b1, MemRole::Diagonal),
                    Pattern::K => {
                        let m = (a1 + b1) * 0.5;
                        add(self, a0, m, MemRole::Diagonal);
                        add(self, b0, m, MemRole::Diagonal);
                    }
                    Pattern::X => {
                        let c = (a0 + b0 + a1 + b1) * 0.25;
                        add(self, a0, c, MemRole::Diagonal);
                        add(self, c, b1, MemRole::Diagonal);
                        add(self, b0, c, MemRole::Diagonal);
                        add(self, c, a1, MemRole::Diagonal);
                    }
                }
                if strut || i + 1 == n {
                    if pat == Pattern::K {
                        let m = (a1 + b1) * 0.5;
                        add(self, a1, m, MemRole::Strut);
                        add(self, m, b1, MemRole::Strut);
                    } else {
                        add(self, a1, b1, MemRole::Strut);
                    }
                }
            }
        }
        let corner_pts: Vec<V3> = corners.iter().map(|&c| at(c, 0.0)).collect();
        let plates = self.joints(&leaves, &corner_pts, len, chord_t, chord_half, chord_area / corners.len() as f64);
        let own: f64 = leaves.iter().map(|l| self.info[l.sec as usize].area * l.len()).sum::<f64>() * 7850.0;
        let kid_mass: f64 = kids.iter().map(|(k, _)| self.protos[*k as usize].mass).sum();
        Proto { level, len, width: w, planar, pattern: pat, rule, panels: n, leaves, plates, kids, chord_area, chord_t, mass: own + kid_mass, axis_w: wa }
    }

    /// Plates of one prototype: a gusset (or a direct lacing patch) per node and face, one rivet
    /// at each lacing crossing, splices on rolled chords longer than a piece.
    #[allow(clippy::too_many_arguments)]
    fn joints(&mut self, leaves: &[Leaf], corners: &[V3], len: f64, chord_t: f64, chord_half: f64, chord_area: f64) -> Vec<Plate> {
        let key = |p: V3| (mm(p.x * 10.0), mm(p.y * 10.0), mm(p.z * 10.0));
        let mut nodes: Vec<(V3, Vec<(usize, V3)>)> = vec![];
        let mut ix: HashMap<(i64, i64, i64), usize> = HashMap::new();
        for (i, l) in leaves.iter().enumerate() {
            for (p, q) in [(l.a, l.b), (l.b, l.a)] {
                let k = key(p);
                let n = *ix.entry(k).or_insert_with(|| {
                    nodes.push((p, vec![]));
                    nodes.len() - 1
                });
                nodes[n].1.push((i, (q - p).normalized()));
            }
        }
        let on_corner = |p: V3| corners.iter().any(|c| (c.x - p.x).abs() < 1e-6 && (c.y - p.y).abs() < 1e-6);
        let mut out = vec![];
        for (p, ends) in &nodes {
            let corner = on_corner(*p);
            let mut faces: Vec<u8> = ends.iter().map(|e| leaves[e.0].face).filter(|f| *f != NO_FACE).collect();
            faces.sort_unstable();
            faces.dedup();
            for f in faces {
                let fe: Vec<&(usize, V3)> = ends.iter().filter(|e| leaves[e.0].face == f).collect();
                let l0 = &leaves[fe[0].0];
                let nrm = l0.up.cross((l0.b - l0.a).normalized()).normalized();
                let e1 = Z;
                let e2 = nrm.cross(Z).normalized();
                let tl = self.info[l0.sec as usize].t;
                if !corner && fe.len() == 4 && fe.iter().all(|e| leaves[e.0].role == MemRole::Diagonal) {
                    out.push(plate::crossing(*p, e1, e2, tl));
                    continue;
                }
                let flats = fe.iter().all(|e| self.info[leaves[e.0].sec as usize].flat);
                let tp = if flats { 0.0 } else { ((tl.max(chord_t).max(self.r.plate_min)) * 500.0).ceil() / 500.0 };
                let grip = if flats { tl + chord_t } else { tp + tl.max(chord_t) };
                let mut list = vec![];
                let mut nmax = 2;
                for e in &fe {
                    let s = &self.info[leaves[e.0].sec as usize];
                    let d = super::rivet::catalog().for_ply(s.t.min(if flats { chord_t } else { tp })).d;
                    let nr = rivets_for(s.area, d);
                    nmax = nmax.max(nr);
                    list.push(End { u: e.1, start: chord_half + 0.5 * d * 3.0, n: nr, t_ply: s.t.min(if flats { chord_t } else { tp }), grip });
                }
                if corner && !flats {
                    // the gusset also rivets into the chord on both sides of the node
                    for (dir, ok) in [(Z, p.z < len - 1e-6), (-Z, p.z > 1e-6)] {
                        if ok {
                            list.push(End { u: dir, start: 0.0, n: nmax, t_ply: chord_t.min(tp), grip });
                        }
                    }
                }
                let kind = if flats { PlateKind::Lacing } else { PlateKind::Gusset };
                let (pl, fnd) = plate::build(kind, *p, e1, e2, tp, &list);
                self.findings += fnd;
                out.push(pl);
            }
        }
        // splices on rolled chords: pieces at most piece_max, joined at the nearest panel node
        let chord_leaves: Vec<&Leaf> = leaves.iter().filter(|l| l.role == MemRole::Chord).collect();
        if !chord_leaves.is_empty() && len > self.r.piece_max {
            let k = (len / self.r.piece_max).ceil() as usize;
            let s = chord_leaves[0].sec;
            let t = self.info[s as usize].t;
            let d = super::rivet::catalog().for_ply(t).d;
            let nr = rivets_for(chord_area, d);
            let mut zs: Vec<f64> = chord_leaves.iter().map(|l| l.a.z).collect();
            zs.sort_by(f64::total_cmp);
            zs.dedup_by(|a, b| (*a - *b).abs() < 1e-6);
            for j in 1..k {
                let want = len * j as f64 / k as f64;
                let zj = zs.iter().copied().min_by(|a, b| (a - want).abs().total_cmp(&(b - want).abs())).unwrap_or(want);
                for c in corners {
                    let leaf = chord_leaves.iter().find(|l| (l.a.x - c.x).abs() < 1e-6 && (l.a.y - c.y).abs() < 1e-6).copied();
                    let Some(l) = leaf else { continue };
                    let o = v3(c.x, c.y, zj);
                    let e2 = l.up.cross(Z).normalized();
                    let ends = [
                        End { u: Z, start: 0.005, n: nr, t_ply: t, grip: 2.0 * t },
                        End { u: -Z, start: 0.005, n: nr, t_ply: t, grip: 2.0 * t },
                    ];
                    let (pl, fnd) = plate::build(PlateKind::Splice, o, Z, e2, t, &ends);
                    self.findings += fnd;
                    out.push(pl);
                }
            }
        }
        out
    }
}

fn canon(n: V3) -> V3 {
    let n = n.normalized();
    let s = if n.x.abs() > 1e-9 { n.x } else if n.y.abs() > 1e-9 { n.y } else { n.z };
    if s < 0.0 {
        -n
    } else {
        n
    }
}

/// The tower: its lattice (level 1) from [`tower::lattice_tower_300`], every long member built as
/// a lattice (level 2) whose chords may be laced pairs (level 3), and the joints at every level.
pub fn tower(tp: &TowerParams, r: &HierRules) -> Result<Hier, String> {
    r.check()?;
    let mut tp = tp.clone();
    if r.levels >= 2 {
        // a lattice of lattices: tower panels at the rule's length, arches split alike
        let k = tp.panel_shaft / tp.panel_leg;
        tp.panel_leg = r.tower_panel;
        tp.panel_shaft = r.tower_panel * k;
        tp.floor_grid = r.floor_grid;
        let half = tp.w(0.0) - tp.leg0;
        tp.arch_panels = ((std::f64::consts::PI * half / r.tower_panel).ceil() as usize).max(4);
    }
    let (pat, rule) = pick_pattern(r, MemRole::Chord, tp.leg0, tp.panel_leg);
    tp.pattern = pat;
    let skeleton = tower::lattice_tower_300(&tp)?;
    let errs = skeleton.validate();
    if !errs.is_empty() {
        return Err(errs.join("; "));
    }
    let mut b = B::new(r);
    b.note(rule, if rule == "steel.lat.k_ratio" { r.k_ratio } else { r.x_min_width }, "steel_skeleton", String::new(), vec![
        ("n", skeleton.members.len().to_string()),
        ("pattern", pat_key(pat).into()),
        ("panel", fmt(tp.panel_leg)),
    ]);
    let f = &skeleton;
    let nn = f.nodes.len();
    let mut inc: Vec<Vec<u32>> = vec![vec![]; nn];
    for (i, m) in f.members.iter().enumerate() {
        inc[m.a as usize].push(i as u32);
        inc[m.b as usize].push(i as u32);
    }
    // latticed or rolled, and the width of each latticed member
    let width: Vec<f64> = f
        .members
        .iter()
        .map(|m| {
            let l = f.length(m);
            if r.levels < 2 || m.role == MemRole::Beam || l < r.min_len {
                return 0.0;
            }
            let w = (l * r.width_ratio).min(r.max_width);
            if w < r.min_width {
                0.0
            } else {
                w
            }
        })
        .collect();
    let setback: Vec<f64> = (0..nn).map(|n| inc[n].iter().map(|&m| width[m as usize]).fold(0.0, f64::max) * r.setback).collect();
    if setback.iter().any(|s| *s > 0.0) {
        b.note("steel.lat.setback", r.setback, "steel_setback", String::new(), vec![("k", fmt(r.setback))]);
    }
    let mut roots = vec![];
    let mut leaves = vec![];
    let mut lat = vec![false; f.members.len()];
    for (i, m) in f.members.iter().enumerate() {
        let (pa, pb) = (f.nodes[m.a as usize], f.nodes[m.b as usize]);
        let l = (pb - pa).len();
        let (sa, sb) = (setback[m.a as usize], setback[m.b as usize]);
        let local = l - sa - sb;
        let zmid = (pa.z + pb.z) / 2.0;
        let z = (pb - pa).normalized();
        let n = v3(m.n[0] as f64, m.n[1] as f64, m.n[2] as f64);
        let mut y = n - z * n.dot(z);
        if y.len() < 1e-3 {
            y = if z.z.abs() < 0.9 { Z - z * z.z } else { X - z * z.x };
        }
        let y = y.normalized();
        let x = y.cross(z).normalized();
        if width[i] > 0.0 && local >= 2.0 * width[i] {
            b.note("steel.lat.min_len", r.min_len, "steel_latticed", String::new(), vec![("len", fmt(r.min_len)), ("ratio", fmt(r.width_ratio)), ("max", fmt(r.max_width))]);
            let p = b.proto(2, false, local, width[i], m.role, zmid);
            roots.push(Root { proto: p, xf: Xf { o: pa + z * sa, x, y, z }, member: i as u32 });
            lat[i] = true;
        } else {
            b.note("steel.lat.min_len", r.min_len, "steel_rolled", String::new(), vec![("len", fmt(r.min_len))]);
            let s = b.sec(&f.sections[m.sec as usize]);
            b.leaf(&mut leaves, Leaf { a: pa, b: pb, up: x, sec: s, role: m.role, level: 1, face: NO_FACE, heel: false });
        }
    }
    // joints of the tower lattice: base plates on the ground, a gusset per plane of members
    let proto_of: HashMap<u32, u32> = roots.iter().map(|r| (r.member, r.proto)).collect();
    let mut plates = vec![];
    for n in 0..nn {
        let ms = &inc[n];
        if ms.is_empty() {
            continue;
        }
        let o = f.nodes[n];
        let dir = |m: u32| {
            let mm = &f.members[m as usize];
            let (a, c) = (f.nodes[mm.a as usize], f.nodes[mm.b as usize]);
            if mm.a as usize == n { (c - a).normalized() } else { (a - c).normalized() }
        };
        if o.z <= 1e-6 {
            let size = ms.iter().map(|&m| width[m as usize]).fold(0.2, f64::max);
            plates.push(plate::base(o, size, 0.03));
            continue;
        }
        if ms.len() < 2 {
            continue;
        }
        // candidate planes: the lattice faces the members were generated in
        let mut planes: Vec<V3> = vec![];
        let mut covered = vec![false; ms.len()];
        let cands: Vec<V3> = ms.iter().map(|&m| f.members[m as usize].n).map(|q| v3(q[0] as f64, q[1] as f64, q[2] as f64)).filter(|q| q.len() > 1e-6).map(canon).collect();
        let try_plane = |nrm: V3, planes: &mut Vec<V3>, covered: &mut Vec<bool>| {
            if planes.iter().any(|q| q.dot(nrm).abs() > 0.999) {
                return;
            }
            let inp: Vec<usize> = (0..ms.len()).filter(|&i| dir(ms[i]).dot(nrm).abs() < 0.05).collect();
            if inp.len() >= 2 {
                for &i in &inp {
                    covered[i] = true;
                }
                planes.push(nrm);
            }
        };
        for c in cands {
            try_plane(c, &mut planes, &mut covered);
        }
        for i in 0..ms.len() {
            if covered[i] {
                continue;
            }
            // the member's own plane with its most nearly perpendicular neighbour
            let u = dir(ms[i]);
            let k = (0..ms.len()).filter(|&j| j != i).min_by(|&a, &c| u.dot(dir(ms[a])).abs().total_cmp(&u.dot(dir(ms[c])).abs()));
            if let Some(k) = k {
                let c = u.cross(dir(ms[k]));
                if c.len() > 1e-3 {
                    try_plane(canon(c), &mut planes, &mut covered);
                }
            }
        }
        for nrm in planes {
            let e1 = {
                let u = dir(ms[(0..ms.len()).find(|&i| dir(ms[i]).dot(nrm).abs() < 0.05).unwrap_or(0)]);
                (u - nrm * u.dot(nrm)).normalized()
            };
            let e2 = nrm.cross(e1).normalized();
            let mut list = vec![];
            let mut tmax: f64 = 0.0;
            let mut parts = vec![];
            for &m in ms.iter().filter(|&&m| dir(m).dot(nrm).abs() < 0.05) {
                let (area, t) = match proto_of.get(&m) {
                    Some(&p) => (b.protos[p as usize].chord_area, b.protos[p as usize].chord_t),
                    None => {
                        let s = &f.sections[f.members[m as usize].sec as usize];
                        (sections::find(s).map_or(1e-3, |x| sections::area(&x)), thick(s))
                    }
                };
                tmax = tmax.max(t);
                parts.push((m, area, t));
            }
            let tp = (tmax.max(r.plate_min) * 500.0).ceil() / 500.0;
            for (m, area, t) in parts {
                let d = super::rivet::catalog().for_ply(t.min(tp)).d;
                let start = if lat[m as usize] { setback[n] } else { 0.0 } + 1.5 * d;
                list.push(End { u: dir(m), start, n: rivets_for(area, d), t_ply: t.min(tp), grip: tp + t });
            }
            let (pl, fnd) = plate::build(PlateKind::Gusset, o, e1, e2, tp, &list);
            b.findings += fnd;
            plates.push(pl);
        }
    }
    // bracing and chord choices, counted over every placed lattice
    let mut seen: Vec<(Pattern, &'static str, u8, bool, bool)> = vec![];
    let mut stack: Vec<u32> = roots.iter().map(|r| r.proto).collect();
    while let Some(p) = stack.pop() {
        let pr = &b.protos[p as usize];
        let spliced = pr.plates.iter().any(|q| q.kind == PlateKind::Splice);
        seen.push((pr.pattern, pr.rule, pr.level, !pr.kids.is_empty(), spliced));
        stack.extend(pr.kids.iter().map(|k| k.0));
    }
    if !plates.is_empty() {
        b.note("steel.conn.plate_min", r.plate_min, "steel_plate", String::new(), vec![("t", fmt(r.plate_min * 1e3))]);
    }
    for (pat, rule, level, laced, spliced) in seen {
        let pv = match rule {
            "steel.lat.k_ratio" => r.k_ratio,
            "steel.lat.x_min_width" => r.x_min_width,
            // a choice rule's value is its option index
            _ => PATTERN_OPTIONS.iter().position(|p| *p == pat).unwrap_or(0) as f64,
        };
        b.note(rule, pv, "steel_pattern", format!("{}|{level}", pat_key(pat)), vec![("pattern", pat_key(pat).into()), ("level", level.to_string()), ("rule", rule.into())]);
        if spliced {
            b.note("steel.conn.piece_max", r.piece_max, "steel_splice", String::new(), vec![("len", fmt(r.piece_max))]);
        }
        if laced {
            b.note("steel.lat.child_ratio", r.child_ratio, "steel_chord_laced", format!("{}", level + 1), vec![("level", (level + 1).to_string()), ("ratio", fmt(r.child_ratio))]);
        } else if (level as usize) < r.levels {
            b.note("steel.lat.min_width", r.min_width, "steel_chord_rolled", format!("{level}"), vec![("level", level.to_string()), ("w", fmt(r.min_width))]);
        }
    }
    let mut decisions: Vec<Decision> = b.why.into_values().collect();
    decisions.sort_by(|a, c| a.key.cmp(c.key).then(c.count.cmp(&a.count)));
    Ok(Hier {
        params: tp,
        skeleton,
        sections: b.names,
        protos: b.protos,
        roots,
        leaves,
        plates,
        decisions,
        refused_zero: b.refused,
        findings: b.findings,
        setback,
    })
}

impl Hier {
    /// Totals of one prototype with everything inside it, per prototype.
    pub fn proto_totals(&self) -> Vec<Totals> {
        let mut memo: Vec<Option<Totals>> = vec![None; self.protos.len()];
        fn go(h: &Hier, i: usize, memo: &mut Vec<Option<Totals>>) -> Totals {
            if let Some(t) = memo[i] {
                return t;
            }
            let p = &h.protos[i];
            let mut t = Totals { pieces: p.leaves.len(), plates: p.plates.len(), placements: 1, mass: p.mass, ..Totals::default() };
            for l in &p.leaves {
                t.by_level[l.level as usize] += 1;
            }
            for pl in &p.plates {
                t.plates_by_kind[kind_ix(pl.kind)] += 1;
                t.rivets += pl.rivets.len();
            }
            for (k, _) in &p.kids {
                let c = go(h, *k as usize, memo);
                let m = t.mass;
                t.add(&c, 1);
                t.mass = m; // the prototype mass already holds its kids
            }
            memo[i] = Some(t);
            t
        }
        (0..self.protos.len()).map(|i| go(self, i, &mut memo)).collect()
    }

    pub fn totals(&self) -> Totals {
        let per = self.proto_totals();
        let mut t = Totals { pieces: self.leaves.len(), plates: self.plates.len(), ..Totals::default() };
        for l in &self.leaves {
            t.by_level[l.level as usize] += 1;
            if let Some(s) = sections::find(&self.sections[l.sec as usize]) {
                t.mass += sections::area(&s) * l.len() * 7850.0;
            }
        }
        for pl in &self.plates {
            t.plates_by_kind[kind_ix(pl.kind)] += 1;
            t.rivets += pl.rivets.len();
        }
        for r in &self.roots {
            t.add(&per[r.proto as usize], 1);
        }
        t
    }

    /// Every placement of every prototype in world axes (the instancing table), depth first.
    pub fn placements(&self) -> Vec<(u32, Xf)> {
        let mut out = Vec::new();
        let mut stack: Vec<(u32, Xf)> = self.roots.iter().rev().map(|r| (r.proto, r.xf)).collect();
        while let Some((p, xf)) = stack.pop() {
            out.push((p, xf));
            for (k, c) in self.protos[p as usize].kids.iter().rev() {
                stack.push((*k, xf.then(c)));
            }
        }
        out
    }

    /// Structural validity: no piece under 1 mm anywhere, every fastener inside its plate,
    /// every prototype one connected piece through its plates and laced chords.
    pub fn validate(&self) -> Vec<String> {
        let mut out = vec![];
        if self.refused_zero > 0 {
            out.push(format!("{} pieces under 1 mm refused", self.refused_zero));
        }
        let short = |ls: &[Leaf]| ls.iter().filter(|l| l.len() < 1e-3).count();
        let mut zero = short(&self.leaves);
        for (i, p) in self.protos.iter().enumerate() {
            zero += short(&p.leaves);
            for pl in &p.plates {
                for e in plate::check(pl) {
                    out.push(format!("prototype {i}: {e}"));
                }
            }
            let c = self.proto_components(i);
            if c != 1 {
                out.push(format!("prototype {i}: {c} disconnected parts"));
            }
        }
        if zero > 0 {
            out.push(format!("{zero} pieces of zero length"));
        }
        for pl in &self.plates {
            for e in plate::check(pl) {
                out.push(format!("tower joint: {e}"));
            }
        }
        if self.skeleton.components() != 1 {
            out.push("tower lattice is not one piece".into());
        }
        out
    }

    /// Connected parts of one prototype: pieces sharing a node, pieces on one plate, and the
    /// laced chords with the pieces whose nodes lie on their axis.
    pub fn proto_components(&self, i: usize) -> usize {
        let p = &self.protos[i];
        let n = p.leaves.len() + p.kids.len();
        let mut uf: Vec<usize> = (0..n).collect();
        fn find(u: &mut [usize], mut x: usize) -> usize {
            while u[x] != x {
                u[x] = u[u[x]];
                x = u[x];
            }
            x
        }
        let join = |u: &mut Vec<usize>, a: usize, b: usize| {
            let (x, y) = (find(u, a), find(u, b));
            if x != y {
                u[x] = y;
            }
        };
        let key = |q: V3| (mm(q.x * 10.0), mm(q.y * 10.0), mm(q.z * 10.0));
        let mut at: HashMap<(i64, i64, i64), usize> = HashMap::new();
        for (k, l) in p.leaves.iter().enumerate() {
            for q in [l.a, l.b] {
                match at.get(&key(q)) {
                    Some(&o) => join(&mut uf, o, k),
                    None => {
                        at.insert(key(q), k);
                    }
                }
                for (j, (_, xf)) in p.kids.iter().enumerate() {
                    if (q.x - xf.o.x).abs() < 1e-6 && (q.y - xf.o.y).abs() < 1e-6 {
                        join(&mut uf, k, p.leaves.len() + j);
                    }
                }
            }
        }
        let mut roots: Vec<usize> = (0..n).map(|x| find(&mut uf, x)).collect();
        roots.sort_unstable();
        roots.dedup();
        roots.len()
    }

    /// Fingerprint of the whole output in build order (0.1 mm grid), for determinism checks.
    pub fn digest(&self) -> u64 {
        let mut h: u64 = 0xcbf2_9ce4_8422_2325;
        let mut eat = |x: i64| {
            for b in x.to_le_bytes() {
                h = (h ^ b as u64).wrapping_mul(0x0100_0000_01b3);
            }
        };
        let q = |x: f64| (x * 1e4).round() as i64;
        let leaf = |eat: &mut dyn FnMut(i64), l: &Leaf| {
            for v in [l.a, l.b, l.up] {
                eat(q(v.x));
                eat(q(v.y));
                eat(q(v.z));
            }
            eat(l.sec as i64);
            eat(l.level as i64);
        };
        let plate_eat = |eat: &mut dyn FnMut(i64), p: &Plate| {
            eat(kind_ix(p.kind) as i64);
            for v in [p.o, p.e1, p.e2] {
                eat(q(v.x));
                eat(q(v.y));
                eat(q(v.z));
            }
            for r in &p.rivets {
                eat(q(r.0));
                eat(q(r.1));
            }
        };
        for p in &self.protos {
            eat(q(p.len));
            eat(q(p.width));
            for l in &p.leaves {
                leaf(&mut eat, l);
            }
            for pl in &p.plates {
                plate_eat(&mut eat, pl);
            }
            for (k, xf) in &p.kids {
                eat(*k as i64);
                eat(q(xf.o.x));
                eat(q(xf.o.y));
            }
        }
        for r in &self.roots {
            eat(r.proto as i64);
            for v in [r.xf.o, r.xf.x, r.xf.y, r.xf.z] {
                eat(q(v.x));
                eat(q(v.y));
                eat(q(v.z));
            }
        }
        for l in &self.leaves {
            leaf(&mut eat, l);
        }
        for pl in &self.plates {
            plate_eat(&mut eat, pl);
        }
        h
    }
}
