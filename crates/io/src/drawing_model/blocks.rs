//! Block definitions and top-level INSERTs of a drawing file, kept through edits so the DXF
//! export writes blocks back. An INSERT whose expanded primitives are all still in the underlay,
//! untouched, is written as an INSERT of its definition (scale, rotation, array, ATTRIBs) moved
//! with the underlay; one with an edited, moved or deleted primitive is written as primitives.
// the write-back half lives only with the test-only DXF writer (`dxf-write`)
#![cfg_attr(not(any(test, feature = "dxf-write")), allow(dead_code, unused_imports))]

use std::collections::{HashMap, HashSet};
use std::f64::consts::{PI, TAU};
use std::sync::Arc;

use super::source::{plan_flip, Pen, UArc, UFill, UPoly, Underlay};
#[cfg(any(test, feature = "dxf-write"))]
use super::write::{Placement, WriteReport};
use crate::dxf::{
    expand_inserts, lower::in_paper, Block, DxfDocument, Entity, ExpandOptions, HeaderVar, Insert,
    Layer, LineType, Pair, TextStyle,
};

type P = (f64, f64);

/// Tables of the file that block contents refer to (file units).
#[derive(Clone, Debug, Default, PartialEq)]
pub struct BlockSrc {
    /// Definitions reachable from the captured INSERTs, nested ones included.
    pub defs: Vec<Block>,
    /// `$LTSCALE` of the file (dash lengths of pens).
    pub header: Vec<HeaderVar>,
    pub layers: Vec<Layer>,
    pub ltypes: Vec<LineType>,
    pub styles: Vec<TextStyle>,
}

/// A model-space INSERT of the file and where the underlay has taken it since.
#[derive(Clone, Debug, PartialEq)]
pub struct URef {
    /// The INSERT as read (file units), with its ATTRIBs.
    pub insert: Insert,
    /// World base point (file units) and X axis direction (rad) in the file.
    pub base0: P,
    pub rot0: f64,
    /// The same now, in model metres and radians.
    pub pos: P,
    pub rotation: f64,
}

/// Blocks of an underlay; the tables are shared by copies of it.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct UBlocks {
    pub src: Arc<BlockSrc>,
    pub refs: Vec<URef>,
}

impl UBlocks {
    pub fn is_empty(&self) -> bool {
        self.refs.is_empty()
    }

    /// Rotation by `rot` about `pivot`, then shift `d` (model m), as the whole underlay moves.
    pub(super) fn transform(&mut self, pivot: P, rot: f64, d: P) {
        let (s, c) = rot.sin_cos();
        for r in &mut self.refs {
            let (x, y) = (r.pos.0 - pivot.0, r.pos.1 - pivot.1);
            r.pos = (pivot.0 + c * x - s * y + d.0, pivot.1 + s * x + c * y + d.1);
            r.rotation += rot;
        }
    }
}

fn upper(s: &str) -> String {
    s.to_uppercase()
}

fn same_name(a: &str, b: &str) -> bool {
    a == b || upper(a) == upper(b)
}

/// The two space blocks are never INSERT targets.
fn space(name: &str) -> bool {
    let n = upper(name);
    n.starts_with("*MODEL_SPACE") || n.starts_with("*PAPER_SPACE")
}

fn is_dimension(r: &[Pair]) -> bool {
    r.first().is_some_and(|p| p.value.ends_with("DIMENSION"))
}

/// Block a block entity refers to: a nested INSERT or a dimension picture.
fn refers(e: &Entity) -> Option<&str> {
    match e {
        Entity::Insert(i) => Some(i.block.as_str()),
        Entity::Unknown(r) if is_dimension(r) => r
            .iter()
            .skip(1)
            .find(|p| p.code == 2)
            .map(|p| p.value.as_str()),
        _ => None,
    }
}

/// Indices of `defs` reachable from `roots`, in `defs` order.
fn closure<'a>(defs: &[Block], roots: impl Iterator<Item = &'a str>) -> Vec<usize> {
    let mut by: HashMap<String, usize> = HashMap::new();
    for (i, b) in defs.iter().enumerate() {
        by.entry(upper(&b.name)).or_insert(i);
    }
    let mut stack: Vec<String> = roots.map(upper).collect();
    let mut seen = HashSet::new();
    while let Some(n) = stack.pop() {
        if space(&n) {
            continue;
        }
        let Some(&k) = by.get(&n) else {
            continue;
        };
        if seen.insert(k) {
            stack.extend(defs[k].entities.iter().filter_map(refers).map(upper));
        }
    }
    let mut v: Vec<usize> = seen.into_iter().collect();
    v.sort_unstable();
    v
}

/// World base point (file units) and X axis direction (rad) of an INSERT.
fn pose0(i: &Insert) -> (P, f64) {
    let r = i.rotation.to_radians();
    match plan_flip(&i.common) {
        // extrusion (0,0,-1): the OCS x is mirrored
        Some(true) => ((-i.pos[0], i.pos[1]), PI - r),
        _ => ((i.pos[0], i.pos[1]), r),
    }
}

