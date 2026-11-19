// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Seeded random edit sequences on one drawing: offset (either side), join with a tolerance,
//! explode, move to layer, undo and redo, with the DXF export checked after every step. A
//! failure names the seed and the step: replay with `run(seed, steps, Want::ALL)`.

use super::super::super::Origin;
use super::*;
use crate::editor::Editor;
use ddc_io::drawing_model::Pen;
use ddc_io::dxf::{Common, DxfDocument, Entity, LwPolyline, LwVertex};

macro_rules! need {
    ($c:expr, $($m:tt)+) => {
        if !$c {
            return Err(format!($($m)+));
        }
    };
}

/// Ring support and layer checks need ends this close (m).
const RING_TOL: f64 = 1e-6;

struct Rng(u64);

impl Rng {
    fn next(&mut self) -> u64 {
        self.0 = self.0.wrapping_add(0x9E37_79B9_7F4A_7C15);
        let mut z = self.0;
        z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
        z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
        z ^ (z >> 31)
    }

    fn below(&mut self, n: usize) -> usize {
        (self.next() % n as u64) as usize
    }

    /// In [0, 1).
    fn unit(&mut self) -> f64 {
        (self.next() >> 11) as f64 / (1u64 << 53) as f64
    }
}

#[derive(Clone, Copy)]
struct Want {
    /// Per-state invariants: finite, not degenerate, layers and pens kept, rings closed.
    inv: bool,
    /// Every undo and redo lands on the recorded state; all steps undone restore the start.
    undo: bool,
    /// The export writes as many entities as the drawing holds.
    export: bool,
}

impl Want {
    const ALL: Want = Want {
        inv: true,
        undo: true,
        export: true,
    };
}

#[derive(Default, Debug)]
struct Stats {
    offset: usize,
    join: usize,
    explode: usize,
    moved: usize,
    undo: usize,
    redo: usize,
}

impl Stats {
    fn add(&mut self, o: &Stats) {
        self.offset += o.offset;
        self.join += o.join;
        self.explode += o.explode;
        self.moved += o.moved;
        self.undo += o.undo;
        self.redo += o.redo;
    }
}

fn lc(layer: &str) -> Common {
    Common {
        layer: layer.into(),
        ..Default::default()
    }
}

fn dline(layer: &str, a: (f64, f64), b: (f64, f64)) -> Entity {
    Entity::Line(ddc_io::dxf::Line {
        common: lc(layer),
        start: [a.0, a.1, 0.0],
        end: [b.0, b.1, 0.0],
    })
}

fn darc(layer: &str, c: (f64, f64), r: f64, a0: f64, a1: f64) -> Entity {
    Entity::Arc(ddc_io::dxf::Arc {
        common: lc(layer),
        center: [c.0, c.1, 0.0],
        radius: r,
        start_angle: a0,
        end_angle: a1,
    })
}

