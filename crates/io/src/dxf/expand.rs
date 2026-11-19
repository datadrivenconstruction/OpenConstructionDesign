// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! INSERT expansion: nested blocks, scale/rotation/mirror, MINSERT arrays, ATTRIB/ATTDEF text,
//! extrusion (OCS) and BYBLOCK / layer 0 inheritance. The result is a flat list in world coordinates.

use std::collections::HashMap;
use std::f64::consts::PI;

use super::entities::*;
use super::lower::{self, Fill, LowerReport, Lowered};
use super::text::decode_unicode;
use super::{Block, DxfDocument, Pair};

#[derive(Debug, Clone)]
pub struct ExpandOptions {
    /// Maximum block nesting (an INSERT at top level has depth 1).
    pub max_depth: usize,
    /// Budget for expanded entities plus block instances; expansion stops once reached.
    pub max_entities: usize,
    /// Keep paper-space entities (code 67 = 1) too; by default only model space is drawn.
    pub paper: bool,
}

impl Default for ExpandOptions {
    fn default() -> Self {
        ExpandOptions {
            max_depth: 32,
            max_entities: 1_000_000,
            paper: false,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq)]
pub struct ExpandReport {
    pub inserts_expanded: usize,
    pub missing_blocks: usize,
    pub cycles: usize,
    pub depth_exceeded: usize,
    /// Inserts with a zero scale or a non-horizontal extrusion.
    pub skipped_degenerate: usize,
    /// Budget exhausted: the output is incomplete.
    pub truncated: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BlockKind {
    Door,
    Window,
}

/// Placement of an INSERT whose block name looks like a door or a window (world coordinates).
#[derive(Debug, Clone, PartialEq)]
pub struct InsertHint {
    pub name: String,
    pub layer: String,
    pub kind: BlockKind,
    pub pos: (f64, f64),
    /// Degrees, direction of the block X axis.
    pub rotation: f64,
    /// Lengths of the images of the block X and Y axes.
    pub scale: (f64, f64),
}

/// Any named block placement (world coordinates), for block → family rules.
#[derive(Debug, Clone, PartialEq)]
pub struct InsertInfo {
    pub name: String,
    pub layer: String,
    pub pos: (f64, f64),
    /// Degrees, direction of the block X axis.
    pub rotation: f64,
    pub scale: (f64, f64),
    /// Block extents along its own X and Y axes, scaled (0 when the block has no geometry).
    pub size: (f64, f64),
    /// Centre of the block extents (world).
    pub center: (f64, f64),
    /// Attribute tag → value.
    pub attribs: Vec<(String, String)>,
}

#[derive(Debug, Clone, Default)]
pub struct Expanded {
    pub entities: Vec<Entity>,
    pub report: ExpandReport,
    pub hints: Vec<InsertHint>,
    /// HATCH / SOLID fills, loops and pattern segments in world coordinates.
    pub fills: Vec<Fill>,
    pub inserts: Vec<InsertInfo>,
    pub lower: LowerReport,
}

pub fn block_kind(name: &str) -> Option<BlockKind> {
    let n = name.to_lowercase();
    const DOOR: [&str; 5] = ["door", "\u{434}\u{432}\u{435}\u{440}", "dver", "porte", "tür"];
    const WIN: [&str; 5] = ["window", "\u{43e}\u{43a}\u{43d}", "okno", "fenster", "fenêtre"];
    if DOOR.iter().any(|k| n.contains(k)) {
        Some(BlockKind::Door)
    } else if WIN.iter().any(|k| n.contains(k)) {
        Some(BlockKind::Window)
    } else {
        None
    }
}

/// x' = a x + c y + e, y' = b x + d y + f.
#[derive(Clone, Copy, Debug)]
struct Aff {
    a: f64,
    b: f64,
    c: f64,
    d: f64,
    e: f64,
    f: f64,
}

impl Aff {
    const ID: Aff = Aff {
        a: 1.0,
        b: 0.0,
        c: 0.0,
        d: 1.0,
        e: 0.0,
        f: 0.0,
    };
    const FLIP: Aff = Aff { a: -1.0, ..Aff::ID };
    /// self applied after `o`.
    fn mul(&self, o: &Aff) -> Aff {
        Aff {
            a: self.a * o.a + self.c * o.b,
            b: self.b * o.a + self.d * o.b,
            c: self.a * o.c + self.c * o.d,
            d: self.b * o.c + self.d * o.d,
            e: self.a * o.e + self.c * o.f + self.e,
            f: self.b * o.e + self.d * o.f + self.f,
        }
    }
    fn p(&self, x: f64, y: f64) -> (f64, f64) {
        (
            self.a * x + self.c * y + self.e,
            self.b * x + self.d * y + self.f,
        )
    }
    fn det(&self) -> f64 {
        self.a * self.d - self.b * self.c
    }
    fn kx(&self) -> f64 {
        self.a.hypot(self.b)
    }
    fn ky(&self) -> f64 {
        self.c.hypot(self.d)
    }
    /// Similarity (equal axis scales, orthogonal axes): circles stay circles.
    fn uniform(&self) -> bool {
        let (kx, ky) = (self.kx(), self.ky());
        let m = kx.max(ky).max(1e-300);
        (kx - ky).abs() <= 1e-9 * m && (self.a * self.c + self.b * self.d).abs() <= 1e-9 * m * m
    }
    /// Angle (degrees) of the image of a direction given in degrees.
    fn angle(&self, deg: f64) -> f64 {
        let (s, c) = deg.to_radians().sin_cos();
        let (x, y) = (self.a * c + self.c * s, self.b * c + self.d * s);
        y.atan2(x).to_degrees()
    }
}

struct Ctx {
    layer: String,
    linetype: Option<String>,
    color: Option<i32>,
    true_color: Option<i32>,
    lineweight: Option<i32>,
}

impl Ctx {
    fn of(c: &Common) -> Ctx {
        Ctx {
            layer: c.layer.clone(),
            linetype: c.linetype.clone(),
            color: c.color,
            true_color: c.true_color,
            lineweight: c.lineweight,
        }
    }
}

/// Entity attributes inside a block: layer 0 and BYBLOCK values come from the INSERT.
/// Extrusion groups are dropped (the matrix carries them).
fn resolve(c: &Common, ctx: &Ctx) -> Common {
    let mut r = c.clone();
    if c.layer == "0" {
        r.layer = ctx.layer.clone();
    }
    if c.linetype
        .as_deref()
        .is_some_and(|l| l.eq_ignore_ascii_case("BYBLOCK"))
    {
        r.linetype = ctx.linetype.clone();
    }
    if c.color == Some(0) {
        r.color = ctx.color;
        if r.true_color.is_none() {
            r.true_color = ctx.true_color;
        }
    }
    if c.lineweight == Some(-2) {
        r.lineweight = ctx.lineweight;
    }
    r.extra.retain(|p| !(210..=230).contains(&p.code));
    r
}

fn extra_f(extra: &[Pair], code: i32, dflt: f64) -> f64 {
    extra
        .iter()
        .find(|p| p.code == code)
        .and_then(|p| p.value.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
        .unwrap_or(dflt)
}

/// `Some(true)`: extrusion (0,0,-1) (x mirrored); `Some(false)`: normal; `None`: not horizontal.
fn flip_of(c: &Common) -> Option<bool> {
    let n = [
        extra_f(&c.extra, 210, 0.0),
        extra_f(&c.extra, 220, 0.0),
        extra_f(&c.extra, 230, 1.0),
    ];
    let len = (n[0] * n[0] + n[1] * n[1] + n[2] * n[2]).sqrt();
    if len <= 1e-9 {
        return Some(false);
    }
    if n[0].hypot(n[1]) / len > 1e-6 {
        return None;
    }
    Some(n[2] < 0.0)
}

/// Matrix for entity coordinates: its OCS mirror, then the block placement.
fn with_ocs(m: &Aff, c: &Common) -> Option<Aff> {
    Some(if flip_of(c)? { m.mul(&Aff::FLIP) } else { *m })
}

struct St<'a> {
    opt: &'a ExpandOptions,
    blocks: HashMap<String, &'a Block>,
    /// Block bodies with raw records lowered, once per block.
    bodies: HashMap<String, std::rc::Rc<Vec<Lowered>>>,
    /// Local extents (min, max) per block, relative to its base point.
    extents: HashMap<String, Option<((f64, f64), (f64, f64))>>,
    /// Blocks whose extents are being computed (cycle guard for nested inserts).
    ext_stack: Vec<String>,
    /// Anonymous dynamic-block copy (upper-case `*U…`) → source block name.
    alias: HashMap<String, String>,
    /// Pattern segments of block fills in the entity plane, keyed by the fill's address in
    /// `bodies` (stable: bodies are never dropped during expansion).
    patterns: HashMap<usize, std::rc::Rc<Vec<((f64, f64), (f64, f64))>>>,
    out: Expanded,
    count: usize,
    stack: Vec<String>,
    /// IMAGEDEF handle to file path, for image stand-ins.
    images: HashMap<String, String>,
}

impl St<'_> {
    fn charge(&mut self) -> bool {
        if self.count >= self.opt.max_entities {
            self.out.report.truncated = true;
            return false;
        }
        self.count += 1;
        true
    }
    /// Charges `n` units at once (pattern segments of one fill).
    fn charge_n(&mut self, n: usize) -> bool {
        if self.count.saturating_add(n) > self.opt.max_entities {
            self.out.report.truncated = true;
            return false;
        }
        self.count += n;
        true
    }
    fn push(&mut self, e: Entity) {
        if self.charge() {
            self.out.entities.push(e);
        }
    }
}

pub fn expand_inserts(doc: &DxfDocument, opt: &ExpandOptions) -> Expanded {
    let mut st = St {
        opt,
        blocks: doc
            .blocks
            .iter()
            .map(|b| (b.name.to_uppercase(), b))
            .collect(),
        bodies: HashMap::new(),
        extents: HashMap::new(),
        ext_stack: vec![],
        alias: anon_sources(doc),
        patterns: HashMap::new(),
        out: Expanded::default(),
        count: 0,
        stack: vec![],
        images: super::annot::image_paths(doc),
    };
    let mut rep = LowerReport::default();
    let top = lower::lower_list(&doc.entities, opt.paper, &mut rep);
    for e in &top {
        match e {
            Lowered::Ent(Entity::Insert(i)) => insert(&mut st, i, &Aff::ID, None),
            Lowered::Ent(Entity::Unknown(p)) => {
                st.push(Entity::Unknown(p.clone()));
                for x in super::annot::lower(p, &st.images) {
                    match x {
                        Entity::Insert(i) => insert(&mut st, &i, &Aff::ID, None),
                        other => st.push(other),
                    }
                }
            }
            Lowered::Ent(other) => st.push(other.clone()),
            Lowered::Fill(f) => fill(&mut st, f, &Aff::ID, None),
        }
        if st.out.report.truncated {
            break;
        }
    }
    let inner = std::mem::take(&mut st.out.lower);
    let mut types = rep.unsupported_types;
    for (k, v) in inner.unsupported_types {
        *types.entry(k).or_default() += v;
    }
    st.out.lower = LowerReport {
        lowered: rep.lowered + inner.lowered,
        paper: rep.paper,
        unsupported: rep.unsupported + inner.unsupported,
        unsupported_types: types,
    };
    st.out
}

impl St<'_> {
    fn body(&mut self, key: &str, blk: &Block) -> std::rc::Rc<Vec<Lowered>> {
        if let Some(b) = self.bodies.get(key) {
            return b.clone();
        }
        let mut rep = LowerReport::default();
        let b = std::rc::Rc::new(lower::lower_list(&blk.entities, true, &mut rep));
        self.out.lower.lowered += rep.lowered;
        self.out.lower.unsupported += rep.unsupported;
        for (k, v) in rep.unsupported_types {
            *self.out.lower.unsupported_types.entry(k).or_default() += v;
        }
        self.bodies.insert(key.to_string(), b.clone());
        b
    }