/// Model-space INSERTs of existing blocks with numeric placement, and the tables they need.
/// `doc` has decoded names, `unit` is metres per drawing unit, as for the underlay.
pub(super) fn capture(doc: &DxfDocument, unit: f64) -> UBlocks {
    let names: HashSet<String> = doc.blocks.iter().map(|b| upper(&b.name)).collect();
    let mut refs = vec![];
    for e in &doc.entities {
        let Entity::Insert(i) = e else {
            continue;
        };
        let numeric =
            i.pos.iter().chain(&i.scale).all(|v| v.is_finite()) && i.rotation.is_finite();
        if in_paper(e) || !numeric || space(&i.block) || !names.contains(&upper(&i.block)) {
            continue;
        }
        let (base0, rot0) = pose0(i);
        refs.push(URef {
            insert: i.clone(),
            base0,
            rot0,
            pos: (base0.0 * unit, base0.1 * unit),
            rotation: rot0,
        });
    }
    if refs.is_empty() {
        return UBlocks::default();
    }
    let defs = closure(&doc.blocks, refs.iter().map(|r| r.insert.block.as_str()))
        .into_iter()
        .map(|i| doc.blocks[i].clone())
        .collect();
    let src = BlockSrc {
        defs,
        header: doc
            .header
            .iter()
            .filter(|h| h.name.eq_ignore_ascii_case("$LTSCALE"))
            .cloned()
            .collect(),
        layers: doc.layers.clone(),
        ltypes: doc.ltypes.clone(),
        styles: doc.styles.clone(),
    };
    UBlocks {
        src: Arc::new(src),
        refs,
    }
}

fn num(extra: &[Pair], code: i32) -> Option<f64> {
    extra
        .iter()
        .find(|p| p.code == code)
        .and_then(|p| p.value.trim().parse::<f64>().ok())
        .filter(|v| v.is_finite())
}

fn set_num(extra: &mut Vec<Pair>, code: i32, v: f64) {
    match extra.iter_mut().find(|p| p.code == code) {
        Some(p) => p.value = v.to_string(),
        None => extra.push(Pair::new(code, v.to_string())),
    }
}

/// The INSERT where the underlay has taken it, in file coordinates: base point moved, rotation
/// turned (opposite for a mirrored extrusion), ATTRIBs moved and turned alike. Unmoved INSERTs
/// are returned as read. `None`: a non-horizontal INSERT that was moved.
#[cfg(any(test, feature = "dxf-write"))]
fn placed(r: &URef, pl: &Placement) -> Option<Insert> {
    let now = pl.pt2(r.pos);
    let d = r.rotation - pl.rot - r.rot0;
    let tol = 1e-9 * r.base0.0.abs().max(r.base0.1.abs()).max(1.0);
    let moved = (now.0 - r.base0.0).abs() > tol || (now.1 - r.base0.1).abs() > tol;
    let w = d.rem_euclid(TAU);
    let turned = w.min(TAU - w) > 1e-12;
    if !moved && !turned {
        return Some(r.insert.clone());
    }
    if !(now.0.is_finite() && now.1.is_finite() && d.is_finite()) {
        return None;
    }
    let sg = if plan_flip(&r.insert.common)? { -1.0 } else { 1.0 };
    let (s, c) = d.sin_cos();
    let f = |q: P| {
        let (x, y) = (q.0 - r.base0.0, q.1 - r.base0.1);
        (now.0 + c * x - s * y, now.1 + s * x + c * y)
    };
    let deg = d.to_degrees();
    let mut i = r.insert.clone();
    i.pos[0] = sg * now.0;
    i.pos[1] = now.1;
    i.rotation = (i.rotation + sg * deg).rem_euclid(360.0);
    for a in &mut i.attribs {
        // a non-horizontal attribute is not drawn in plan: left as read
        let Some(af) = plan_flip(&a.common) else {
            continue;
        };
        let sa = if af { -1.0 } else { 1.0 };
        let q = f((sa * a.pos[0], a.pos[1]));
        a.pos[0] = sa * q.0;
        a.pos[1] = q.1;
        if let (Some(x), Some(y)) = (num(&a.common.extra, 11), num(&a.common.extra, 21)) {
            let q = f((sa * x, y));
            set_num(&mut a.common.extra, 11, sa * q.0);
            set_num(&mut a.common.extra, 21, q.1);
        }
        let rot = num(&a.common.extra, 50).unwrap_or(0.0);
        set_num(&mut a.common.extra, 50, (rot + sa * deg).rem_euclid(360.0));
    }
    Some(i)
}

/// A document with the tables of the file and the definitions one block needs.
fn mini_doc(src: &BlockSrc, block: &str) -> DxfDocument {
    DxfDocument {
        header: src.header.clone(),
        ltypes: src.ltypes.clone(),
        layers: src.layers.clone(),
        styles: src.styles.clone(),
        blocks: closure(&src.defs, std::iter::once(block))
            .into_iter()
            .map(|i| src.defs[i].clone())
            .collect(),
        ..Default::default()
    }
}

/// INSERTs to write and the underlay primitives they stand for (indices per kind).
#[derive(Debug, Default)]
pub(super) struct Kept {
    pub inserts: Vec<Insert>,
    pub lines: HashSet<usize>,
    pub arcs: HashSet<usize>,
    pub texts: HashSet<usize>,
    pub closed: HashSet<usize>,
    pub fills: HashSet<usize>,
    /// INSERTs with an edited primitive: written as primitives.
    pub exploded: usize,
    /// Moved INSERTs with a non-horizontal extrusion (they have no primitives in the underlay):
    /// their contents drawn flat at the new place, attributes as plain texts. Counted in `exploded`.
    pub flat: Vec<Entity>,
    /// Layers the written definitions and flat entities use (file names).
    pub layers: Vec<String>,
    /// INSERTs that could not be drawn flat either (bad placement, empty result): not written.
    pub skipped: usize,
}