/// Lines, arcs and a closed ring on four layers, two of them with their own pens.
fn seeded() -> (Editor, Drawings) {
    let v = |x: f64, y: f64, bulge: f64| LwVertex {
        x,
        y,
        bulge,
        ..Default::default()
    };
    let mut doc = DxfDocument::default();
    // R: a closed ring, three straight edges and a half circle on top
    doc.entities.push(Entity::LwPolyline(LwPolyline {
        common: lc("R"),
        flags: 1,
        vertices: vec![v(0.0, 0.0, 0.0), v(4.0, 0.0, 0.0), v(4.0, 2.0, 1.0), v(0.0, 2.0, 0.0)],
    }));
    // A: loose lines and arcs, two lines touching
    doc.entities.push(dline("A", (-6.0, -1.0), (-2.0, -1.0)));
    doc.entities.push(dline("A", (-2.0, -1.0), (-2.0, 3.0)));
    doc.entities.push(dline("A", (-8.0, 4.0), (-5.0, 6.0)));
    doc.entities.push(dline("A", (-6.0, -3.0), (-1.0, -4.0)));
    doc.entities.push(darc("A", (-3.0, 8.0), 2.0, 10.0, 200.0));
    doc.entities.push(darc("A", (6.0, -5.0), 1.5, 0.0, 270.0));
    // B: a triangle of loose lines, not yet a ring
    doc.entities.push(dline("B", (10.0, 0.0), (13.0, 0.0)));
    doc.entities.push(dline("B", (13.0, 0.0), (11.5, 2.5)));
    doc.entities.push(dline("B", (11.5, 2.5), (10.0, 0.0)));
    // C: a half circle closed by its chord
    doc.entities.push(dline("C", (20.0, 0.0), (24.0, 0.0)));
    doc.entities.push(darc("C", (22.0, 0.0), 2.0, 0.0, 180.0));

    let ed = Editor::new_in_memory().unwrap();
    let mut ds = Drawings::default();
    ds.add(&doc, "fuzz", None, ed.level, Some(1.0), Origin::AsIs, v2(0.0, 0.0));
    ds.sel = 0;
    let u = &mut ds.list[0].u;
    let base = u.pens.len() as u32;
    u.pens.push(Pen {
        color: Some([200, 30, 30]),
        weight_mm: 0.35,
        dash: vec![],
    });
    u.pens.push(Pen {
        color: Some([30, 30, 200]),
        weight_mm: 0.5,
        dash: vec![],
    });
    for l in &mut u.lines {
        match l.layer.as_str() {
            "B" => l.pen = base,
            "C" => l.pen = base + 1,
            _ => {}
        }
    }
    for a in &mut u.arcs {
        match a.layer.as_str() {
            "B" => a.pen = base,
            "C" => a.pen = base + 1,
            _ => {}
        }
    }
    (ed, ds)
}

fn gap(a: (f64, f64), b: (f64, f64)) -> f64 {
    (a.0 - b.0).hypot(a.1 - b.1)
}

fn seg_d(p: V2, a: V2, b: V2) -> f64 {
    let d = b - a;
    let l2 = d.x * d.x + d.y * d.y;
    let t = if l2 > 0.0 {
        (((p.x - a.x) * d.x + (p.y - a.y) * d.y) / l2).clamp(0.0, 1.0)
    } else {
        0.0
    };
    (p - v2(a.x + d.x * t, a.y + d.y * t)).len()
}

fn arc_d(p: V2, a: &UArc) -> f64 {
    let c = v2(a.c.0, a.c.1);
    let rel = (angle(c, p) - a.a0).rem_euclid(TAU);
    if rel <= a.a1 - a.a0 {
        ((p - c).len() - a.r).abs()
    } else {
        (p - at(c, a.r, a.a0)).len().min((p - at(c, a.r, a.a1)).len())
    }
}

/// Lines then arcs: (is arc, index, both ends).
fn pieces(u: &Underlay) -> Vec<(bool, usize, [V2; 2])> {
    let mut out = vec![];
    for (i, l) in u.lines.iter().enumerate() {
        out.push((false, i, [v2(l.a.0, l.a.1), v2(l.b.0, l.b.1)]));
    }
    for (i, a) in u.arcs.iter().enumerate() {
        let c = v2(a.c.0, a.c.1);
        out.push((true, i, [at(c, a.r, a.a0), at(c, a.r, a.a1)]));
    }
    out
}

fn to_pick(ps: &[(bool, usize, [V2; 2])], got: &[usize]) -> Pick {
    let mut pk = Pick::default();
    for &j in got {
        let (arc, i, _) = ps[j];
        if arc {
            pk.arcs.push(i);
        } else {
            pk.lines.push(i);
        }
    }
    pk
}