    /// Extents of the block geometry including nested inserts (one instance of a MINSERT),
    /// relative to its base.
    fn extent(&mut self, key: &str, blk: &Block) -> Option<((f64, f64), (f64, f64))> {
        if let Some(e) = self.extents.get(key) {
            return *e;
        }
        let body = self.body(key, blk);
        self.ext_stack.push(key.to_string());
        let (mut lo, mut hi) = ((f64::MAX, f64::MAX), (f64::MIN, f64::MIN));
        let mut add = |x: f64, y: f64| {
            lo = (lo.0.min(x), lo.1.min(y));
            hi = (hi.0.max(x), hi.1.max(y));
        };
        for e in body.iter() {
            match e {
                Lowered::Ent(Entity::Line(l)) => {
                    add(l.start[0], l.start[1]);
                    add(l.end[0], l.end[1]);
                }
                Lowered::Ent(Entity::LwPolyline(p)) => {
                    p.vertices.iter().for_each(|v| add(v.x, v.y))
                }
                Lowered::Ent(Entity::Circle(c)) => {
                    add(c.center[0] - c.radius, c.center[1] - c.radius);
                    add(c.center[0] + c.radius, c.center[1] + c.radius);
                }
                Lowered::Ent(Entity::Arc(a)) => {
                    let (s, mut e) = (a.start_angle.to_radians(), a.end_angle.to_radians());
                    while e <= s {
                        e += 2.0 * PI;
                    }
                    for q in arc_points((a.center[0], a.center[1]), a.radius, s, e) {
                        add(q.0, q.1);
                    }
                }
                Lowered::Fill(f) => f.loops.iter().flatten().for_each(|q| add(q.0, q.1)),
                Lowered::Ent(Entity::Insert(i)) => {
                    let ck = i.block.to_uppercase();
                    if self.ext_stack.contains(&ck) || self.ext_stack.len() >= self.opt.max_depth {
                        continue;
                    }
                    let Some(&cb) = self.blocks.get(&ck) else {
                        continue;
                    };
                    let (sx, sy) = (i.scale[0], i.scale[1]);
                    let Some(flip) = flip_of(&i.common) else {
                        continue;
                    };
                    if sx.abs() < 1e-12 || sy.abs() < 1e-12 {
                        continue;
                    }
                    let Some((clo, chi)) = self.extent(&ck, cb) else {
                        continue;
                    };
                    let (sr, cr) = i.rotation.to_radians().sin_cos();
                    let mir = if flip { -1.0 } else { 1.0 };
                    for (dx, dy) in [
                        (clo.0, clo.1),
                        (chi.0, clo.1),
                        (chi.0, chi.1),
                        (clo.0, chi.1),
                    ] {
                        let (x, y) = (dx * sx, dy * sy);
                        add(
                            mir * (i.pos[0] + cr * x - sr * y),
                            i.pos[1] + sr * x + cr * y,
                        );
                    }
                }
                _ => {}
            }
        }
        self.ext_stack.pop();
        let r = (lo.0 <= hi.0 && lo.0.is_finite() && hi.1.is_finite()).then(|| {
            (
                (lo.0 - blk.base[0], lo.1 - blk.base[1]),
                (hi.0 - blk.base[0], hi.1 - blk.base[1]),
            )
        });
        self.extents.insert(key.to_string(), r);
        r
    }
}