/// Each block reference of the underlay: kept when every primitive of its expansion at its
/// current place is found in the underlay, unused by another reference.
#[cfg(any(test, feature = "dxf-write"))]
pub(super) fn keep(u: &Underlay, pl: &Placement) -> Kept {
    let mut kept = Kept::default();
    if u.blocks.refs.is_empty() {
        return kept;
    }
    let ix = Index::new(u);
    let opt = ExpandOptions::default();
    let mut minis: HashMap<String, DxfDocument> = HashMap::new();
    for r in &u.blocks.refs {
        let Some(ins) = placed(r, pl) else {
            // counted as exploded only when something is written in its place
            let flat = flatten(r, pl, &u.blocks.src, &opt);
            if flat.is_empty() {
                kept.skipped += 1;
            } else {
                kept.exploded += 1;
                kept.flat.extend(flat);
            }
            continue;
        };
        let mini = minis
            .entry(upper(&ins.block))
            .or_insert_with(|| mini_doc(&u.blocks.src, &ins.block));
        mini.entities = vec![Entity::Insert(ins.clone())];
        let (mut e, _) = Underlay::build(mini, pl.k(), &opt);
        e.transform((0.0, 0.0), pl.rot, pl.offset);
        match ix.find(u, &e, &kept) {
            Some(f) => {
                kept.lines.extend(f.lines);
                kept.arcs.extend(f.arcs);
                kept.texts.extend(f.texts);
                kept.closed.extend(f.closed);
                kept.fills.extend(f.fills);
                kept.inserts.push(ins);
            }
            None => kept.exploded += 1,
        }
    }
    kept.layers = used_layers(&u.blocks.src, &kept);
    kept
}

/// A moved INSERT with a non-horizontal extrusion as plain primitives: the extrusion is dropped
/// (the block is drawn flat, as in plan) and the placement applied to the definition.
#[cfg(any(test, feature = "dxf-write"))]
fn flatten(r: &URef, pl: &Placement, src: &BlockSrc, opt: &ExpandOptions) -> Vec<Entity> {
    let mut flat = r.clone();
    flat.insert
        .common
        .extra
        .retain(|p| !(210..=230).contains(&p.code));
    let Some(ins) = placed(&flat, pl) else {
        return vec![];
    };
    let mut mini = mini_doc(src, &ins.block);
    mini.entities = vec![Entity::Insert(ins)];
    expand_inserts(&mini, opt).entities
}

fn layer_of(e: &Entity) -> Option<&str> {
    Some(match e {
        Entity::Line(x) => &x.common.layer,
        Entity::LwPolyline(x) => &x.common.layer,
        Entity::Circle(x) => &x.common.layer,
        Entity::Arc(x) => &x.common.layer,
        Entity::Text(x) => &x.common.layer,
        Entity::MText(x) => &x.common.layer,
        Entity::Insert(x) => &x.common.layer,
        Entity::AttDef(x) => &x.common.layer,
        Entity::Unknown(r) => return r.iter().find(|p| p.code == 8).map(|p| p.value.as_str()),
    })
}

/// Layers named by the definitions of the kept INSERTs (nested ones too) and the flat entities.
#[cfg(any(test, feature = "dxf-write"))]
fn used_layers(src: &BlockSrc, kept: &Kept) -> Vec<String> {
    let mut out: Vec<String> = vec![];
    let mut add = |n: &str| {
        if !out.iter().any(|x| same_name(x, n)) {
            out.push(n.to_string());
        }
    };
    for i in closure(&src.defs, kept.inserts.iter().map(|i| i.block.as_str())) {
        let b = &src.defs[i];
        add(&b.layer);
        b.entities.iter().filter_map(layer_of).for_each(&mut add);
        b.attdefs.iter().for_each(|a| add(&a.common.layer));
    }
    kept.flat.iter().filter_map(layer_of).for_each(&mut add);
    out
}

/// Writes the flat entities of `kept` and adds the layers that definitions and flat entities use
/// but the document lacks, with the source layer's color, weight and line type.
#[cfg(any(test, feature = "dxf-write"))]
pub(super) fn write_flat(
    doc: &mut DxfDocument,
    rep: &mut WriteReport,
    src: &BlockSrc,
    kept: &mut Kept,
) {
    for n in &kept.layers {
        if n.is_empty() || doc.layers.iter().any(|l| same_name(&l.name, n)) {
            continue;
        }
        let Some(l) = src.layers.iter().find(|l| same_name(&l.name, n)) else {
            continue;
        };
        let mut l = l.clone();
        let known = l.linetype.is_empty()
            || l.linetype.eq_ignore_ascii_case("CONTINUOUS")
            || doc.ltypes.iter().any(|t| same_name(&t.name, &l.linetype));
        if !known {
            l.linetype = "Continuous".into();
        }
        doc.layers.push(l);
    }
    for e in std::mem::take(&mut kept.flat) {
        match &e {
            Entity::Line(_) => rep.lines += 1,
            Entity::Arc(_) => rep.arcs += 1,
            Entity::Circle(_) => rep.circles += 1,
            Entity::Text(_) | Entity::MText(_) => rep.texts += 1,
            Entity::LwPolyline(_) => rep.polylines += 1,
            _ => {}
        }
        doc.entities.push(e);
    }
}

/// Grid cell of the primitive index, m; larger than the match tolerance.
const CELL: f64 = 1e-3;

fn tol(q: P) -> f64 {
    1e-6 + 1e-9 * q.0.abs().max(q.1.abs())
}

fn same(a: P, b: P) -> bool {
    let t = tol(a);
    (a.0 - b.0).abs() <= t && (a.1 - b.1).abs() <= t
}

fn same_len(a: f64, b: f64) -> bool {
    (a - b).abs() <= 1e-6 + 1e-9 * a.abs()
}

fn same_ang(a: f64, b: f64) -> bool {
    let d = (a - b).rem_euclid(TAU);
    d.min(TAU - d) <= 1e-8
}

fn same_pen(a: &Pen, b: &Pen) -> bool {
    a.color == b.color
        && (a.weight_mm - b.weight_mm).abs() <= 1e-4
        && a.dash.len() == b.dash.len()
        && a.dash
            .iter()
            .zip(&b.dash)
            .all(|(x, y)| (x - y).abs() <= 1e-5 * x.abs().max(y.abs()).max(1e-6))
}

fn full(a: &UArc) -> bool {
    a.a1 - a.a0 >= TAU - 1e-9
}