/// A few pieces: half the time everything touching a random one, else up to three at random.
fn make_pick(u: &Underlay, rng: &mut Rng) -> Pick {
    let ps = pieces(u);
    if ps.is_empty() {
        return Pick::default();
    }
    let seed = rng.below(ps.len());
    let mut got = vec![seed];
    if rng.below(100) < 55 {
        let mut k = 0;
        while k < got.len() && got.len() < 8 {
            let e = ps[got[k]].2;
            for (j, p) in ps.iter().enumerate() {
                if got.len() >= 8 {
                    break;
                }
                if !got.contains(&j) && e.iter().any(|a| p.2.iter().any(|b| (*a - *b).len() <= 1e-3)) {
                    got.push(j);
                }
            }
            k += 1;
        }
    } else {
        for _ in 0..rng.below(3) {
            let j = rng.below(ps.len());
            if !got.contains(&j) {
                got.push(j);
            }
        }
    }
    to_pick(&ps, &got)
}

fn mid_of(u: &Underlay, pk: &Pick) -> V2 {
    if let Some(l) = pk.lines.first().and_then(|&i| u.lines.get(i)) {
        return v2((l.a.0 + l.b.0) / 2.0, (l.a.1 + l.b.1) / 2.0);
    }
    if let Some(a) = pk.arcs.first().and_then(|&i| u.arcs.get(i)) {
        return at(v2(a.c.0, a.c.1), a.r, (a.a0 + a.a1) / 2.0);
    }
    v2(0.0, 0.0)
}

/// Any picked piece is an edge of a ring: moving it alone to another layer leaves the ring
/// out of the export by design (see the writer), so the fuzz keeps rings whole.
fn on_ring(u: &Underlay, pk: &Pick) -> bool {
    match curves(u, pk) {
        Ok(ps) => ps.iter().any(|p| {
            let layer = layer_of(u, p.src);
            u.closed
                .iter()
                .any(|c| member(c, layer, &p.geo) || circle_of(c, layer, &p.geo))
        }),
        Err(_) => false,
    }
}

/// Every state: finite, not degenerate, known pens, rings backed by pieces.
fn check_state(u: &Underlay) -> Result<(), String> {
    let pens = u.pens.len().max(1);
    for (i, l) in u.lines.iter().enumerate() {
        let f = [l.a.0, l.a.1, l.b.0, l.b.1].iter().all(|x| x.is_finite());
        need!(f, "line {i} has a non-finite end");
        need!(gap(l.a, l.b) > SAME, "line {i} has zero length");
        need!((l.pen as usize) < pens, "line {i} has pen {} of {pens}", l.pen);
        need!(!l.layer.is_empty(), "line {i} has no layer");
    }
    for (i, a) in u.arcs.iter().enumerate() {
        let f = [a.c.0, a.c.1, a.r, a.a0, a.a1].iter().all(|x| x.is_finite());
        need!(f, "arc {i} has a non-finite value");
        need!(a.r > SAME, "arc {i} has radius {}", a.r);
        need!(a.a1 - a.a0 > SAME, "arc {i} has no sweep");
        need!((a.pen as usize) < pens, "arc {i} has pen {} of {pens}", a.pen);
        need!(!a.layer.is_empty(), "arc {i} has no layer");
    }
    for (k, r) in u.closed.iter().enumerate() {
        need!(
            r.pts.iter().all(|q| q.0.is_finite() && q.1.is_finite()),
            "ring {k} has a non-finite point"
        );
        if let Some(rad) = r.circle {
            need!(rad.is_finite() && rad > SAME, "circle ring {k} has radius {rad}");
            continue;
        }
        need!(r.pts.len() >= 3, "ring {k} has {} points", r.pts.len());
        for (j, q) in r.pts.iter().enumerate() {
            let p = v2(q.0, q.1);
            let mut d = f64::INFINITY;
            for l in &u.lines {
                d = d.min(seg_d(p, v2(l.a.0, l.a.1), v2(l.b.0, l.b.1)));
            }
            for a in &u.arcs {
                d = d.min(arc_d(p, a));
            }
            need!(d <= RING_TOL, "ring {k} point {j} floats {d} m off the pieces: ring not closed");
        }
    }
    Ok(())
}