/// Fill in world coordinates: loops mapped, pattern clipped in the entity plane, then mapped.
fn fill(st: &mut St, f: &Fill, m: &Aff, ctx: Option<&Ctx>) {
    let Some(mm) = with_ocs(m, &f.common) else {
        return;
    };
    if !st.charge() {
        return;
    }
    let mut o = f.clone();
    o.common = match ctx {
        Some(c) => resolve(&f.common, c),
        None => {
            let mut c = f.common.clone();
            c.extra.retain(|p| !(210..=230).contains(&p.code));
            c
        }
    };
    if !f.solid && !f.lines.is_empty() {
        let key = f as *const Fill as usize;
        let segs = match st.patterns.get(&key) {
            Some(s) => s.clone(),
            None => {
                let s = std::rc::Rc::new(
                    lower::pattern_segments(&f.loops, &f.lines, 20_000).unwrap_or_default(),
                );
                // top-level fills are seen once; only block bodies repeat
                if ctx.is_some() {
                    st.patterns.insert(key, s.clone());
                }
                s
            }
        };
        if !st.charge_n(segs.len()) {
            return;
        }
        o.segs = segs
            .iter()
            .map(|(a, b)| (mm.p(a.0, a.1), mm.p(b.0, b.1)))
            .collect();
    }
    o.lines.clear();
    for l in &mut o.loops {
        for q in l.iter_mut() {
            *q = mm.p(q.0, q.1);
        }
    }
    st.out.fills.push(o);
}

fn insert(st: &mut St, ins: &Insert, parent: &Aff, ctx: Option<&Ctx>) {
    if st.out.report.truncated {
        return;
    }
    let common = match ctx {
        Some(c) => resolve(&ins.common, c),
        None => ins.common.clone(),
    };
    let key = ins.block.to_uppercase();
    let Some(&blk) = st.blocks.get(&key) else {
        st.out.report.missing_blocks += 1;
        return;
    };
    if st.stack.contains(&key) {
        st.out.report.cycles += 1;
        return;
    }
    if st.stack.len() >= st.opt.max_depth {
        st.out.report.depth_exceeded += 1;
        return;
    }
    let (sx, sy) = (ins.scale[0], ins.scale[1]);
    let flip = flip_of(&ins.common);
    if flip.is_none() || sx.abs() < 1e-12 || sy.abs() < 1e-12 {
        st.out.report.skipped_degenerate += 1;
        return;
    }
    let cols = extra_f(&ins.common.extra, 70, 1.0).clamp(1.0, 1e9) as i64;
    let rows = extra_f(&ins.common.extra, 71, 1.0).clamp(1.0, 1e9) as i64;
    let (dc, dr) = (
        extra_f(&ins.common.extra, 44, 0.0),
        extra_f(&ins.common.extra, 45, 0.0),
    );
    let (sr, cr) = ins.rotation.to_radians().sin_cos();
    let inner = Ctx::of(&common);
    let flip_m = if flip == Some(true) {
        Aff::FLIP
    } else {
        Aff::ID
    };
    // dynamic-block copies are reported under their source name (block → family rules)
    let shown = st
        .alias
        .get(&key)
        .cloned()
        .unwrap_or_else(|| ins.block.clone());
    let kind = block_kind(&shown);
    let listed = !skip_anonymous(&ins.block);

    st.stack.push(key);
    'arr: for r in 0..rows {
        for c in 0..cols {
            if !st.charge() {
                break 'arr;
            }
            let (ox, oy) = (c as f64 * dc, r as f64 * dr);
            // flip * translate(pos + R*offset) * R * S * translate(-base)
            let (px, py) = (
                ins.pos[0] + cr * ox - sr * oy,
                ins.pos[1] + sr * ox + cr * oy,
            );
            let local = flip_m
                .mul(&Aff {
                    a: cr * sx,
                    b: sr * sx,
                    c: -sr * sy,
                    d: cr * sy,
                    e: px,
                    f: py,
                })
                .mul(&Aff {
                    e: -blk.base[0],
                    f: -blk.base[1],
                    ..Aff::ID
                });
            let m = parent.mul(&local);
            st.out.report.inserts_expanded += 1;
            if listed {
                let (bx, by) = (blk.base[0], blk.base[1]);
                let (size, center) = match st.extent(&ins.block.to_uppercase(), blk) {
                    Some((lo, hi)) => (
                        ((hi.0 - lo.0) * m.kx(), (hi.1 - lo.1) * m.ky()),
                        m.p(bx + (lo.0 + hi.0) / 2.0, by + (lo.1 + hi.1) / 2.0),
                    ),
                    None => ((0.0, 0.0), m.p(bx, by)),
                };
                st.out.inserts.push(InsertInfo {
                    name: shown.clone(),
                    layer: common.layer.clone(),
                    pos: m.p(bx, by),
                    rotation: m.angle(0.0),
                    scale: (m.kx(), m.ky()),
                    size,
                    center,
                    attribs: ins
                        .attribs
                        .iter()
                        .map(|a| (decode_unicode(&a.tag), decode_unicode(&a.value)))
                        .collect(),
                });
            }
            if let Some(kind) = kind {
                st.out.hints.push(InsertHint {
                    name: shown.clone(),
                    layer: common.layer.clone(),
                    kind,
                    pos: m.p(blk.base[0], blk.base[1]),
                    rotation: m.angle(0.0),
                    scale: (m.kx(), m.ky()),
                });
            }
            block_body(st, blk, &m, &inner);
            // ATTRIBs hold coordinates already placed in the containing space; once per INSERT.
            if r == 0 && c == 0 {
                for a in &ins.attribs {
                    attrib(st, a, parent, &inner);
                }
            }
            if st.out.report.truncated {
                break 'arr;
            }
        }
    }
    st.stack.pop();
}

