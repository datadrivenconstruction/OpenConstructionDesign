//! Underlay -> DXF entities: the drawing as the user edited it, written back in the coordinates
//! and units of its file (inverse placement). Lines, arcs, circles, texts, closed polylines and
//! fills (HATCH) keep their layer and pen. Block INSERTs whose primitives are untouched are
//! written as INSERTs of their definitions (see `blocks`); edited ones as their primitives.

use std::collections::{HashMap, HashSet};
use std::f64::consts::TAU;

use super::blocks::{self, Kept};
use super::source::{aci_rgb, Underlay};
use crate::dxf::{
    self, lower::hatch_record, units_meters, Common, DxfDocument, Entity, HeaderVar, Layer,
    LineType, LwVertex, Pair,
};

type P = (f64, f64);

/// File -> model: model point = R(rot) * (file point * unit) + offset (metres, radians).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Placement {
    pub offset: P,
    pub rot: f64,
    /// Metres per drawing unit of the file.
    pub unit: f64,
}

impl Placement {
    /// No move or turn: only the unit of the file.
    pub fn file(unit: f64) -> Self {
        Placement {
            offset: (0.0, 0.0),
            rot: 0.0,
            unit,
        }
    }

    pub(super) fn k(&self) -> f64 {
        if self.unit.is_finite() && self.unit > 0.0 {
            self.unit
        } else {
            1.0
        }
    }

    /// Model point (m) -> file point (drawing units).
    pub(super) fn pt(&self, q: P) -> [f64; 3] {
        let (x, y) = (q.0 - self.offset.0, q.1 - self.offset.1);
        let (s, c) = self.rot.sin_cos();
        let k = self.k();
        [(c * x + s * y) / k, (-s * x + c * y) / k, 0.0]
    }

    pub(super) fn pt2(&self, q: P) -> P {
        let v = self.pt(q);
        (v[0], v[1])
    }

    pub(super) fn len(&self, v: f64) -> f64 {
        v / self.k()
    }

    /// Model angle (rad) -> file angle (degrees, 0..360).
    pub(super) fn deg(&self, a: f64) -> f64 {
        (a - self.rot).to_degrees().rem_euclid(360.0)
    }
}

/// What was written, per kind.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct WriteReport {
    pub lines: usize,
    pub arcs: usize,
    pub circles: usize,
    pub texts: usize,
    pub polylines: usize,
    pub fills: usize,
    /// Block INSERTs written as INSERTs.
    pub blocks: usize,
    /// Block definitions added to the document.
    pub block_defs: usize,
    /// Block INSERTs with an edited primitive: their contents are written as plain primitives.
    pub blocks_exploded: usize,
    /// Primitives with non-numeric geometry, not written.
    pub skipped: usize,
}

impl WriteReport {
    /// Entities written.
    pub fn total(&self) -> usize {
        self.lines
            + self.arcs
            + self.circles
            + self.texts
            + self.polylines
            + self.fills
            + self.blocks
    }

    pub fn add(&mut self, o: &WriteReport) {
        self.lines += o.lines;
        self.arcs += o.arcs;
        self.circles += o.circles;
        self.texts += o.texts;
        self.polylines += o.polylines;
        self.fills += o.fills;
        self.blocks += o.blocks;
        self.block_defs += o.block_defs;
        self.blocks_exploded += o.blocks_exploded;
        self.skipped += o.skipped;
    }
}

/// A new document holding only the underlay.
pub fn to_dxf(u: &Underlay, pl: &Placement) -> (DxfDocument, WriteReport) {
    let mut doc = DxfDocument::default();
    let rep = append_underlay(&mut doc, u, pl);
    (doc, rep)
}

/// Appends the underlay's layers, line types and entities to `doc` (model space).
pub fn append_underlay(doc: &mut DxfDocument, u: &Underlay, pl: &Placement) -> WriteReport {
    let mut kept = blocks::keep(u, pl);
    let mut w = Writer {
        doc,
        u,
        pl: *pl,
        styles: HashMap::new(),
        rep: WriteReport {
            blocks_exploded: kept.exploded,
            skipped: kept.skipped,
            ..Default::default()
        },
    };
    w.units();
    if !kept.inserts.is_empty() || !kept.flat.is_empty() {
        // before the layers: a layer's line type may come from the file's table
        w.rep.block_defs = blocks::write_defs(w.doc, &u.blocks.src, &mut kept.inserts);
    }
    w.layers();
    blocks::write_flat(w.doc, &mut w.rep, &u.blocks.src, &mut kept);
    for i in std::mem::take(&mut kept.inserts) {
        w.doc.entities.push(Entity::Insert(i));
        w.rep.blocks += 1;
    }
    let (used_lines, used_arcs) = w.closed(&kept);
    w.lines(&used_lines);
    w.arcs(&used_arcs);
    w.texts(&kept);
    w.fills(&kept);
    w.rep
}