/// The export holds one entity per loose piece and one polyline per matched ring.
fn export_check(ds: &Drawings) -> Result<(), String> {
    let u = &ds.list[0].u;
    need!(
        u.texts.is_empty() && u.fills.is_empty() && u.inserts.is_empty(),
        "fuzz drawing grew texts, fills or inserts"
    );
    let mut doc = DxfDocument::default();
    let rep = ds.export_into(&mut doc);
    let polys: Vec<&LwPolyline> = doc
        .entities
        .iter()
        .filter_map(|e| match e {
            Entity::LwPolyline(p) => Some(p),
            _ => None,
        })
        .collect();
    let verts: usize = polys.iter().map(|p| p.vertices.len()).sum();
    let expect = (u.lines.len() + u.arcs.len() + polys.len()) as i64 - verts as i64;
    need!(
        doc.entities.len() as i64 == expect,
        "export wrote {} entities, the drawing holds {} ({} lines, {} arcs, {} polylines of {} edges)",
        doc.entities.len(),
        expect,
        u.lines.len(),
        u.arcs.len(),
        polys.len(),
        verts
    );
    need!(
        rep.total() == doc.entities.len(),
        "report says {} entities, document has {}",
        rep.total(),
        doc.entities.len()
    );
    need!(rep.skipped == 0, "export skipped {} primitives", rep.skipped);
    need!(rep.polylines == polys.len(), "report counts {} polylines, document has {}", rep.polylines, polys.len());
    need!(polys.len() <= u.closed.len(), "more polylines than rings");
    Ok(())
}

fn check_offset(pk: &Pick, before: &Underlay, after: &Underlay) -> Result<(), String> {
    let (bl, ba, bc) = (before.lines.len(), before.arcs.len(), before.closed.len());
    need!(
        after.lines.len() >= bl && after.arcs.len() >= ba && after.closed.len() >= bc,
        "offset removed primitives"
    );
    need!(after.lines[..bl] == before.lines[..], "offset changed an existing line");
    need!(after.arcs[..ba] == before.arcs[..], "offset changed an existing arc");
    need!(after.closed[..bc] == before.closed[..], "offset changed an existing ring");
    need!(after.lines.len() + after.arcs.len() > bl + ba, "offset added nothing");
    let src: Vec<(String, u32)> = pk
        .lines
        .iter()
        .filter_map(|&i| before.lines.get(i).map(|l| (l.layer.clone(), l.pen)))
        .chain(
            pk.arcs
                .iter()
                .filter_map(|&i| before.arcs.get(i).map(|a| (a.layer.clone(), a.pen))),
        )
        .collect();
    for l in &after.lines[bl..] {
        need!(
            src.contains(&(l.layer.clone(), l.pen)),
            "offset piece on layer {} pen {} has no picked source",
            l.layer,
            l.pen
        );
    }
    for a in &after.arcs[ba..] {
        need!(
            src.contains(&(a.layer.clone(), a.pen)),
            "offset arc on layer {} pen {} has no picked source",
            a.layer,
            a.pen
        );
    }
    for r in &after.closed[bc..] {
        need!(src.iter().any(|s| s.0 == r.layer), "offset ring on layer {} has no picked source", r.layer);
    }
    Ok(())
}