fn same_arc(x: &UArc, y: &UArc) -> bool {
    same(x.c, y.c)
        && same_len(x.r, y.r)
        && full(x) == full(y)
        && (full(x) || (same_ang(x.a0, y.a0) && ((x.a1 - x.a0) - (y.a1 - y.a0)).abs() <= 1e-8))
}

/// Circles compare by centre and radius: their rings start where the file's axes were.
fn ring_key(p: &UPoly) -> Option<P> {
    if p.pts.is_empty() {
        return None;
    }
    if p.circle.is_some() {
        let n = p.pts.len() as f64;
        let s = p.pts.iter().fold((0.0, 0.0), |a, q| (a.0 + q.0, a.1 + q.1));
        Some((s.0 / n, s.1 / n))
    } else {
        Some(p.pts[0])
    }
}

fn same_poly(x: &UPoly, y: &UPoly) -> bool {
    match (x.circle, y.circle) {
        (Some(a), Some(b)) => {
            same_len(a, b) && ring_key(x).zip(ring_key(y)).is_some_and(|(p, q)| same(p, q))
        }
        (None, None) => x.pts.len() == y.pts.len() && x.pts.iter().zip(&y.pts).all(|(p, q)| same(*p, *q)),
        _ => false,
    }
}

fn same_fill(x: &UFill, y: &UFill) -> bool {
    x.solid == y.solid
        && x.pattern == y.pattern
        && x.loops.len() == y.loops.len()
        && x.loops.iter().zip(&y.loops).all(|(a, b)| {
            a.len() == b.len() && a.iter().zip(b).all(|(p, q)| same(*p, *q))
        })
}

#[derive(Default)]
struct Grid(HashMap<(i64, i64), Vec<usize>>);

impl Grid {
    fn cell(q: P) -> (i64, i64) {
        ((q.0 / CELL).floor() as i64, (q.1 / CELL).floor() as i64)
    }
    fn add(&mut self, q: P, i: usize) {
        if q.0.is_finite() && q.1.is_finite() {
            let v = self.0.entry(Self::cell(q)).or_default();
            if v.last() != Some(&i) {
                v.push(i);
            }
        }
    }
    fn near(&self, q: P) -> Vec<usize> {
        let mut v = vec![];
        if !(q.0.is_finite() && q.1.is_finite()) {
            return v;
        }
        let (x, y) = Self::cell(q);
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(l) = self.0.get(&(x + dx, y + dy)) {
                    v.extend(l);
                }
            }
        }
        v.sort_unstable();
        v.dedup();
        v
    }
}

#[derive(Default)]
struct Found {
    lines: HashSet<usize>,
    arcs: HashSet<usize>,
    texts: HashSet<usize>,
    closed: HashSet<usize>,
    fills: HashSet<usize>,
}

/// Underlay primitives by grid cell of a key point.
struct Index {
    lines: Grid,
    arcs: Grid,
    texts: Grid,
    closed: Grid,
    fills: Grid,
}

impl Index {
    fn new(u: &Underlay) -> Self {
        let mut ix = Index {
            lines: Grid::default(),
            arcs: Grid::default(),
            texts: Grid::default(),
            closed: Grid::default(),
            fills: Grid::default(),
        };
        for (i, l) in u.lines.iter().enumerate() {
            ix.lines.add(l.a, i);
            ix.lines.add(l.b, i);
        }
        for (i, a) in u.arcs.iter().enumerate() {
            ix.arcs.add(a.c, i);
        }
        for (i, t) in u.texts.iter().enumerate() {
            ix.texts.add(t.pos, i);
        }
        for (i, p) in u.closed.iter().enumerate() {
            if let Some(q) = ring_key(p) {
                ix.closed.add(q, i);
            }
        }
        for (i, f) in u.fills.iter().enumerate() {
            if let Some(q) = f.loops.first().and_then(|l| l.first()) {
                ix.fills.add(*q, i);
            }
        }
        ix
    }

    /// Indices of `u` holding every primitive of `e`, none taken before; `None` if one is missing.
    fn find(&self, u: &Underlay, e: &Underlay, kept: &Kept) -> Option<Found> {
        let mut f = Found::default();
        for x in &e.lines {
            let k = self.lines.near(x.a).into_iter().find(|&k| {
                let y = &u.lines[k];
                !kept.lines.contains(&k)
                    && !f.lines.contains(&k)
                    && x.dashed == y.dashed
                    && same_name(&x.layer, &y.layer)
                    && ((same(x.a, y.a) && same(x.b, y.b)) || (same(x.a, y.b) && same(x.b, y.a)))
                    && same_pen(&e.pen(x.pen), &u.pen(y.pen))
            })?;
            f.lines.insert(k);
        }
        for x in &e.arcs {
            let k = self.arcs.near(x.c).into_iter().find(|&k| {
                let y = &u.arcs[k];
                !kept.arcs.contains(&k)
                    && !f.arcs.contains(&k)
                    && same_name(&x.layer, &y.layer)
                    && same_arc(x, y)
                    && same_pen(&e.pen(x.pen), &u.pen(y.pen))
            })?;
            f.arcs.insert(k);
        }
        for x in &e.texts {
            let k = self.texts.near(x.pos).into_iter().find(|&k| {
                let y = &u.texts[k];
                !kept.texts.contains(&k)
                    && !f.texts.contains(&k)
                    && x.text == y.text
                    && same_name(&x.layer, &y.layer)
                    && same(x.pos, y.pos)
                    && same_len(x.height, y.height)
                    && same_ang(x.rotation, y.rotation)
                    && same_pen(&e.pen(x.pen), &u.pen(y.pen))
            })?;
            f.texts.insert(k);
        }
        for x in &e.closed {
            let q = ring_key(x)?;
            let k = self.closed.near(q).into_iter().find(|&k| {
                let y = &u.closed[k];
                !kept.closed.contains(&k)
                    && !f.closed.contains(&k)
                    && same_name(&x.layer, &y.layer)
                    && same_poly(x, y)
            })?;
            f.closed.insert(k);
        }
        for x in &e.fills {
            let q = *x.loops.first().and_then(|l| l.first())?;
            let k = self.fills.near(q).into_iter().find(|&k| {
                let y = &u.fills[k];
                !kept.fills.contains(&k)
                    && !f.fills.contains(&k)
                    && same_name(&x.layer, &y.layer)
                    && same_fill(x, y)
                    && same_pen(&e.pen(x.pen), &u.pen(y.pen))
            })?;
            f.fills.insert(k);
        }
        Some(f)
    }
}