const TOL: f64 = 1e-6;

/// Allowed values of code 370 (1/100 mm).
const LINEWEIGHTS: [i32; 24] = [
    0, 5, 9, 13, 15, 18, 20, 25, 30, 35, 40, 50, 53, 60, 70, 80, 90, 100, 106, 120, 140, 158, 200,
    211,
];

fn lineweight(mm: f32) -> i32 {
    let h = f64::from(mm) * 100.0;
    if !h.is_finite() || h < 0.0 {
        return -3;
    }
    *LINEWEIGHTS
        .iter()
        .min_by(|a, b| (f64::from(**a) - h).abs().total_cmp(&(f64::from(**b) - h).abs()))
        .unwrap()
}

fn solid_name(lt: &str) -> bool {
    matches!(
        lt.to_ascii_uppercase().as_str(),
        "" | "CONTINUOUS" | "BYLAYER" | "BYBLOCK"
    )
}

/// Palette index with exactly this colour (7, the foreground, is `None`).
fn aci_of(rgb: [u8; 3]) -> Option<i32> {
    (1..=255).find(|&i| i != 7 && aci_rgb(i) == Some(rgb))
}

/// (code 62, code 420) of a colour; `None` is the foreground colour 7.
fn color_codes(c: Option<[u8; 3]>) -> (i32, Option<i32>) {
    match c {
        None => (7, None),
        Some(rgb) => match aci_of(rgb) {
            Some(i) => (i, None),
            None => (
                7,
                Some(i32::from(rgb[0]) << 16 | i32::from(rgb[1]) << 8 | i32::from(rgb[2])),
            ),
        },
    }
}

fn same_pattern(a: &[f64], b: &[f64]) -> bool {
    a.len() == b.len()
        && a.iter()
            .zip(b)
            .all(|(x, y)| (x - y).abs() <= 1e-6 * x.abs().max(y.abs()).max(1e-9))
}

fn finite(q: P) -> bool {
    q.0.is_finite() && q.1.is_finite()
}

fn close(a: P, b: P) -> bool {
    (a.0 - b.0).abs() <= TOL && (a.1 - b.1).abs() <= TOL
}

/// Primitive end points by grid cell of `TOL`.
#[derive(Default)]
struct Ends(HashMap<(i64, i64), Vec<usize>>);

impl Ends {
    fn cell(q: P) -> (i64, i64) {
        ((q.0 / TOL).round() as i64, (q.1 / TOL).round() as i64)
    }
    fn add(&mut self, q: P, i: usize) {
        if finite(q) {
            self.0.entry(Self::cell(q)).or_default().push(i);
        }
    }
    fn near(&self, q: P) -> Vec<usize> {
        let (x, y) = Self::cell(q);
        let mut v = vec![];
        for dx in -1..=1 {
            for dy in -1..=1 {
                if let Some(l) = self.0.get(&(x + dx, y + dy)) {
                    for &i in l {
                        if !v.contains(&i) {
                            v.push(i);
                        }
                    }
                }
            }
        }
        v
    }
}

fn arc_ends(c: P, r: f64, a0: f64, a1: f64) -> (P, P) {
    (
        (c.0 + r * a0.cos(), c.1 + r * a0.sin()),
        (c.0 + r * a1.cos(), c.1 + r * a1.sin()),
    )
}

fn full_circle(a0: f64, a1: f64) -> bool {
    a1 - a0 >= TAU - 1e-9
}

struct Writer<'a> {
    doc: &'a mut DxfDocument,
    u: &'a Underlay,
    pl: Placement,
    /// (pen, dashed) -> style pairs of an entity (layer left empty).
    styles: HashMap<(u32, bool), Common>,
    rep: WriteReport,
}