fn check_join(pk: &Pick, tol: f64, before: &Underlay, after: &Underlay) -> Result<(), String> {
    need!(
        before.lines.len() == after.lines.len() && before.arcs.len() == after.arcs.len(),
        "join changed the piece count"
    );
    let t = tol + 1e-9;
    for (i, (b, a)) in before.lines.iter().zip(&after.lines).enumerate() {
        need!(
            a.layer == b.layer && a.pen == b.pen && a.dashed == b.dashed,
            "join changed layer, pen or dash of line {i}"
        );
        if pk.lines.contains(&i) {
            need!(gap(a.a, b.a) <= t && gap(a.b, b.b) <= t, "join moved line {i} by more than {tol}");
        } else {
            need!(a == b, "join changed the unpicked line {i}");
        }
    }
    for (i, (b, a)) in before.arcs.iter().zip(&after.arcs).enumerate() {
        need!(
            a.layer == b.layer && a.pen == b.pen && a.c == b.c && a.r == b.r,
            "join changed layer, pen or circle of arc {i}"
        );
        if pk.arcs.contains(&i) {
            let c = v2(a.c.0, a.c.1);
            let moved = (at(c, a.r, a.a0) - at(c, b.r, b.a0))
                .len()
                .max((at(c, a.r, a.a1) - at(c, b.r, b.a1)).len());
            need!(moved <= t, "join moved arc {i} ends by {moved}");
        } else {
            need!(a == b, "join changed the unpicked arc {i}");
        }
    }
    let bc = before.closed.len();
    need!(
        after.closed.len() == bc + 1 && after.closed[..bc] == before.closed[..],
        "join must add exactly one ring and keep the others"
    );
    let layer = pk
        .lines
        .first()
        .map(|&i| &before.lines[i].layer)
        .or_else(|| pk.arcs.first().map(|&i| &before.arcs[i].layer));
    need!(
        layer.is_some_and(|l| *l == after.closed[bc].layer),
        "joined ring is on layer {}, picked pieces are on {:?}",
        after.closed[bc].layer,
        layer
    );
    Ok(())
}

fn check_explode(before: &Underlay, after: &Underlay) -> Result<(), String> {
    need!(
        after.lines == before.lines && after.arcs == before.arcs && after.texts == before.texts,
        "explode changed the pieces"
    );
    need!(after.closed.len() < before.closed.len(), "explode removed no ring");
    let mut it = before.closed.iter();
    for c in &after.closed {
        need!(it.any(|b| b == c), "explode changed a ring it kept");
    }
    Ok(())
}

fn check_move(pk: &Pick, name: &str, before: &Underlay, after: &Underlay) -> Result<(), String> {
    need!(
        before.lines.len() == after.lines.len() && before.arcs.len() == after.arcs.len(),
        "move changed the piece count"
    );
    need!(after.closed == before.closed, "move changed a ring");
    for (i, (b, a)) in before.lines.iter().zip(&after.lines).enumerate() {
        need!(
            a.a == b.a && a.b == b.b && a.pen == b.pen && a.dashed == b.dashed,
            "move changed geometry, pen or dash of line {i}"
        );
        if pk.lines.contains(&i) {
            need!(a.layer.eq_ignore_ascii_case(name), "picked line {i} is on {}, not {name}", a.layer);
        } else {
            need!(a.layer == b.layer, "move changed the layer of the unpicked line {i}");
        }
    }
    for (i, (b, a)) in before.arcs.iter().zip(&after.arcs).enumerate() {
        need!(
            a.c == b.c && a.r == b.r && a.a0 == b.a0 && a.a1 == b.a1 && a.pen == b.pen,
            "move changed geometry or pen of arc {i}"
        );
        if pk.arcs.contains(&i) {
            need!(a.layer.eq_ignore_ascii_case(name), "picked arc {i} is on {}, not {name}", a.layer);
        } else {
            need!(a.layer == b.layer, "move changed the layer of the unpicked arc {i}");
        }
    }
    Ok(())
}

enum Kind {
    Offset,
    Join,
    Explode,
    Move,
}

struct Run {
    ed: Editor,
    ds: Drawings,
    rng: Rng,
    /// State after each undo step taken so far, `states[pos]` is the current one.
    states: Vec<Underlay>,
    pos: usize,
    n0: usize,
    want: Want,
    op: &'static str,
    st: Stats,
}