/// Anonymous blocks that are never block placements: dimensions, hatches, spaces.
/// `*U…` (dynamic-block copies), `*A…` (groups), `*T…` (tables) stay listed.
fn skip_anonymous(name: &str) -> bool {
    let n = name.to_uppercase();
    ["*D", "*X", "*MODEL", "*PAPER"]
        .iter()
        .any(|p| n.starts_with(p))
}

/// Source block of each `*U…` copy of a dynamic block (upper-case anonymous name → name):
/// the BLOCK_RECORD `AcDbBlockRepBTag` xdata handle when the table is kept, else the only named
/// block with the same entity signature (the copy of an unmodified default state).
fn anon_sources(doc: &DxfDocument) -> HashMap<String, String> {
    let mut out = HashMap::new();
    if let Some(t) = doc.other_tables.iter().find(|t| t.name == "BLOCK_RECORD") {
        let mut by_handle = HashMap::new();
        let mut reps = vec![];
        for e in &t.entries {
            let name = e
                .iter()
                .skip(1)
                .find(|p| p.code == 2)
                .map(|p| p.value.clone());
            let handle = e
                .iter()
                .skip(1)
                .find(|p| p.code == 5)
                .map(|p| p.value.to_uppercase());
            let (Some(name), Some(h)) = (name, handle) else {
                continue;
            };
            if let Some(k) = e
                .iter()
                .position(|p| p.code == 1001 && p.value == "AcDbBlockRepBTag")
            {
                if let Some(src) = e[k + 1..]
                    .iter()
                    .take_while(|p| p.code != 1001)
                    .find(|p| p.code == 1005)
                {
                    reps.push((name.clone(), src.value.to_uppercase()));
                }
            }
            by_handle.insert(h, name);
        }
        for (anon, src) in reps {
            if let Some(n) = by_handle.get(&src).filter(|n| !n.starts_with('*')) {
                out.insert(anon.to_uppercase(), n.clone());
            }
        }
    }
    let is_u = |n: &str| n.len() > 2 && n.is_char_boundary(2) && n[..2].eq_ignore_ascii_case("*U");
    if !doc
        .blocks
        .iter()
        .any(|b| is_u(&b.name) && !out.contains_key(&b.name.to_uppercase()))
    {
        return out;
    }
    let mut named: HashMap<Vec<String>, Vec<&str>> = HashMap::new();
    for b in doc.blocks.iter().filter(|b| !b.name.starts_with('*')) {
        if let Some(sig) = block_signature(b) {
            named.entry(sig).or_default().push(&b.name);
        }
    }
    for b in doc.blocks.iter().filter(|b| is_u(&b.name)) {
        let key = b.name.to_uppercase();
        if out.contains_key(&key) {
            continue;
        }
        let Some(sig) = block_signature(b) else {
            continue;
        };
        if let Some([only]) = named.get(&sig).map(|v| v.as_slice()) {
            out.insert(key, only.to_string());
        }
    }
    out
}

/// Sorted entity kinds (polyline vertex counts, nested block names) and attribute tags;
/// `None` for blocks too small to identify (fewer than two items).
fn block_signature(b: &Block) -> Option<Vec<String>> {
    let mut v: Vec<String> = b
        .entities
        .iter()
        .map(|e| match e {
            Entity::Line(_) => "LINE".to_string(),
            Entity::LwPolyline(p) => format!("LWPOLYLINE:{}", p.vertices.len()),
            Entity::Circle(_) => "CIRCLE".into(),
            Entity::Arc(_) => "ARC".into(),
            Entity::Text(_) => "TEXT".into(),
            Entity::MText(_) => "MTEXT".into(),
            Entity::Insert(i) => format!("INSERT:{}", i.block.to_uppercase()),
            Entity::AttDef(a) => format!("ATTDEF:{}", a.tag.to_uppercase()),
            Entity::Unknown(r) => r.first().map_or(String::new(), |p| p.value.clone()),
        })
        .chain(
            b.attdefs
                .iter()
                .map(|a| format!("ATTDEF:{}", a.tag.to_uppercase())),
        )
        .collect();
    if v.len() < 2 {
        return None;
    }
    v.sort();
    Some(v)
}

fn block_body(st: &mut St, blk: &Block, m: &Aff, ctx: &Ctx) {
    let body = st.body(&blk.name.to_uppercase(), blk);
    for e in body.iter() {
        if st.out.report.truncated {
            return;
        }
        match e {
            Lowered::Ent(Entity::Insert(i)) => insert(st, i, m, Some(ctx)),
            Lowered::Ent(e) => xf_entity(st, e, m, ctx),
            Lowered::Fill(f) => fill(st, f, m, Some(ctx)),
        }
    }
    for d in &blk.attdefs {
        // Constant attributes are geometry; variable ones show only through ATTRIB.
        if d.flags & 1 != 0 || d.flags & 2 == 0 || d.default.is_empty() {
            continue;
        }
        if let Some(mm) = with_ocs(m, &d.common) {
            let rot = extra_f(&d.common.extra, 50, 0.0);
            push_text(
                st,
                resolve(&d.common, ctx),
                mm,
                d.pos,
                d.height,
                rot,
                d.default.clone(),
            );
        }
    }
}

fn attrib(st: &mut St, a: &Attrib, parent: &Aff, ctx: &Ctx) {
    if a.flags & 1 != 0 || a.value.is_empty() {
        return;
    }
    if let Some(mm) = with_ocs(parent, &a.common) {
        let rot = extra_f(&a.common.extra, 50, 0.0);
        push_text(
            st,
            resolve(&a.common, ctx),
            mm,
            a.pos,
            a.height,
            rot,
            a.value.clone(),
        );
    }
}

fn push_text(st: &mut St, common: Common, m: Aff, pos: Pt, h: f64, rot: f64, text: String) {
    let p = m.p(pos[0], pos[1]);
    st.push(Entity::Text(Text {
        common,
        pos: [p.0, p.1, pos[2]],
        height: h * m.ky(),
        text,
        rotation: m.angle(rot),
        style: None,
    }));
}

fn arc_points(c: (f64, f64), r: f64, a0: f64, a1: f64) -> Vec<(f64, f64)> {
    let n = (((a1 - a0).abs() / (PI / 32.0)).ceil() as usize).clamp(1, 256);
    (0..=n)
        .map(|i| {
            let t = a0 + (a1 - a0) * i as f64 / n as f64;
            (c.0 + r * t.cos(), c.1 + r * t.sin())
        })
        .collect()
}