impl Writer<'_> {
    /// `$INSUNITS` of the file unit, unless the document already names one.
    fn units(&mut self) {
        if self
            .doc
            .header
            .iter()
            .any(|h| h.name.eq_ignore_ascii_case("$INSUNITS"))
        {
            return;
        }
        let unit = self.pl.k();
        if let Some(code) = (1..=24).find(|&c| {
            units_meters(c).is_some_and(|m| (m - unit).abs() <= 1e-9 * m.max(unit))
        }) {
            self.doc.header.push(HeaderVar {
                name: "$INSUNITS".into(),
                values: vec![Pair::new(70, code.to_string())],
            });
        }
    }

    /// A line type with this pattern (drawing units), made when missing.
    fn ltype(&mut self, pattern: Vec<f64>) -> String {
        if let Some(t) = self
            .doc
            .ltypes
            .iter()
            .find(|t| !solid_name(&t.name) && same_pattern(&t.pattern, &pattern))
        {
            return t.name.clone();
        }
        fn taken(doc: &DxfDocument, n: &str) -> bool {
            doc.ltypes.iter().any(|t| t.name.eq_ignore_ascii_case(n))
        }
        let mut k = self.doc.ltypes.len() + 1;
        while taken(self.doc, &format!("DDC_DASH_{k}")) {
            k += 1;
        }
        let name = format!("DDC_DASH_{k}");
        self.doc.ltypes.push(LineType {
            name: name.clone(),
            description: String::new(),
            flags: 0,
            pattern_length: pattern.iter().map(|v| v.abs()).sum(),
            pattern,
            extra: vec![],
        });
        name
    }

    fn style(&mut self, pen: u32, dashed: bool) -> Common {
        if let Some(c) = self.styles.get(&(pen, dashed)) {
            return c.clone();
        }
        let p = self.u.pen(pen);
        let (color, true_color) = color_codes(p.color);
        let linetype = if !p.dash.is_empty() {
            let k = self.pl.k();
            let pat: Vec<f64> = p.dash.iter().map(|d| f64::from(*d) / k).collect();
            self.ltype(pat)
        } else if dashed {
            // dashed by name, but its pattern has no length: an empty pattern keeps both
            const NAME: &str = "DDC_DASHED";
            if !self.doc.ltypes.iter().any(|t| t.name.eq_ignore_ascii_case(NAME)) {
                self.doc.ltypes.push(LineType {
                    name: NAME.into(),
                    description: String::new(),
                    flags: 0,
                    pattern_length: 0.0,
                    pattern: vec![],
                    extra: vec![],
                });
            }
            NAME.into()
        } else {
            "Continuous".into()
        };
        let c = Common {
            layer: String::new(),
            linetype: Some(linetype),
            color: true_color.is_none().then_some(color),
            true_color,
            lineweight: Some(lineweight(p.weight_mm)),
            extra: vec![],
        };
        self.styles.insert((pen, dashed), c.clone());
        c
    }

    fn common(&mut self, layer: &str, pen: u32, dashed: bool) -> Common {
        let mut c = self.style(pen, dashed);
        c.layer = layer.to_string();
        c
    }

    /// Layer table entries the document does not have yet (names compare without case).
    fn layers(&mut self) {
        let u = self.u;
        for l in &u.layers {
            if self
                .doc
                .layers
                .iter()
                .any(|x| x.name.eq_ignore_ascii_case(&l.name))
            {
                continue;
            }
            let linetype = if solid_name(&l.linetype) {
                if l.linetype.is_empty() {
                    "Continuous".to_string()
                } else {
                    l.linetype.clone()
                }
            } else if self
                .doc
                .ltypes
                .iter()
                .any(|t| t.name.eq_ignore_ascii_case(&l.linetype))
            {
                l.linetype.clone()
            } else {
                // the file's pattern lives in the pens of the layer's own primitives
                match self.layer_dash(&l.name) {
                    Some(pat) => {
                        let name = l.linetype.clone();
                        self.doc.ltypes.push(LineType {
                            name: name.clone(),
                            description: String::new(),
                            flags: 0,
                            pattern_length: pat.iter().map(|v| v.abs()).sum(),
                            pattern: pat,
                            extra: vec![],
                        });
                        name
                    }
                    None => "Continuous".to_string(),
                }
            };
            let (color, true_color) = color_codes(l.color);
            let mut extra = vec![Pair::new(370, lineweight(l.weight_mm).to_string())];
            if let Some(t) = true_color {
                extra.push(Pair::new(420, t.to_string()));
            }
            self.doc.layers.push(Layer {
                name: l.name.clone(),
                flags: i32::from(l.frozen),
                color: if l.off { -color } else { color },
                linetype,
                extra,
            });
        }
    }

    /// Most common dash pattern (drawing units) among the layer's lines and arcs.
    fn layer_dash(&self, layer: &str) -> Option<Vec<f64>> {
        let mut n: HashMap<u32, usize> = HashMap::new();
        let pens = self
            .u
            .lines
            .iter()
            .filter(|l| l.layer == layer)
            .map(|l| l.pen)
            .chain(self.u.arcs.iter().filter(|a| a.layer == layer).map(|a| a.pen));
        for p in pens {
            if !self.u.pen(p).dash.is_empty() {
                *n.entry(p).or_default() += 1;
            }
        }
        let best = n.into_iter().max_by_key(|(p, c)| (*c, u32::MAX - p))?.0;
        let k = self.pl.k();
        Some(self.u.pen(best).dash.iter().map(|d| f64::from(*d) / k).collect())
    }

    /// Closed outlines whose every edge is still one of the underlay's lines or arcs become
    /// closed polylines; those lines and arcs are not written again. Outlines with a moved or
    /// deleted edge are left out: their remaining edges are written on their own. Primitives
    /// of kept block INSERTs are not used.
    fn closed(&mut self, kept: &Kept) -> (HashSet<usize>, HashSet<usize>) {
        let u = self.u;
        let mut le = Ends::default();
        for (i, l) in u.lines.iter().enumerate() {
            le.add(l.a, i);
            le.add(l.b, i);
        }
        let mut ae = Ends::default();
        for (i, a) in u.arcs.iter().enumerate() {
            if !full_circle(a.a0, a.a1) {
                let (s, e) = arc_ends(a.c, a.r, a.a0, a.a1);
                ae.add(s, i);
                ae.add(e, i);
            }
        }
        let (mut used_l, mut used_a) = (kept.lines.clone(), kept.arcs.clone());
        for (pi, poly) in u.closed.iter().enumerate() {
            // circles are written by their arc
            if kept.closed.contains(&pi)
                || poly.circle.is_some()
                || poly.pts.len() < 3
                || !poly.pts.iter().all(|q| finite(*q))
            {
                continue;
            }
            let Some(m) = match_ring(u, &poly.layer, &poly.pts, &le, &ae, &used_l, &used_a) else {
                continue;
            };
            let dashed = m.lines.first().is_some_and(|&i| u.lines[i].dashed);
            let common = self.common(&poly.layer, m.pen, dashed);
            let vertices = m
                .verts
                .iter()
                .map(|(q, b)| {
                    let f = self.pl.pt2(*q);
                    LwVertex {
                        x: f.0,
                        y: f.1,
                        bulge: *b,
                        ..Default::default()
                    }
                })
                .collect();
            self.doc.entities.push(Entity::LwPolyline(dxf::LwPolyline {
                common,
                flags: 1,
                vertices,
            }));
            self.rep.polylines += 1;
            used_l.extend(m.lines);
            used_a.extend(m.arcs);
        }
        (used_l, used_a)
    }

    fn lines(&mut self, used: &HashSet<usize>) {
        let u = self.u;
        for (i, l) in u.lines.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            if !(finite(l.a) && finite(l.b)) {
                self.rep.skipped += 1;
                continue;
            }
            let common = self.common(&l.layer, l.pen, l.dashed);
            self.doc.entities.push(Entity::Line(dxf::Line {
                common,
                start: self.pl.pt(l.a),
                end: self.pl.pt(l.b),
            }));
            self.rep.lines += 1;
        }
    }

    fn arcs(&mut self, used: &HashSet<usize>) {
        let u = self.u;
        for (i, a) in u.arcs.iter().enumerate() {
            if used.contains(&i) {
                continue;
            }
            let ok = finite(a.c) && a.r.is_finite() && a.r > 0.0;
            if !(ok && a.a0.is_finite() && a.a1.is_finite()) {
                self.rep.skipped += 1;
                continue;
            }
            let common = self.common(&a.layer, a.pen, false);
            let center = self.pl.pt(a.c);
            let radius = self.pl.len(a.r);
            if full_circle(a.a0, a.a1) {
                self.doc.entities.push(Entity::Circle(dxf::Circle {
                    common,
                    center,
                    radius,
                }));
                self.rep.circles += 1;
            } else {
                self.doc.entities.push(Entity::Arc(dxf::Arc {
                    common,
                    center,
                    radius,
                    start_angle: self.pl.deg(a.a0),
                    end_angle: self.pl.deg(a.a1),
                }));
                self.rep.arcs += 1;
            }
        }
    }

    /// A text style table entry for `name` when the document has none: the file's own style
    /// if the underlay kept it, else a plain one.
    fn text_style(&mut self, name: &str) {
        if self.doc.styles.iter().any(|s| s.name.eq_ignore_ascii_case(name)) {
            return;
        }
        let st = self
            .u
            .text_styles
            .iter()
            .find(|s| s.name.eq_ignore_ascii_case(name))
            .cloned()
            .unwrap_or_else(|| dxf::TextStyle {
                name: name.to_string(),
                flags: 0,
                height: 0.0,
                width_factor: 1.0,
                oblique: 0.0,
                font: "txt".into(),
                big_font: String::new(),
                extra: vec![],
            });
        self.doc.styles.push(st);
    }

    fn texts(&mut self, kept: &Kept) {
        let u = self.u;
        for (i, t) in u.texts.iter().enumerate() {
            if kept.texts.contains(&i) {
                continue;
            }
            if !(finite(t.pos) && t.height.is_finite() && t.rotation.is_finite()) {
                self.rep.skipped += 1;
                continue;
            }
            let common = self.common(&t.layer, t.pen, false);
            // the value is decoded text: a literal "%%" must not read back as a control code
            let text = if t.text.contains("%%") {
                t.text.replace('%', "%%%")
            } else {
                t.text.clone()
            };
            if let Some(name) = &t.fmt.style {
                self.text_style(name);
            }
            self.doc.entities.push(super::textfmt::entity(t, &self.pl, common, text));
            self.rep.texts += 1;
        }
    }

    fn fills(&mut self, kept: &Kept) {
        let u = self.u;
        for (i, f) in u.fills.iter().enumerate() {
            if kept.fills.contains(&i) {
                continue;
            }
            let loops: Vec<Vec<P>> = f
                .loops
                .iter()
                .filter(|l| l.len() >= 3)
                .map(|l| l.iter().map(|q| self.pl.pt2(*q)).collect())
                .collect();
            if loops.is_empty() || !loops.iter().flatten().all(|q| finite(*q)) {
                self.rep.skipped += 1;
                continue;
            }
            let c = self.common(&f.layer, f.pen, false);
            let pattern = if f.solid { "SOLID" } else { f.pattern.as_str() };
            let mut rec = hatch_record(&c, &loops, pattern);
            // after code 8: line type and weight, which the record leaves out
            let at = rec.iter().position(|p| p.code == 8).map_or(rec.len(), |k| k + 1);
            let mut style = vec![];
            if let Some(lt) = &c.linetype {
                style.push(Pair::new(6, lt.clone()));
            }
            if let Some(w) = c.lineweight {
                style.push(Pair::new(370, w.to_string()));
            }
            for (j, p) in style.into_iter().enumerate() {
                rec.insert(at + j, p);
            }
            self.doc.entities.push(Entity::Unknown(rec));
            self.rep.fills += 1;
        }
    }
}