impl Run {
    fn new(seed: u64, want: Want) -> Run {
        let (mut ed, mut ds) = seeded();
        let _ = ds.sync_undo(&mut ed);
        let n0 = ed.undo_len();
        let states = vec![ds.list[0].u.clone()];
        Run {
            ed,
            ds,
            rng: Rng(seed),
            states,
            pos: 0,
            n0,
            want,
            op: "start",
            st: Stats::default(),
        }
    }

    fn sync(&mut self) {
        let _ = self.ds.sync_undo(&mut self.ed);
    }

    fn every(&self) -> Result<(), String> {
        if self.want.inv {
            check_state(&self.ds.list[0].u)?;
        }
        if self.want.export {
            export_check(&self.ds)?;
        }
        Ok(())
    }

    fn history(&mut self, undo: bool) -> Result<(), String> {
        if undo {
            self.op = "undo";
            if self.pos == 0 {
                return Ok(());
            }
            self.ed.undo();
            self.pos -= 1;
            self.st.undo += 1;
        } else {
            self.op = "redo";
            if self.pos + 1 >= self.states.len() {
                return Ok(());
            }
            self.ed.redo();
            self.pos += 1;
            self.st.redo += 1;
        }
        self.sync();
        if self.want.undo {
            need!(
                self.ds.list[0].u == self.states[self.pos],
                "drawing after {} differs from the state recorded at depth {}",
                self.op,
                self.pos
            );
            need!(
                self.ed.undo_len() == self.n0 + self.pos,
                "undo depth {} after {}, expected {}",
                self.ed.undo_len(),
                self.op,
                self.n0 + self.pos
            );
            need!(
                self.ed.can_redo() == (self.pos + 1 < self.states.len()),
                "redo availability wrong after {}",
                self.op
            );
        }
        self.every()
    }

    fn step(&mut self) -> Result<(), String> {
        let roll = self.rng.below(100);
        if roll >= 75 {
            return self.history(roll >= 88);
        }
        let before = self.ds.list[0].u.clone();
        let n = self.ed.undo_len();
        let big = before.lines.len() + before.arcs.len() > 300;
        let kind = match roll {
            0..=24 if !big => Kind::Offset,
            0..=44 => Kind::Join,
            45..=59 => Kind::Explode,
            _ => Kind::Move,
        };
        let pk = make_pick(&before, &mut self.rng);
        self.ds.pick = Some((0, pk.clone()));
        let (mut tol, mut name) = (0.0, String::new());
        let res = match kind {
            Kind::Offset => {
                self.op = "offset";
                let m = mid_of(&before, &pk);
                let side = m + v2(
                    (self.rng.unit() - 0.5) * 3.0,
                    (self.rng.unit() - 0.5) * 3.0,
                );
                let dist = 0.05 + self.rng.unit() * 0.8;
                self.ds.offset_pick(dist, side)
            }
            Kind::Join => {
                self.op = "join";
                tol = [1e-6, 0.01, 0.05][self.rng.below(3)];
                self.ds.join_pick(tol)
            }
            Kind::Explode => {
                self.op = "explode";
                self.ds.explode_pick()
            }
            Kind::Move => {
                self.op = "move";
                if on_ring(&before, &pk) {
                    return Ok(());
                }
                name = ["A", "B", "C", "N1", "N2"][self.rng.below(5)].to_string();
                self.ds.move_pick_to_layer(&name)
            }
        };
        self.sync();
        let after = self.ds.list[0].u.clone();
        match res {
            Err(e) => {
                need!(!e.is_empty(), "refusal without a reason");
                need!(self.ed.undo_len() == n, "refused {} pushed an undo step", self.op);
                need!(after == before, "refused {} changed the drawing: {e}", self.op);
            }
            Ok(m) => {
                need!(!m.is_empty(), "success without a message");
                need!(
                    self.ed.undo_len() == n + 1,
                    "{} made {} undo steps",
                    self.op,
                    self.ed.undo_len() as i64 - n as i64
                );
                need!(after != before, "{} reported done, the drawing is unchanged", self.op);
                match kind {
                    Kind::Offset => {
                        check_offset(&pk, &before, &after)?;
                        self.st.offset += 1;
                    }
                    Kind::Join => {
                        check_join(&pk, tol, &before, &after)?;
                        self.st.join += 1;
                    }
                    Kind::Explode => {
                        check_explode(&before, &after)?;
                        self.st.explode += 1;
                    }
                    Kind::Move => {
                        check_move(&pk, &name, &before, &after)?;
                        self.st.moved += 1;
                    }
                }
                self.states.truncate(self.pos + 1);
                self.states.push(after);
                self.pos += 1;
            }
        }
        self.every()
    }