/// Interior points of the arc between two bulged vertices (local coordinates).
fn bulge_points(v: &LwVertex, w: &LwVertex) -> Vec<(f64, f64)> {
    let th = 4.0 * v.bulge.atan();
    let (dx, dy) = (w.x - v.x, w.y - v.y);
    let ch = dx.hypot(dy);
    if ch <= 0.0 || th.abs() < 1e-12 {
        return vec![];
    }
    let r = ch / (2.0 * (th / 2.0).sin().abs());
    let h = r * (th / 2.0).cos() * th.signum();
    let (nx, ny) = (-dy / ch, dx / ch);
    let c = ((v.x + w.x) / 2.0 + nx * h, (v.y + w.y) / 2.0 + ny * h);
    let a0 = (v.y - c.1).atan2(v.x - c.0);
    let pts = arc_points(c, r, a0, a0 + th);
    pts[1..pts.len() - 1].to_vec()
}

fn xf_entity(st: &mut St, e: &Entity, m: &Aff, ctx: &Ctx) {
    match e {
        Entity::Line(l) => {
            let Some(mm) = with_ocs(m, &l.common) else {
                return;
            };
            let (a, b) = (mm.p(l.start[0], l.start[1]), mm.p(l.end[0], l.end[1]));
            st.push(Entity::Line(Line {
                common: resolve(&l.common, ctx),
                start: [a.0, a.1, l.start[2]],
                end: [b.0, b.1, l.end[2]],
            }));
        }
        Entity::LwPolyline(pl) => {
            let Some(mm) = with_ocs(m, &pl.common) else {
                return;
            };
            let uni = mm.uniform();
            let sgn = if mm.det() < 0.0 { -1.0 } else { 1.0 };
            let k = mm.kx();
            let mut out = LwPolyline {
                common: resolve(&pl.common, ctx),
                flags: pl.flags,
                vertices: vec![],
            };
            let n = pl.vertices.len();
            for (i, v) in pl.vertices.iter().enumerate() {
                let p = mm.p(v.x, v.y);
                let mut nv = LwVertex {
                    x: p.0,
                    y: p.1,
                    bulge: v.bulge * sgn,
                    start_width: v.start_width * k,
                    end_width: v.end_width * k,
                };
                let next = if i + 1 < n {
                    Some(&pl.vertices[i + 1])
                } else if pl.closed() && n > 1 {
                    Some(&pl.vertices[0])
                } else {
                    None
                };
                match next {
                    // Arc segment under non-uniform scale becomes chords.
                    Some(w) if !uni && v.bulge != 0.0 => {
                        nv.bulge = 0.0;
                        out.vertices.push(nv);
                        for q in bulge_points(v, w) {
                            let q = mm.p(q.0, q.1);
                            out.vertices.push(LwVertex {
                                x: q.0,
                                y: q.1,
                                ..Default::default()
                            });
                        }
                    }
                    _ => out.vertices.push(nv),
                }
            }
            st.push(Entity::LwPolyline(out));
        }
        Entity::Circle(ci) => {
            let Some(mm) = with_ocs(m, &ci.common) else {
                return;
            };
            let c = resolve(&ci.common, ctx);
            if mm.uniform() {
                let p = mm.p(ci.center[0], ci.center[1]);
                let mut o = ci.clone();
                o.common = c;
                o.center = [p.0, p.1, ci.center[2]];
                o.radius = ci.radius * mm.kx();
                st.push(Entity::Circle(o));
            } else {
                let pts = arc_points((ci.center[0], ci.center[1]), ci.radius, 0.0, 2.0 * PI);
                poly_from(st, c, &mm, &pts[..pts.len() - 1], true);
            }
        }
        Entity::Arc(a) => {
            let Some(mm) = with_ocs(m, &a.common) else {
                return;
            };
            let c = resolve(&a.common, ctx);
            if mm.uniform() {
                let p = mm.p(a.center[0], a.center[1]);
                let alpha = mm.angle(0.0);
                let (s, e) = if mm.det() < 0.0 {
                    (alpha - a.end_angle, alpha - a.start_angle)
                } else {
                    (a.start_angle + alpha, a.end_angle + alpha)
                };
                let mut o = a.clone();
                o.common = c;
                o.center = [p.0, p.1, a.center[2]];
                o.radius = a.radius * mm.kx();
                o.start_angle = s.rem_euclid(360.0);
                o.end_angle = e.rem_euclid(360.0);
                st.push(Entity::Arc(o));
            } else {
                let (s, mut e) = (a.start_angle.to_radians(), a.end_angle.to_radians());
                while e <= s {
                    e += 2.0 * PI;
                }
                let pts = arc_points((a.center[0], a.center[1]), a.radius, s, e);
                poly_from(st, c, &mm, &pts, false);
            }
        }
        Entity::Text(t) => {
            let Some(mm) = with_ocs(m, &t.common) else {
                return;
            };
            let p = mm.p(t.pos[0], t.pos[1]);
            let mut o = t.clone();
            o.common = resolve(&t.common, ctx);
            o.pos = [p.0, p.1, t.pos[2]];
            o.height = t.height * mm.ky();
            o.rotation = mm.angle(t.rotation);
            // the second alignment point (codes 11/21) moves with the text
            let ax = t.common.extra.iter().position(|q| q.code == 11);
            let ay = t.common.extra.iter().position(|q| q.code == 21);
            if let (Some(i), Some(j)) = (ax, ay) {
                let v = |k: usize| t.common.extra[k].value.trim().parse::<f64>().ok();
                if let (Some(x), Some(y)) = (v(i), v(j)) {
                    let q = mm.p(x, y);
                    o.common.extra[i].value = q.0.to_string();
                    o.common.extra[j].value = q.1.to_string();
                }
            }
            st.push(Entity::Text(o));
        }
        Entity::MText(t) => {
            let Some(mm) = with_ocs(m, &t.common) else {
                return;
            };
            let p = mm.p(t.pos[0], t.pos[1]);
            let mut o = t.clone();
            o.common = resolve(&t.common, ctx);
            o.pos = [p.0, p.1, t.pos[2]];
            o.height = t.height * mm.ky();
            o.width = t.width * mm.kx();
            o.rotation = mm.angle(t.rotation);
            st.push(Entity::MText(o));
        }
        Entity::Unknown(p) => {
            for x in super::annot::lower(p, &st.images) {
                match x {
                    Entity::Insert(i) => insert(st, &i, m, Some(ctx)),
                    other => xf_entity(st, &other, m, ctx),
                }
            }
        }
        Entity::AttDef(_) | Entity::Insert(_) => {}
    }
}