/// A closed outline found edge by edge among unused lines and arcs of its layer and pen.
struct RingMatch {
    /// Vertices (model m) with the bulge of the edge that starts there.
    verts: Vec<(P, f64)>,
    lines: Vec<usize>,
    arcs: Vec<usize>,
    pen: u32,
}

fn match_ring(
    u: &Underlay,
    layer: &str,
    ring: &[P],
    le: &Ends,
    ae: &Ends,
    used_l: &HashSet<usize>,
    used_a: &HashSet<usize>,
) -> Option<RingMatch> {
    let n = ring.len();
    let mut m = RingMatch {
        verts: vec![],
        lines: vec![],
        arcs: vec![],
        pen: 0,
    };
    let mut pen: Option<u32> = None;
    let mut i = 0;
    while i < n {
        let (p, q) = (ring[i], ring[(i + 1) % n]);
        let line = le.near(p).into_iter().find(|&k| {
            let l = &u.lines[k];
            !used_l.contains(&k)
                && !m.lines.contains(&k)
                && l.layer == layer
                && pen.is_none_or(|x| x == l.pen)
                && ((close(l.a, p) && close(l.b, q)) || (close(l.b, p) && close(l.a, q)))
        });
        if let Some(k) = line {
            pen.get_or_insert(u.lines[k].pen);
            m.lines.push(k);
            m.verts.push((p, 0.0));
            i += 1;
            continue;
        }
        // an arc from p: the ring follows its circle up to the arc's other end
        let mut found = None;
        for k in ae.near(p) {
            let a = &u.arcs[k];
            if used_a.contains(&k)
                || m.arcs.contains(&k)
                || a.layer != layer
                || pen.is_some_and(|x| x != a.pen)
            {
                continue;
            }
            let (s, e) = arc_ends(a.c, a.r, a.a0, a.a1);
            let (fwd, other) = if close(s, p) {
                (true, e)
            } else if close(e, p) {
                (false, s)
            } else {
                continue;
            };
            let tol = TOL.max(a.r * 1e-9);
            let on = |x: P| ((x.0 - a.c.0).hypot(x.1 - a.c.1) - a.r).abs() <= tol;
            let mut j = i + 1;
            let mut ok = false;
            while j <= n {
                let x = ring[j % n];
                if close(x, other) {
                    ok = true;
                    break;
                }
                if !on(x) {
                    break;
                }
                j += 1;
            }
            if ok {
                let b = ((a.a1 - a.a0) / 4.0).tan();
                found = Some((k, j, if fwd { b } else { -b }));
                break;
            }
        }
        let (k, j, b) = found?;
        pen.get_or_insert(u.arcs[k].pen);
        m.arcs.push(k);
        m.verts.push((p, b));
        i = j;
    }
    if i != n {
        return None;
    }
    m.pen = pen.unwrap_or(0);
    Some(m)
}