/// The definition with its own and its nested block names mapped.
fn renamed(b: &Block, map: &HashMap<String, String>) -> Block {
    let to = |n: &str| map.get(&upper(n)).cloned();
    let mut b = b.clone();
    if let Some(n) = to(&b.name) {
        b.name = n;
    }
    for e in &mut b.entities {
        match e {
            Entity::Insert(i) => {
                if let Some(n) = to(&i.block) {
                    i.block = n;
                }
            }
            Entity::Unknown(r) if is_dimension(r) => {
                if let Some(p) = r.iter_mut().skip(1).find(|p| p.code == 2) {
                    if let Some(n) = to(&p.value) {
                        p.value = n;
                    }
                }
            }
            _ => {}
        }
    }
    b
}

fn taken(blocks: &[Block], map: &HashMap<String, String>, n: &str) -> bool {
    let u = upper(n);
    map.contains_key(&u)
        || map.values().any(|v| upper(v) == u)
        || blocks.iter().any(|b| upper(&b.name) == u)
}

/// `NAME_2`, `NAME_3`, ...; anonymous `*U7` gets the next free `*U<k>`.
fn fresh_name(name: &str, taken: impl Fn(&str) -> bool) -> String {
    let stem = match name.strip_prefix('*') {
        Some(rest) => {
            let letters: String = rest.chars().take_while(|c| c.is_ascii_alphabetic()).collect();
            format!("*{letters}")
        }
        None => format!("{name}_"),
    };
    (2u64..)
        .map(|k| format!("{stem}{k}"))
        .find(|n| !taken(n))
        .unwrap_or_else(|| format!("{stem}0"))
}