fn poly_from(st: &mut St, common: Common, m: &Aff, pts: &[(f64, f64)], closed: bool) {
    st.push(Entity::LwPolyline(LwPolyline {
        common,
        flags: closed as i32,
        vertices: pts
            .iter()
            .map(|&(x, y)| {
                let q = m.p(x, y);
                LwVertex {
                    x: q.0,
                    y: q.1,
                    ..Default::default()
                }
            })
            .collect(),
    }));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn line(layer: &str, a: [f64; 2], b: [f64; 2]) -> Entity {
        Entity::Line(Line {
            common: Common::on_layer(layer),
            start: [a[0], a[1], 0.0],
            end: [b[0], b[1], 0.0],
        })
    }
    fn ins(name: &str, x: f64, y: f64) -> Insert {
        Insert {
            common: Common::on_layer("A"),
            block: name.into(),
            pos: [x, y, 0.0],
            ..Default::default()
        }
    }
    fn blk(name: &str, ents: Vec<Entity>) -> Block {
        let mut b = Block::new(name);
        b.entities = ents;
        b
    }
    fn doc(blocks: Vec<Block>, ents: Vec<Entity>) -> DxfDocument {
        DxfDocument {
            blocks,
            entities: ents,
            ..Default::default()
        }
    }
    fn run(d: &DxfDocument) -> Expanded {
        expand_inserts(d, &ExpandOptions::default())
    }
    fn seg(e: &Entity) -> ([f64; 2], [f64; 2]) {
        let Entity::Line(l) = e else {
            panic!("not line")
        };
        ([l.start[0], l.start[1]], [l.end[0], l.end[1]])
    }
    fn near(a: [f64; 2], b: [f64; 2]) {
        assert!(
            (a[0] - b[0]).abs() < 1e-9 && (a[1] - b[1]).abs() < 1e-9,
            "{a:?} != {b:?}"
        );
    }

    #[test]
    fn translate_scale_rotate_exact() {
        let mut i = ins("B", 10.0, 20.0);
        i.scale = [2.0, 3.0, 1.0];
        i.rotation = 90.0;
        let d = doc(
            vec![blk("B", vec![line("0", [1.0, 0.0], [1.0, 1.0])])],
            vec![Entity::Insert(i)],
        );
        let (a, b) = seg(&run(&d).entities[0]);
        // (1,0)->(2,0)->rot90 (0,2)->(10,22); (1,1)->(2,3)->(-3,2)->(7,22)
        near(a, [10.0, 22.0]);
        near(b, [7.0, 22.0]);
    }

    #[test]
    fn negative_scale_mirrors_and_block_base() {
        let mut i = ins("B", 5.0, 0.0);
        i.scale = [-1.0, 1.0, 1.0];
        let mut b = blk("B", vec![line("0", [3.0, 0.0], [4.0, 1.0])]);
        b.base = [1.0, 0.0, 0.0];
        let d = doc(vec![b], vec![Entity::Insert(i)]);
        let (a, bb) = seg(&run(&d).entities[0]);
        near(a, [3.0, 0.0]);
        near(bb, [2.0, 1.0]);
    }

    #[test]
    fn nested_blocks_compose() {
        let mut inner = ins("IN", 1.0, 0.0);
        inner.rotation = 90.0;
        let d = doc(
            vec![
                blk("IN", vec![line("0", [0.0, 0.0], [1.0, 0.0])]),
                blk("OUT", vec![Entity::Insert(inner)]),
            ],
            vec![Entity::Insert({
                let mut o = ins("OUT", 100.0, 0.0);
                o.scale = [2.0, 2.0, 2.0];
                o
            })],
        );
        let (a, b) = seg(&run(&d).entities[0]);
        near(a, [102.0, 0.0]);
        near(b, [102.0, 2.0]);
    }

    #[test]
    fn cycle_and_depth_and_missing() {
        let d = doc(
            vec![
                blk(
                    "A",
                    vec![
                        Entity::Insert(ins("B", 0.0, 0.0)),
                        line("0", [0.0, 0.0], [1.0, 0.0]),
                    ],
                ),
                blk(
                    "B",
                    vec![
                        Entity::Insert(ins("A", 0.0, 0.0)),
                        Entity::Insert(ins("NOPE", 0.0, 0.0)),
                    ],
                ),
            ],
            vec![Entity::Insert(ins("a", 0.0, 0.0))],
        );
        let r = run(&d);
        assert_eq!(r.entities.len(), 1);
        assert_eq!((r.report.cycles, r.report.missing_blocks), (1, 1));

        // chain of 40 blocks: depth 32 exceeded
        let mut blocks = vec![blk("N40", vec![line("0", [0.0, 0.0], [1.0, 0.0])])];
        for k in (0..40).rev() {
            blocks.push(blk(
                &format!("N{k}"),
                vec![Entity::Insert(ins(&format!("N{}", k + 1), 0.0, 0.0))],
            ));
        }
        let d = doc(blocks, vec![Entity::Insert(ins("N0", 0.0, 0.0))]);
        let r = run(&d);
        assert_eq!(r.entities.len(), 0);
        assert_eq!(r.report.depth_exceeded, 1);
    }

    #[test]
    fn minsert_grid_rotated() {
        let mut i = ins("B", 0.0, 0.0);
        i.rotation = 90.0;
        i.common.extra = vec![
            Pair::new(70, "3"),
            Pair::new(71, "2"),
            Pair::new(44, "10"),
            Pair::new(45, "5"),
        ];
        let d = doc(
            vec![blk("B", vec![line("0", [0.0, 0.0], [1.0, 0.0])])],
            vec![Entity::Insert(i)],
        );
        let r = run(&d);
        assert_eq!(r.entities.len(), 6);
        // copy (col 2,row 1): offset (20,5) rotated 90 -> (-5,20); line (0,0)->(0,1)
        let (a, b) = seg(&r.entities[5]);
        near(a, [-5.0, 20.0]);
        near(b, [-5.0, 21.0]);
    }

    #[test]
    fn attrib_and_constant_attdef_text() {
        let mut b = blk("B", vec![]);
        b.attdefs.push(AttDef {
            tag: "K".into(),
            default: "const".into(),
            flags: 2,
            height: 1.0,
            pos: [1.0, 1.0, 0.0],
            common: Common::on_layer("0"),
            ..Default::default()
        });
        b.attdefs.push(AttDef {
            tag: "V".into(),
            default: "var".into(),
            flags: 0,
            ..Default::default()
        });
        let mut i = ins("B", 10.0, 10.0);
        i.scale = [2.0, 2.0, 2.0];
        i.attribs.push(Attrib {
            tag: "V".into(),
            value: "D-12".into(),
            pos: [11.0, 12.0, 0.0],
            height: 2.5,
            common: Common::on_layer("TXT"),
            ..Default::default()
        });
        i.attribs.push(Attrib {
            value: "hidden".into(),
            flags: 1,
            ..Default::default()
        });
        let r = run(&doc(vec![b], vec![Entity::Insert(i)]));
        assert_eq!(r.entities.len(), 2);
        let Entity::Text(c) = &r.entities[0] else {
            panic!()
        };
        assert_eq!(
            (
                c.text.as_str(),
                c.pos[0],
                c.pos[1],
                c.height,
                c.common.layer.as_str()
            ),
            ("const", 12.0, 12.0, 2.0, "A")
        );
        let Entity::Text(v) = &r.entities[1] else {
            panic!()
        };
        assert_eq!(
            (
                v.text.as_str(),
                v.pos[0],
                v.pos[1],
                v.height,
                v.common.layer.as_str()
            ),
            ("D-12", 11.0, 12.0, 2.5, "TXT")
        );
    }

    #[test]
    fn extrusion_flip_and_arc_mirror() {
        let mut a = Arc {
            center: [1.0, 0.0, 0.0],
            radius: 2.0,
            start_angle: 0.0,
            end_angle: 90.0,
            ..Default::default()
        };
        a.common.extra = vec![
            Pair::new(210, "0"),
            Pair::new(220, "0"),
            Pair::new(230, "-1"),
        ];
        let d = doc(
            vec![blk("B", vec![Entity::Arc(a)])],
            vec![Entity::Insert(ins("B", 0.0, 0.0))],
        );
        let Entity::Arc(o) = &run(&d).entities[0] else {
            panic!()
        };
        // x mirrored: center (-1,0); angles 0..90 -> 180-90..180-0
        assert!((o.center[0] + 1.0).abs() < 1e-9 && o.center[1].abs() < 1e-9);
        assert!((o.start_angle - 90.0).abs() < 1e-9 && (o.end_angle - 180.0).abs() < 1e-9);
        assert!(o.common.extra.is_empty());

        // insert with flipped extrusion and a tilted one (skipped)
        let mut i = ins("L", 3.0, 0.0);
        i.common.extra = vec![Pair::new(230, "-1")];
        let mut t = ins("L", 0.0, 0.0);
        t.common.extra = vec![Pair::new(210, "1")];
        let d = doc(
            vec![blk("L", vec![line("0", [0.0, 0.0], [1.0, 0.0])])],
            vec![Entity::Insert(i), Entity::Insert(t)],
        );
        let r = run(&d);
        assert_eq!(r.entities.len(), 1);
        assert_eq!(r.report.skipped_degenerate, 1);
        let (a, b) = seg(&r.entities[0]);
        near(a, [-3.0, 0.0]);
        near(b, [-4.0, 0.0]);
    }

    #[test]
    fn byblock_and_layer0_inheritance() {
        let mut l1 = Line {
            common: Common::on_layer("0"),
            end: [1.0, 0.0, 0.0],
            ..Default::default()
        };
        l1.common.color = Some(0);
        l1.common.linetype = Some("ByBlock".into());
        let mut l2 = Line {
            common: Common::on_layer("WALLS"),
            end: [1.0, 0.0, 0.0],
            ..Default::default()
        };
        l2.common.color = Some(3);
        let mut outer = ins("IN", 0.0, 0.0);
        outer.common.layer = "0".into();
        outer.common.color = Some(0);
        outer.common.linetype = Some("BYBLOCK".into());
        let mut top = ins("OUT", 0.0, 0.0);
        top.common.layer = "DOORS".into();
        top.common.color = Some(5);
        top.common.linetype = Some("DASHED".into());
        let d = doc(
            vec![
                blk("IN", vec![Entity::Line(l1), Entity::Line(l2)]),
                blk("OUT", vec![Entity::Insert(outer)]),
            ],
            vec![Entity::Insert(top)],
        );
        let r = run(&d);
        let (Entity::Line(a), Entity::Line(b)) = (&r.entities[0], &r.entities[1]) else {
            panic!()
        };
        assert_eq!(
            (
                a.common.layer.as_str(),
                a.common.color,
                a.common.linetype.as_deref()
            ),
            ("DOORS", Some(5), Some("DASHED"))
        );
        assert_eq!(
            (b.common.layer.as_str(), b.common.color),
            ("WALLS", Some(3))
        );
    }

    #[test]
    fn nonuniform_scale_turns_circle_into_polyline() {
        let c = Circle {
            radius: 1.0,
            ..Default::default()
        };
        let mut i = ins("B", 0.0, 0.0);
        i.scale = [2.0, 1.0, 1.0];
        let r = run(&doc(
            vec![blk("B", vec![Entity::Circle(c)])],
            vec![Entity::Insert(i)],
        ));
        let Entity::LwPolyline(p) = &r.entities[0] else {
            panic!()
        };
        assert!(p.closed());
        let xmax = p.vertices.iter().map(|v| v.x).fold(f64::MIN, f64::max);
        assert!((xmax - 2.0).abs() < 1e-9);
    }

    #[test]
    fn budget_truncates_and_hints() {
        let mut i = ins("DOOR-90", 0.0, 0.0);
        i.common.extra = vec![
            Pair::new(70, "100"),
            Pair::new(71, "100"),
            Pair::new(44, "1"),
            Pair::new(45, "1"),
        ];
        let d = doc(
            vec![blk("DOOR-90", vec![line("0", [0.0, 0.0], [1.0, 0.0])])],
            vec![Entity::Insert(i)],
        );
        let o = ExpandOptions {
            max_entities: 50,
            ..Default::default()
        };
        let r = expand_inserts(&d, &o);
        assert!(r.report.truncated);
        assert!(r.entities.len() <= 50);
        assert_eq!(r.hints[0].kind, BlockKind::Door);
        assert_eq!(block_kind(&format!("{}_1", crate::ru_fixtures::ru("window_word"))), Some(BlockKind::Window));
        assert_eq!(block_kind("chair"), None);
    }

    #[test]
    fn nested_insert_extent_gives_door_width() {
        // door made only of sub-blocks: leaf (0.9 wide) and frame
        let leaf = blk("LEAF", vec![line("0", [0.0, 0.0], [900.0, 0.0])]);
        let frame = blk("FRAME", vec![line("0", [0.0, 0.0], [0.0, 50.0])]);
        let mut sub = ins("LEAF", 0.0, 0.0);
        sub.rotation = 0.0;
        let door = blk(
            "DOOR_ASM",
            vec![
                Entity::Insert(sub),
                Entity::Insert(ins("FRAME", 900.0, 0.0)),
            ],
        );
        // self-reference must not loop
        let mut cyc = blk("CYC", vec![line("0", [0.0, 0.0], [10.0, 0.0])]);
        cyc.entities.push(Entity::Insert(ins("CYC", 5.0, 0.0)));
        let d = doc(
            vec![leaf, frame, door, cyc],
            vec![
                Entity::Insert(ins("DOOR_ASM", 100.0, 0.0)),
                Entity::Insert(ins("CYC", 0.0, 0.0)),
            ],
        );
        let r = run(&d);
        let i = r.inserts.iter().find(|i| i.name == "DOOR_ASM").unwrap();
        assert!((i.size.0 - 900.0).abs() < 1e-9, "{:?}", i.size);
        assert!((i.size.1 - 50.0).abs() < 1e-9, "{:?}", i.size);
        assert!((i.center.0 - 550.0).abs() < 1e-9);
        let c = r.inserts.iter().find(|i| i.name == "CYC").unwrap();
        assert!((c.size.0 - 10.0).abs() < 1e-9, "{:?}", c.size);
    }

    #[test]
    fn dynamic_block_copy_reports_source_name() {
        let two = || {
            vec![
                line("0", [0.0, 0.0], [900.0, 0.0]),
                line("0", [0.0, 0.0], [0.0, 900.0]),
            ]
        };
        // by signature: *U7 is the only match of "DOOR_DYN"
        let d = doc(
            vec![blk("DOOR_DYN", two()), blk("*U7", two()), blk("*D1", two())],
            vec![
                Entity::Insert(ins("*U7", 0.0, 0.0)),
                Entity::Insert(ins("*D1", 0.0, 0.0)),
            ],
        );
        let r = run(&d);
        assert_eq!(r.inserts.len(), 1, "*D skipped, *U listed");
        assert_eq!(r.inserts[0].name, "DOOR_DYN");
        assert_eq!(r.hints.len(), 1);
        assert_eq!(r.hints[0].kind, BlockKind::Door);

        // by BLOCK_RECORD xdata: geometry differs (stretched), handle decides
        let mut d = doc(
            vec![
                blk("WINDOW_DYN", two()),
                blk("*U12", vec![line("0", [0.0, 0.0], [1500.0, 0.0])]),
            ],
            vec![Entity::Insert(ins("*U12", 0.0, 0.0))],
        );
        let rec = |h: &str, name: &str, xd: Option<&str>| {
            let mut v = vec![
                Pair::new(0, "BLOCK_RECORD"),
                Pair::new(5, h),
                Pair::new(100, "AcDbSymbolTableRecord"),
                Pair::new(2, name),
            ];
            if let Some(src) = xd {
                v.extend([
                    Pair::new(1001, "AcDbBlockRepBTag"),
                    Pair::new(1070, "1"),
                    Pair::new(1005, src),
                ]);
            }
            v
        };
        d.other_tables.push(crate::dxf::RawTable {
            name: "BLOCK_RECORD".into(),
            header: vec![],
            entries: vec![rec("1F", "WINDOW_DYN", None), rec("2A", "*U12", Some("1f"))],
        });
        let r = run(&d);
        assert_eq!(r.inserts[0].name, "WINDOW_DYN");
        assert_eq!(r.hints[0].kind, BlockKind::Window);

        // unresolved copy keeps its own name
        let d = doc(
            vec![blk("*U3", two())],
            vec![Entity::Insert(ins("*U3", 0.0, 0.0))],
        );
        assert_eq!(run(&d).inserts[0].name, "*U3");
    }

    #[test]
    fn attrib_tags_decode_unicode() {
        let mut i = ins("D", 0.0, 0.0);
        i.attribs.push(Attrib {
            common: Common::on_layer("0"),
            tag: "\\U+0428\\U+0418\\U+0420".into(),
            value: "900".into(),
            ..Default::default()
        });
        let d = doc(
            vec![blk("D", vec![line("0", [0.0, 0.0], [1.0, 0.0])])],
            vec![Entity::Insert(i)],
        );
        assert_eq!(run(&d).inserts[0].attribs[0].0, crate::ru_fixtures::ru("width_abbr"));
    }

    #[test]
    fn block_pattern_is_cached_and_charged() {
        let sq = vec![(0.0, 0.0), (10.0, 0.0), (10.0, 10.0), (0.0, 10.0)];
        let mut rec = lower::hatch_record(&Common::on_layer("H"), &[sq], "ANSI31");
        let k = rec.iter().position(|p| p.code == 78).unwrap();
        rec.splice(
            k..=k,
            [
                Pair::new(78, "1"),
                Pair::new(53, "45"),
                Pair::new(43, "0"),
                Pair::new(44, "0"),
                Pair::new(45, "-0.5"),
                Pair::new(46, "0.5"),
                Pair::new(79, "0"),
            ],
        );
        let ents: Vec<Entity> = (0..50)
            .map(|k| Entity::Insert(ins("HB", k as f64 * 20.0, 0.0)))
            .collect();
        let d = doc(vec![blk("HB", vec![Entity::Unknown(rec)])], ents);
        let r = run(&d);
        assert!(!r.report.truncated);
        assert_eq!(r.fills.len(), 50);
        let n = r.fills[0].segs.len();
        assert!(n > 10, "{n}");
        assert!(r.fills.iter().all(|f| f.segs.len() == n));
        // pattern segments count against the budget
        let o = ExpandOptions {
            max_entities: 10 * n,
            ..Default::default()
        };
        let r = expand_inserts(&d, &o);
        assert!(r.report.truncated);
        assert!(r.fills.len() < 12, "{}", r.fills.len());
    }

    fn table_rec(block: &str) -> Entity {
        Entity::Unknown(vec![
            Pair::new(0, "ACAD_TABLE"),
            Pair::new(8, "A"),
            Pair::new(100, "AcDbBlockReference"),
            Pair::new(2, block),
            Pair::new(10, "10"),
            Pair::new(20, "20"),
            Pair::new(30, "0"),
            Pair::new(100, "AcDbTable"),
            Pair::new(11, "0"),
            Pair::new(21, "1"),
            Pair::new(31, "0"),
            Pair::new(41, "5"),
        ])
    }

    #[test]
    fn dxf_acad_table_expanded() {
        let b = blk("*T1", vec![line("0", [1.0, 0.0], [3.0, 0.0])]);
        let r = run(&doc(vec![b], vec![table_rec("*T1")]));
        // Direction (0,1) turns the block a quarter turn; row height 41 is not a scale.
        let (a, e) = seg(&r.entities[1]);
        near(a, [10.0, 21.0]);
        near(e, [10.0, 23.0]);
        assert_eq!(r.report.inserts_expanded, 1);
    }

    #[test]
    fn dxf_table_missing_block_counted() {
        let r = run(&doc(vec![], vec![table_rec("*T9")]));
        assert_eq!(r.report.missing_blocks, 1);
    }

    #[test]
    fn block_reference_class_record_expands_its_block() {
        let raw = Entity::Unknown(vec![
            Pair::new(0, "ACIDBLOCKREFERENCE"),
            Pair::new(8, "A"),
            Pair::new(100, "AcDbBlockReference"),
            Pair::new(2, "B"),
            Pair::new(10, "10"),
            Pair::new(20, "20"),
            Pair::new(30, "0"),
        ]);
        let b = blk("B", vec![line("0", [1.0, 0.0], [1.0, 1.0])]);
        let r = run(&doc(vec![b.clone()], vec![raw.clone()]));
        // The record itself is kept (round trip), its block is drawn.
        assert!(matches!(&r.entities[0], Entity::Unknown(_)));
        let (a, e) = seg(&r.entities[1]);
        near(a, [11.0, 20.0]);
        near(e, [11.0, 21.0]);
        assert_eq!(r.report.inserts_expanded, 1);
        // Nested inside another block.
        let outer = blk("OUT", vec![raw]);
        let r = run(&doc(
            vec![b, outer],
            vec![Entity::Insert(ins("OUT", 100.0, 0.0))],
        ));
        let (a, _) = seg(&r.entities[0]);
        near(a, [111.0, 20.0]);
    }
}