#[cfg(test)]
mod tests {
    use super::super::fixture;
    use super::*;

    const UNIT: f64 = 0.001;

    fn counts(u: &Underlay) -> [usize; 6] {
        [
            u.lines.len(),
            u.arcs.len(),
            u.texts.len(),
            u.closed.len(),
            u.fills.len(),
            u.layers.len(),
        ]
    }

    fn back(doc: &DxfDocument) -> DxfDocument {
        dxf::read_str(&dxf::write_string(doc)).expect("written text reads back")
    }

    /// Pen of every primitive of one kind as comparable text, sorted.
    fn pens(u: &Underlay) -> Vec<String> {
        let key = |kind: &str, layer: &str, pen: u32, dashed: bool| {
            let p = u.pen(pen);
            let dash: Vec<i64> = p
                .dash
                .iter()
                .map(|d| (f64::from(*d) * 1e6).round() as i64)
                .collect();
            format!(
                "{kind}|{layer}|{:?}|{}|{dash:?}|{dashed}",
                p.color,
                (p.weight_mm * 100.0).round()
            )
        };
        let mut v: Vec<String> = u
            .lines
            .iter()
            .map(|l| key("line", &l.layer, l.pen, l.dashed))
            .chain(u.arcs.iter().map(|a| key("arc", &a.layer, a.pen, false)))
            .chain(u.texts.iter().map(|t| key("text", &t.layer, t.pen, false)))
            .chain(u.fills.iter().map(|f| key("fill", &f.layer, f.pen, f.solid)))
            .collect();
        v.sort();
        v
    }