/// Adds the definitions the INSERTs use (nested ones too) and the file's line types and text
/// styles the document lacks. A name the document already holds with a different definition
/// gets a free one, and the INSERTs and nesting blocks follow it; an equal definition is shared.
/// Returns the number of definitions added.
#[cfg(any(test, feature = "dxf-write"))]
pub(super) fn write_defs(doc: &mut DxfDocument, src: &BlockSrc, inserts: &mut [Insert]) -> usize {
    let need = closure(&src.defs, inserts.iter().map(|i| i.block.as_str()));
    let mut map: HashMap<String, String> = need
        .iter()
        .map(|&i| (upper(&src.defs[i].name), src.defs[i].name.clone()))
        .collect();
    // renaming a nested block changes its parents, so repeat until nothing changes
    loop {
        let mut changed = false;
        for &i in &need {
            let def = &src.defs[i];
            let key = upper(&def.name);
            if map[&key] != def.name {
                continue;
            }
            let differs = doc
                .blocks
                .iter()
                .find(|b| same_name(&b.name, &def.name))
                .is_some_and(|have| *have != renamed(def, &map));
            if differs {
                let fresh = fresh_name(&def.name, |n| taken(&doc.blocks, &map, n));
                map.insert(key, fresh);
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let mut added = 0;
    for &i in &need {
        let def = renamed(&src.defs[i], &map);
        if !doc.blocks.iter().any(|b| same_name(&b.name, &def.name)) {
            doc.blocks.push(def);
            added += 1;
        }
    }
    for i in inserts.iter_mut() {
        if let Some(n) = map.get(&upper(&i.block)) {
            i.block = n.clone();
        }
    }
    // dash lengths in the file are times its $LTSCALE; the document has its own
    let k = DxfDocument {
        header: src.header.clone(),
        ..Default::default()
    }
    .ltscale()
        / doc.ltscale();
    for t in &src.ltypes {
        if !doc.ltypes.iter().any(|x| same_name(&x.name, &t.name)) {
            let mut t = t.clone();
            if k != 1.0 {
                t.pattern.iter_mut().for_each(|v| *v *= k);
                t.pattern_length *= k;
            }
            doc.ltypes.push(t);
        }
    }
    for s in &src.styles {
        if !doc.styles.iter().any(|x| same_name(&x.name, &s.name)) {
            doc.styles.push(s.clone());
        }
    }
    added
}

#[cfg(test)]
mod tests {
    use super::super::write::{append_underlay, to_dxf, WriteReport};
    use super::*;
    use crate::dxf::{self, Arc as DArc, AttDef, Attrib, Circle, Common, Line, Text};

    const UNIT: f64 = 0.001;

    fn on(layer: &str) -> Common {
        Common::on_layer(layer)
    }

    fn line(layer: &str, a: P, b: P) -> Entity {
        Entity::Line(Line {
            common: on(layer),
            start: [a.0, a.1, 0.0],
            end: [b.0, b.1, 0.0],
        })
    }

    fn lay(name: &str, color: i32) -> Layer {
        Layer {
            name: name.into(),
            flags: 0,
            color,
            linetype: "CONTINUOUS".into(),
            extra: vec![],
        }
    }

    fn ins(block: &str, p: P, rot: f64, scale: [f64; 3], layer: &str) -> Insert {
        Insert {
            common: on(layer),
            block: block.into(),
            pos: [p.0, p.1, 0.0],
            scale,
            rotation: rot,
            attribs: vec![],
        }
    }

    fn attrib(tag: &str, value: &str, p: P, flags: i32, extra: Vec<Pair>) -> Attrib {
        let mut common = on("A-TAG");
        common.extra = extra;
        Attrib {
            common,
            tag: tag.into(),
            value: value.into(),
            pos: [p.0, p.1, 0.0],
            height: 50.0,
            flags,
        }
    }

    /// Nested block, plain and mirrored scale, OCS-mirrored extrusion, an array and attributes
    /// (mm). Top-level INSERTs: 5.
    fn sample() -> DxfDocument {
        let mut d = DxfDocument {
            header: vec![HeaderVar {
                name: "$INSUNITS".into(),
                values: vec![Pair::new(70, "4")],
            }],
            layers: vec![lay("0", 7), lay("A-EQPM", 3), lay("A-TAG", 5)],
            ..Default::default()
        };
        let mut inner = Block::new("INNER");
        inner.entities = vec![Entity::Arc(DArc {
            common: on("0"),
            center: [100.0, 0.0, 0.0],
            radius: 50.0,
            start_angle: 10.0,
            end_angle: 200.0,
        })];
        let mut outer = Block::new("OUTER");
        outer.base = [10.0, 5.0, 0.0];
        outer.entities = vec![
            line("0", (0.0, 0.0), (400.0, 0.0)),
            line("A-EQPM", (400.0, 0.0), (400.0, 300.0)),
            Entity::Circle(Circle {
                common: on("0"),
                center: [200.0, 150.0, 0.0],
                radius: 60.0,
            }),
            Entity::Insert(ins("INNER", (50.0, 250.0), 45.0, [1.5, 1.5, 1.0], "0")),
            Entity::Text(Text {
                common: on("0"),
                pos: [20.0, 20.0, 0.0],
                height: 25.0,
                text: "EQ".into(),
                rotation: 0.0,
                style: None,
            }),
        ];
        let mut tag = Block::new("TAG");
        for (k, t) in ["NO", "NAME", "HIDDEN"].iter().enumerate() {
            tag.attdefs.push(AttDef {
                common: on("0"),
                tag: (*t).into(),
                prompt: (*t).into(),
                default: "-".into(),
                pos: [0.0, -100.0 * (k as f64 + 1.0), 0.0],
                height: 50.0,
                flags: i32::from(k == 2),
            });
        }
        tag.entities = vec![
            Entity::Circle(Circle {
                common: on("0"),
                center: [0.0; 3],
                radius: 80.0,
            }),
            line("0", (-80.0, 0.0), (80.0, 0.0)),
        ];
        d.blocks = vec![inner, outer, tag];
        let mut flipped = ins("OUTER", (-3000.0, 1000.0), 20.0, [1.0, 1.0, 1.0], "A-EQPM");
        flipped.common.extra = vec![
            Pair::new(210, "0.0"),
            Pair::new(220, "0.0"),
            Pair::new(230, "-1.0"),
        ];
        let mut array = ins("INNER", (0.0, 6000.0), 0.0, [1.0, 1.0, 1.0], "A-EQPM");
        array.common.extra = vec![
            Pair::new(70, "3"),
            Pair::new(71, "2"),
            Pair::new(44, "400.0"),
            Pair::new(45, "300.0"),
        ];
        let mut tagged = ins("TAG", (2500.0, 4000.0), 10.0, [1.0, 1.0, 1.0], "A-TAG");
        tagged.attribs = vec![
            attrib("NO", "101", (2517.4, 3901.5), 0, vec![]),
            attrib("NAME", "B\u{fc}ro 2", (2400.0, 4100.0), 0, vec![Pair::new(50, "15.0")]),
            attrib("HIDDEN", "x", (2500.0, 3700.0), 1, vec![]),
        ];
        d.entities = vec![
            Entity::Insert(ins("OUTER", (1000.0, 2000.0), 30.0, [2.0, 2.0, 1.0], "A-EQPM")),
            Entity::Insert(ins("OUTER", (5000.0, 2000.0), 0.0, [-1.0, 1.0, 1.0], "A-EQPM")),
            Entity::Insert(flipped),
            Entity::Insert(array),
            Entity::Insert(tagged),
            line("A-EQPM", (0.0, 0.0), (9000.0, 0.0)),
        ];
        d
    }

    const TOP: usize = 5;

    fn back(doc: &DxfDocument) -> DxfDocument {
        dxf::read_str(&dxf::write_string(doc)).expect("written text reads back")
    }

    fn top_inserts(doc: &DxfDocument) -> Vec<&Insert> {
        doc.entities
            .iter()
            .filter_map(|e| match e {
                Entity::Insert(i) => Some(i),
                _ => None,
            })
            .collect()
    }

    fn counts(u: &Underlay) -> [usize; 6] {
        [
            u.lines.len(),
            u.arcs.len(),
            u.texts.len(),
            u.closed.len(),
            u.fills.len(),
            u.inserts.len(),
        ]
    }

    /// Every primitive of `a` has its own match in `b`, and the counts agree.
    fn same_geometry(a: &Underlay, b: &Underlay) {
        assert_eq!(counts(a)[..5], counts(b)[..5], "lines, arcs, texts, closed, fills");
        let mut used = HashSet::new();
        for x in &a.lines {
            let k = (0..b.lines.len()).find(|&k| {
                let y = &b.lines[k];
                !used.contains(&k)
                    && ((same(x.a, y.a) && same(x.b, y.b)) || (same(x.a, y.b) && same(x.b, y.a)))
            });
            used.insert(k.unwrap_or_else(|| panic!("line {:?} -> {:?} lost", x.a, x.b)));
        }
        let mut used = HashSet::new();
        for x in &a.arcs {
            let k = (0..b.arcs.len()).find(|&k| !used.contains(&k) && same_arc(x, &b.arcs[k]));
            used.insert(k.unwrap_or_else(|| panic!("arc at {:?} lost", x.c)));
        }
        let mut used = HashSet::new();
        for x in &a.texts {
            let k = (0..b.texts.len()).find(|&k| {
                let y = &b.texts[k];
                !used.contains(&k)
                    && x.text == y.text
                    && same(x.pos, y.pos)
                    && same_ang(x.rotation, y.rotation)
                    && same_len(x.height, y.height)
            });
            used.insert(k.unwrap_or_else(|| panic!("text {} lost", x.text)));
        }
    }

    fn names(u: &Underlay) -> Vec<(String, Vec<(String, String)>)> {
        let mut v: Vec<_> = u.inserts.iter().map(|i| (i.name.clone(), i.attribs.clone())).collect();
        v.sort();
        v
    }

    /// The project file keeps blocks and styles exactly: the export after a reopen writes the
    /// same INSERTs as before it.
    #[test]
    fn blocks_survive_the_project_record() {
        let u = Underlay::from_dxf(&sample(), UNIT);
        assert_eq!(u.blocks.refs.len(), TOP);
        let back = super::super::blob::decode(&super::super::blob::encode(&u)).unwrap();
        assert_eq!(back, u);
        assert_eq!(
            to_dxf(&back, &Placement::file(UNIT)).0,
            to_dxf(&u, &Placement::file(UNIT)).0
        );
    }

    #[test]
    fn blocks_survive_export() {
        let doc = sample();
        let u1 = Underlay::from_dxf(&doc, UNIT);
        assert_eq!(u1.blocks.refs.len(), TOP);
        let (out, rep) = to_dxf(&u1, &Placement::file(UNIT));
        assert_eq!((rep.blocks, rep.blocks_exploded, rep.skipped), (TOP, 0, 0));
        assert_eq!(rep.block_defs, 3);
        let doc2 = back(&out);
        for n in ["OUTER", "INNER", "TAG"] {
            let defs = doc2.blocks.iter().filter(|b| b.name == n).count();
            assert_eq!(defs, 1, "definition {n} written once");
        }
        let tag = doc2.blocks.iter().find(|b| b.name == "TAG").unwrap();
        assert_eq!(tag.attdefs.len(), 3, "attribute definitions kept");
        assert_eq!(top_inserts(&doc2).len(), TOP);
        assert_eq!(rep.total(), doc2.entities.len());
        // only the plain line is written as a primitive
        assert_eq!(rep.lines, 1);
        let u2 = Underlay::from_dxf(&doc2, UNIT);
        assert_eq!(counts(&u2), counts(&u1), "same block count and primitives");
        assert_eq!(names(&u2), names(&u1));
        same_geometry(&u1, &u2);
        // a second round keeps them again
        let (out3, rep3) = to_dxf(&u2, &Placement::file(UNIT));
        assert_eq!((rep3.blocks, rep3.blocks_exploded), (TOP, 0));
        same_geometry(&u1, &Underlay::from_dxf(&back(&out3), UNIT));
    }

    #[test]
    fn untouched_inserts_written_as_read() {
        let doc = sample();
        let u = Underlay::from_dxf(&doc, UNIT);
        let pl = Placement {
            offset: (120.0, -35.5),
            rot: 0.7,
            unit: UNIT,
        };
        let mut placed = u.clone();
        placed.transform((0.0, 0.0), pl.rot, pl.offset);
        let (out, rep) = to_dxf(&placed, &pl);
        assert_eq!((rep.blocks, rep.blocks_exploded), (TOP, 0));
        let src: Vec<&Insert> = top_inserts(&doc);
        assert_eq!(top_inserts(&out), src, "placement only: INSERTs exactly as in the file");
    }

    #[test]
    fn inserts_keep_transform() {
        let doc = sample();
        let pl = Placement {
            offset: (120.0, -35.5),
            rot: 0.7,
            unit: UNIT,
        };
        let mut u = Underlay::from_dxf(&doc, UNIT);
        u.transform((0.0, 0.0), pl.rot, pl.offset);
        // the user turns and moves the whole drawing in the model
        let turn = 0.25;
        u.transform((123.0, -30.0), turn, (1.5, -2.0));
        let (out, rep) = to_dxf(&u, &pl);
        assert_eq!((rep.blocks, rep.blocks_exploded, rep.skipped), (TOP, 0, 0));
        let doc2 = back(&out);
        let (src, got) = (top_inserts(&doc), top_inserts(&doc2));
        assert_eq!(got.len(), src.len());
        for (a, b) in src.iter().zip(&got) {
            assert_eq!(a.block, b.block);
            assert_eq!(a.scale, b.scale, "{}: scale kept", a.block);
            assert_eq!(a.common.extra, b.common.extra, "{}: extrusion and array kept", a.block);
            let flip = plan_flip(&a.common) == Some(true);
            let t: f64 = if flip { -turn } else { turn };
            let want = a.rotation + t.to_degrees();
            let d = (b.rotation - want).rem_euclid(360.0);
            assert!(d.min(360.0 - d) < 1e-9, "{}: {} vs {want}", a.block, b.rotation);
        }
        // read back and placed, the file gives what the user sees
        let mut u2 = Underlay::from_dxf(&doc2, UNIT);
        u2.transform((0.0, 0.0), pl.rot, pl.offset);
        same_geometry(&u, &u2);
        assert_eq!(u2.inserts.len(), u.inserts.len());
        for (a, b) in u.inserts.iter().zip(&u2.inserts) {
            assert!(same(a.pos, b.pos), "{}: {:?} vs {:?}", a.name, a.pos, b.pos);
            assert!(same_ang(a.rotation, b.rotation), "{}", a.name);
        }
    }

    #[test]
    fn attribs_keep_values() {
        let doc = sample();
        let pl = Placement {
            offset: (10.0, 20.0),
            rot: -0.4,
            unit: UNIT,
        };
        let mut u = Underlay::from_dxf(&doc, UNIT);
        let at_file = names(&u);
        u.transform((0.0, 0.0), pl.rot, pl.offset);
        u.transform((0.0, 0.0), 0.3, (2.0, 1.0));
        let (out, _) = to_dxf(&u, &pl);
        let doc2 = back(&out);
        let src = top_inserts(&doc)[4];
        let got = top_inserts(&doc2).into_iter().find(|i| i.block == "TAG").unwrap();
        let tv = |i: &Insert| -> Vec<(String, String, i32)> {
            i.attribs.iter().map(|a| (a.tag.clone(), a.value.clone(), a.flags)).collect()
        };
        assert_eq!(tv(got), tv(src), "tags, values and invisible flag");
        let rot = num(&got.attribs[1].common.extra, 50).unwrap();
        let d = (rot - 15.0 - 0.3f64.to_degrees()).rem_euclid(360.0);
        assert!(d.min(360.0 - d) < 1e-9, "attribute turned with its block: {rot}");
        // values on re-import; visible ones drawn where the user sees them
        let mut u2 = Underlay::from_dxf(&doc2, UNIT);
        assert_eq!(names(&u2), at_file);
        u2.transform((0.0, 0.0), pl.rot, pl.offset);
        same_geometry(&u, &u2);
        assert!(u2.texts.iter().any(|t| t.text == "B\u{fc}ro 2"));
        assert!(!u2.texts.iter().any(|t| t.text == "x"), "invisible stays invisible");
    }

    #[test]
    fn edited_block_is_exploded() {
        let doc = sample();
        let u1 = Underlay::from_dxf(&doc, UNIT);
        let tag_line = |u: &Underlay| {
            u.lines
                .iter()
                .position(|l| l.layer == "A-TAG" && (l.a.1 - 4.0).abs() < 0.05)
                .unwrap()
        };
        // a deleted primitive: that INSERT goes out as its remaining primitives
        let mut u = u1.clone();
        let k = tag_line(&u);
        u.remove_pick(&super::super::Pick {
            lines: vec![k],
            ..Default::default()
        });
        let (out, rep) = to_dxf(&u, &Placement::file(UNIT));
        assert_eq!((rep.blocks, rep.blocks_exploded), (TOP - 1, 1));
        let u2 = Underlay::from_dxf(&back(&out), UNIT);
        assert_eq!(u2.inserts.len(), u1.inserts.len() - 1);
        same_geometry(&u, &u2);
        assert!(u2.texts.iter().any(|t| t.text == "101"), "attribute kept as text");
        // a moved primitive: likewise, at its new place
        let mut u = u1.clone();
        let k = tag_line(&u);
        u.transform_pick(
            &super::super::Pick {
                lines: vec![k],
                ..Default::default()
            },
            (0.0, 0.0),
            0.0,
            (0.0, 0.5),
            false,
        );
        let (out, rep) = to_dxf(&u, &Placement::file(UNIT));
        assert_eq!((rep.blocks, rep.blocks_exploded), (TOP - 1, 1));
        same_geometry(&u, &Underlay::from_dxf(&back(&out), UNIT));
    }

    #[test]
    fn clashing_block_names_get_free_ones() {
        let a = Underlay::from_dxf(&sample(), UNIT);
        let mut other = sample();
        // same names, other contents: OUTER differs, so does everything nesting it
        let inner = other.blocks.iter_mut().find(|b| b.name == "INNER").unwrap();
        inner.entities.push(line("0", (0.0, 0.0), (0.0, 77.0)));
        let b = Underlay::from_dxf(&other, UNIT);
        let mut doc = DxfDocument::default();
        let mut rep = WriteReport::default();
        rep.add(&append_underlay(&mut doc, &a, &Placement::file(UNIT)));
        let pl_b = Placement {
            offset: (50.0, 0.0),
            rot: 0.0,
            unit: UNIT,
        };
        let mut moved = b.clone();
        moved.translate(pl_b.offset);
        rep.add(&append_underlay(&mut doc, &moved, &pl_b));
        // TAG is equal in both and shared; INNER and OUTER get new names in the second
        assert_eq!(rep.blocks, 2 * TOP);
        assert_eq!(rep.block_defs, 5);
        let names: HashSet<String> = doc.blocks.iter().map(|x| x.name.clone()).collect();
        for n in ["INNER", "OUTER", "TAG", "INNER_2", "OUTER_2"] {
            assert!(names.contains(n), "{n} in {names:?}");
        }
        let outer2 = doc.blocks.iter().find(|x| x.name == "OUTER_2").unwrap();
        assert!(outer2
            .entities
            .iter()
            .any(|e| matches!(e, Entity::Insert(i) if i.block == "INNER_2")));
        // each drawing reads back with its own contents
        let all = Underlay::from_dxf(&back(&doc), UNIT);
        let mut both = a.clone();
        let second = b.clone();
        both.lines.extend(second.lines);
        both.arcs.extend(second.arcs);
        both.texts.extend(second.texts);
        both.closed.extend(second.closed);
        both.fills.extend(second.fills);
        same_geometry(&both, &all);
        // the same drawing appended twice shares its definitions
        let mut doc = DxfDocument::default();
        let r1 = append_underlay(&mut doc, &a, &Placement::file(UNIT));
        let r2 = append_underlay(&mut doc, &a, &Placement::file(UNIT));
        assert_eq!((r1.block_defs, r2.block_defs, doc.blocks.len()), (3, 0, 3));
    }

    #[test]
    fn fresh_names_skip_taken_ones() {
        let t = |n: &str| ["DOOR_2", "*U2", "*U3"].contains(&n);
        assert_eq!(fresh_name("DOOR", t), "DOOR_3");
        assert_eq!(fresh_name("*U7", t), "*U4");
    }
}