    /// Undo every step back to the start, then redo them all.
    fn finish(&mut self) -> Result<(), String> {
        self.op = "undo all";
        let last = self.pos;
        while self.pos > 0 {
            self.ed.undo();
            self.sync();
            self.pos -= 1;
            need!(
                self.ds.list[0].u == self.states[self.pos],
                "undo to depth {} does not restore the recorded state",
                self.pos
            );
        }
        need!(
            self.ds.list[0].u == self.states[0],
            "undo of all {last} steps does not restore the drawing"
        );
        need!(self.ed.undo_len() == self.n0, "undo depth {} after undo all, expected {}", self.ed.undo_len(), self.n0);
        self.op = "redo all";
        while self.pos < last {
            self.ed.redo();
            self.sync();
            self.pos += 1;
            need!(
                self.ds.list[0].u == self.states[self.pos],
                "redo to depth {} does not restore the recorded state",
                self.pos
            );
        }
        Ok(())
    }
}

/// One seeded sequence; the error text starts with the seed and the step.
fn run(seed: u64, steps: usize, want: Want) -> Result<Stats, String> {
    let mut r = Run::new(seed, want);
    let fail = |i: usize, op: &str, e: String| format!("modify_fuzz seed {seed} step {i} ({op}): {e}");
    r.every().map_err(|e| fail(0, "start", e))?;
    for i in 1..=steps {
        r.step().map_err(|e| fail(i, r.op, e))?;
    }
    if want.undo {
        r.finish().map_err(|e| fail(steps, r.op, e))?;
    }
    Ok(r.st)
}

fn sweep(seeds: std::ops::Range<u64>, steps: usize, want: Want) -> Stats {
    let mut all = Stats::default();
    for seed in seeds {
        match run(seed, steps, want) {
            Ok(s) => all.add(&s),
            Err(e) => panic!("{e}"),
        }
    }
    all
}

#[test]
fn modify_fuzz_invariants() {
    let want = Want {
        inv: true,
        undo: false,
        export: false,
    };
    let s = sweep(0..12, 200, want);
    // the sequences must reach every tool, or they prove nothing
    assert!(s.offset > 0 && s.join > 0 && s.explode > 0 && s.moved > 0, "{s:?}");
    assert!(s.undo > 0 && s.redo > 0, "{s:?}");
}

#[test]
fn modify_fuzz_undo_all_restores() {
    let want = Want {
        inv: false,
        undo: true,
        export: false,
    };
    let s = sweep(0..12, 200, want);
    assert!(s.undo > 0 && s.redo > 0, "{s:?}");
}

#[test]
fn modify_fuzz_export_counts() {
    let want = Want {
        inv: false,
        undo: false,
        export: true,
    };
    sweep(0..12, 200, want);
}

#[test]
#[ignore = "1000 seeds, slow"]
fn modify_fuzz_1000_seeds() {
    let s = sweep(0..1000, 200, Want::ALL);
    assert!(s.offset > 0 && s.join > 0 && s.explode > 0 && s.moved > 0, "{s:?}");
}