    #[test]
    fn untouched_underlay_counts_match_import() {
        let doc = fixture::sample_plan(0);
        let u1 = Underlay::from_dxf(&doc, UNIT);
        assert!(!u1.closed.is_empty() && !u1.fills.is_empty() && !u1.inserts.is_empty());
        let (out, rep) = to_dxf(&u1, &Placement::file(UNIT));
        let doc2 = back(&out);
        let u2 = Underlay::from_dxf(&doc2, UNIT);
        assert_eq!(counts(&u2), counts(&u1), "lines, arcs, texts, closed, fills, layers");
        // the file's four doors and windows stay blocks
        assert_eq!((rep.blocks, rep.blocks_exploded), (u1.blocks.refs.len(), 0));
        assert_eq!(rep.blocks, 4);
        assert_eq!(u2.inserts, u1.inserts, "same blocks at the same places");
        assert_eq!(rep.skipped, 0);
        assert!(rep.polylines > 0 && rep.circles > 0 && rep.fills == u1.fills.len());
        assert_eq!(rep.total(), doc2.entities.len());
        assert_eq!(doc2.insunits(), 4, "file unit named: mm");
        // a second pass gives the same file again
        let (out2, rep2) = to_dxf(&u2, &Placement::file(UNIT));
        assert_eq!(rep2.total(), rep.total());
        assert_eq!(counts(&Underlay::from_dxf(&back(&out2), UNIT)), counts(&u1));
    }

    #[test]
    fn export_keeps_layers_and_pens() {
        let doc = fixture::sample_plan(0);
        let mut u1 = Underlay::from_dxf(&doc, UNIT);
        let furn = u1.layers.iter_mut().find(|l| l.name == "A-FURN").unwrap();
        furn.off = true;
        furn.frozen = true;
        // a colour off the palette goes out as true colour
        u1.layers.iter_mut().find(|l| l.name == "A-DOOR").unwrap().color = Some([1, 2, 3]);
        let (out, _) = to_dxf(&u1, &Placement::file(UNIT));
        let u2 = Underlay::from_dxf(&back(&out), UNIT);
        for l in &u1.layers {
            let m = u2
                .layers
                .iter()
                .find(|x| x.name == l.name)
                .unwrap_or_else(|| panic!("layer {} lost", l.name));
            assert_eq!(
                (m.color, m.off, m.frozen, m.weight_mm),
                (l.color, l.off, l.frozen, l.weight_mm),
                "{}",
                l.name
            );
            assert!(
                m.linetype.eq_ignore_ascii_case(&l.linetype),
                "{}: {} vs {}",
                l.name,
                m.linetype,
                l.linetype
            );
        }
        // the doors are kept block INSERTs: their ByLayer contents take the new layer colour
        let mut want = pens(&u1);
        for p in &mut want {
            *p = p.replace("|A-DOOR|Some([0, 255, 0])|", "|A-DOOR|Some([1, 2, 3])|");
        }
        want.sort();
        assert_eq!(pens(&u2), want);
        assert!(u1.lines.iter().any(|l| l.dashed && !u1.pen(l.pen).dash.is_empty()));
    }

    #[test]
    fn placement_inverts_to_file_coordinates() {
        let doc = fixture::sample_plan(0);
        let u1 = Underlay::from_dxf(&doc, UNIT);
        let mut moved = u1.clone();
        let (rot, off) = (0.7, (120.0, -35.5));
        moved.transform((0.0, 0.0), rot, off);
        let pl = Placement { offset: off, rot, unit: UNIT };
        let (a, _) = to_dxf(&u1, &Placement::file(UNIT));
        let (b, _) = to_dxf(&moved, &pl);
        assert_eq!(a.entities.len(), b.entities.len());
        for (x, y) in a.entities.iter().zip(&b.entities) {
            match (x, y) {
                (Entity::Line(x), Entity::Line(y)) => {
                    for k in 0..2 {
                        assert!((x.start[k] - y.start[k]).abs() < 1e-6);
                        assert!((x.end[k] - y.end[k]).abs() < 1e-6);
                    }
                }
                (Entity::Text(x), Entity::Text(y)) => {
                    assert!((x.pos[0] - y.pos[0]).abs() < 1e-6);
                    assert!((x.pos[1] - y.pos[1]).abs() < 1e-6);
                    let d = (x.rotation - y.rotation).rem_euclid(360.0);
                    assert!(d < 1e-6 || d > 360.0 - 1e-6, "{} vs {}", x.rotation, y.rotation);
                }
                (Entity::Arc(x), Entity::Arc(y)) => {
                    assert!((x.center[0] - y.center[0]).abs() < 1e-6);
                    assert!((x.radius - y.radius).abs() < 1e-9);
                    let d = (x.start_angle - y.start_angle).rem_euclid(360.0);
                    assert!(d < 1e-6 || d > 360.0 - 1e-6);
                }
                _ => assert_eq!(std::mem::discriminant(x), std::mem::discriminant(y)),
            }
        }
    }
}
