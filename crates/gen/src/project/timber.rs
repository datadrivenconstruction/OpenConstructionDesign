//! Timber structure of a project (`design.structure` timber or timber_frame): a glulam
//! post-and-beam frame joining the columns along both grid directions, sized by span, and the
//! floor cut into cross-laminated panels whose joints fall at the panel width. A bearing-wall
//! timber system gets the panels only. Pure: the project is not changed, the caller places what
//! [`design`] returns; spans and widths come from the `timber.*` rules, sections from
//! `data/timber.json`.

use std::sync::OnceLock;

use serde_json::{json, Value as J};

use super::rules::{self, rule};
use super::{GProject, ProjectSpec};

const DATA: &str = include_str!("../../data/timber.json");
const EPS: f64 = 1e-6;

/// A glulam beam between two columns, under the floor of its storey.
#[derive(Clone, Debug, PartialEq)]
pub struct TBeam {
    pub storey: usize,
    pub a: (f64, f64),
    pub b: (f64, f64),
    /// Axis height above the storey level.
    pub z: f64,
    /// Width and depth.
    pub section: (f64, f64),
    /// Along x; the beams along y are the secondary ones.
    pub primary: bool,
}

impl TBeam {
    pub fn span(&self) -> f64 {
        (self.b.0 - self.a.0).hypot(self.b.1 - self.a.1)
    }
}

/// One cross-laminated floor panel: the plan rectangle x0, y0, x1, y1 it is cut from, its
/// outline (the slab clipped to it) and the stair and shaft holes that fall in it.
#[derive(Clone, Debug, PartialEq)]
pub struct TPanel {
    pub storey: usize,
    pub rect: [f64; 4],
    pub outer: Vec<(f64, f64)>,
    pub holes: Vec<Vec<(f64, f64)>>,
}

#[derive(Clone, Debug, Default, PartialEq)]
pub struct GTimber {
    pub beams: Vec<TBeam>,
    pub panels: Vec<TPanel>,
    /// Beam type names start with this; the section follows.
    pub prefix: String,
    pub material: String,
    /// Column pairs further apart than `timber.beam_span_max`: no beam is built between them.
    pub too_long: usize,
}

impl GTimber {
    pub fn is_empty(&self) -> bool {
        self.beams.is_empty() && self.panels.is_empty()
    }

    /// Name of the beam type of a section, in mm.
    pub fn type_name(&self, section: (f64, f64)) -> String {
        format!("{} {:.0}x{:.0}", self.prefix, section.0 * 1000.0, section.1 * 1000.0)
    }

    /// Index of the first panel of each storey: it takes the storey's slab slot.
    pub fn first_panels(&self) -> Vec<usize> {
        let mut seen = std::collections::BTreeSet::new();
        (0..self.panels.len()).filter(|i| seen.insert(self.panels[*i].storey)).collect()
    }

    pub fn longest_span(&self) -> f64 {
        self.beams.iter().map(TBeam::span).fold(0.0, f64::max)
    }

    pub fn report(&self) -> J {
        json!({
            "beams": self.beams.len(),
            "longest_beam_span_m": (self.longest_span() * 100.0).round() / 100.0,
            "floor_panels": self.panels.len(),
            "spans_over_rule": self.too_long,
            "beam_type": self.prefix,
        })
    }
}

fn sections() -> &'static Vec<(f64, f64)> {
    static S: OnceLock<Vec<(f64, f64)>> = OnceLock::new();
    S.get_or_init(|| {
        let j: J = serde_json::from_str(DATA).expect("timber.json");
        j["beam"]["sections"]
            .as_array()
            .expect("timber.json beam.sections")
            .iter()
            .map(|s| (s[0].as_f64().expect("width"), s[1].as_f64().expect("depth")))
            .collect()
    })
}

fn names() -> (String, String) {
    let j: J = serde_json::from_str(DATA).expect("timber.json");
    (
        j["beam"]["prefix"].as_str().unwrap_or_default().to_string(),
        j["beam"]["material"].as_str().unwrap_or_default().to_string(),
    )
}

/// The shallowest catalogue section whose depth carries `span`, the deepest when none does.
pub fn section_for(span: f64) -> (f64, f64) {
    let need = span / rule("timber.span_depth");
    let all = sections();
    all.iter().copied().find(|s| s.1 + EPS >= need).or_else(|| all.last().copied()).unwrap_or((0.0, 0.0))
}

/// Area of a ring.
pub fn area(ring: &[(f64, f64)]) -> f64 {
    let n = ring.len();
    (0..n).map(|i| ring[i].0 * ring[(i + 1) % n].1 - ring[(i + 1) % n].0 * ring[i].1).sum::<f64>().abs() / 2.0
}

/// The ring cut to the rectangle, one side at a time.
fn clip(ring: &[(f64, f64)], r: [f64; 4]) -> Vec<(f64, f64)> {
    // (axis, bound, keep the side above it)
    let sides = [(0, r[0], true), (0, r[2], false), (1, r[1], true), (1, r[3], false)];
    let mut pts = ring.to_vec();
    for (axis, bound, above) in sides {
        let val = |q: &(f64, f64)| if axis == 0 { q.0 } else { q.1 };
        let keep = |q: &(f64, f64)| if above { val(q) >= bound - EPS } else { val(q) <= bound + EPS };
        let mut next = vec![];
        for (i, a) in pts.iter().enumerate() {
            let b = pts[(i + 1) % pts.len()];
            let cut = (a.0 + (b.0 - a.0) * (bound - val(a)) / (val(&b) - val(a)), a.1 + (b.1 - a.1) * (bound - val(a)) / (val(&b) - val(a)));
            match (keep(a), keep(&b)) {
                (true, true) => next.push(b),
                (true, false) => next.push(cut),
                (false, true) => {
                    next.push(cut);
                    next.push(b);
                }
                (false, false) => {}
            }
        }
        pts = next;
        if pts.len() < 3 {
            return vec![];
        }
    }
    pts
}

/// Whether `q` lies on the edge of the panel rectangle `r`.
fn on_edge(r: [f64; 4], q: &(f64, f64)) -> bool {
    (q.0 - r[0]).abs() < EPS || (q.0 - r[2]).abs() < EPS || (q.1 - r[1]).abs() < EPS || (q.1 - r[3]).abs() < EPS
}

/// A hole that reaches the panel's edge (it crossed a joint) is no hole but a notch in the
/// outline: the outline minus such holes, in as many pieces as it falls into; the holes inside
/// stay holes. Without such a hole, or when the cut cannot be made, the outline is kept.
fn notched(outer: &[(f64, f64)], cuts: Vec<Vec<(f64, f64)>>, r: [f64; 4]) -> Vec<(Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)> {
    if !cuts.iter().any(|h| h.iter().any(|q| on_edge(r, q))) {
        return vec![(outer.to_vec(), cuts)];
    }
    let v = |ring: &[(f64, f64)]| ring.iter().map(|q| ddc_geom::v2(q.0, q.1)).collect::<Vec<_>>();
    let back = |ring: &[ddc_geom::V2]| ring.iter().map(|q| (q.x, q.y)).collect::<Vec<_>>();
    let cut_v: Vec<_> = cuts.iter().map(|h| v(h)).collect();
    match ddc_geom::poly::subtract(&[v(outer)], &cut_v) {
        Some(pieces) if !pieces.is_empty() => pieces
            .iter()
            .map(|(o, hs)| (back(o), hs.iter().map(|h| back(h)).collect()))
            .collect(),
        _ => notched_rects(outer, cuts, r),
    }
}

/// `r` minus the box `b`: up to four rectangles around it.
fn rect_minus(r: [f64; 4], b: [f64; 4]) -> Vec<[f64; 4]> {
    let c = [b[0].max(r[0]), b[1].max(r[1]), b[2].min(r[2]), b[3].min(r[3])];
    if c[0] >= c[2] - EPS || c[1] >= c[3] - EPS {
        return vec![r];
    }
    [[r[0], r[1], c[0], r[3]], [c[2], r[1], r[2], r[3]], [c[0], r[1], c[2], c[1]], [c[0], c[3], c[2], r[3]]]
        .into_iter()
        .filter(|p| p[2] - p[0] > EPS && p[3] - p[1] > EPS)
        .collect()
}

/// The axis-aligned fallback when the polygon difference fails: the panel rectangle minus
/// the bounding boxes of the edge holes, the outline clipped to each piece.
fn notched_rects(outer: &[(f64, f64)], cuts: Vec<Vec<(f64, f64)>>, r: [f64; 4]) -> Vec<(Vec<(f64, f64)>, Vec<Vec<(f64, f64)>>)> {
    let (edge, inner): (Vec<_>, Vec<_>) = cuts.into_iter().partition(|h| h.iter().any(|q| on_edge(r, q)));
    let mut rects = vec![r];
    for h in &edge {
        let b = h.iter().fold([f64::MAX, f64::MAX, f64::MIN, f64::MIN], |a, q| [a[0].min(q.0), a[1].min(q.1), a[2].max(q.0), a[3].max(q.1)]);
        rects = rects.into_iter().flat_map(|p| rect_minus(p, b)).collect();
    }
    rects
        .into_iter()
        .filter_map(|p| {
            let o = clip(outer, p);
            (o.len() >= 3 && area(&o) > EPS).then(|| {
                let hs = inner.iter().map(|h| clip(h, p)).filter(|h| h.len() >= 3 && area(h) > EPS).collect();
                (o, hs)
            })
        })
        .collect()
}

/// The panels of one floor: strips across its longer side, as wide as the rule allows and
/// all alike, so every joint falls at a whole panel width.
fn panels(storey: usize, slab: &[(f64, f64)], holes: &[Vec<(f64, f64)>], out: &mut Vec<TPanel>) {
    if slab.len() < 3 {
        return;
    }
    let (x0, x1) = slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.0), a.1.max(q.0)));
    let (y0, y1) = slab.iter().fold((f64::MAX, f64::MIN), |a, q| (a.0.min(q.1), a.1.max(q.1)));
    let along_x = x1 - x0 >= y1 - y0;
    let (lo, hi) = if along_x { (x0, x1) } else { (y0, y1) };
    let n = (((hi - lo) / rule("timber.panel_w")).ceil() as usize).max(1);
    let step = (hi - lo) / n as f64;
    for i in 0..n {
        let (a, b) = (lo + step * i as f64, lo + step * (i + 1) as f64);
        let rect = if along_x { [a, y0, b, y1] } else { [x0, a, x1, b] };
        let outer = clip(slab, rect);
        if outer.len() >= 3 && area(&outer) > EPS {
            let cuts: Vec<Vec<(f64, f64)>> = holes.iter().map(|h| clip(h, rect)).filter(|h| h.len() >= 3 && area(h) > EPS).collect();
            for (outer, holes) in notched(&outer, cuts, rect) {
                out.push(TPanel { storey, rect, outer, holes });
            }
        }
    }
}

/// Beams from each column to the next one along its row and along its line.
fn beams(storey: usize, height: f64, cols: &[(f64, f64)], out: &mut Vec<TBeam>, too_long: &mut usize) {
    let max = rule("timber.beam_span_max");
    for (i, c) in cols.iter().enumerate() {
        for primary in [true, false] {
            let dist = |q: &(f64, f64)| (q.0 - c.0).hypot(q.1 - c.1);
            let next = cols
                .iter()
                .enumerate()
                .filter(|(j, q)| {
                    *j != i
                        && if primary {
                            (q.1 - c.1).abs() < EPS && q.0 > c.0 + EPS
                        } else {
                            (q.0 - c.0).abs() < EPS && q.1 > c.1 + EPS
                        }
                })
                .min_by(|a, b| dist(a.1).total_cmp(&dist(b.1)));
            if let Some((_, q)) = next {
                if dist(q) > max + EPS {
                    *too_long += 1;
                    continue;
                }
                let section = section_for(dist(q));
                out.push(TBeam { storey, a: *c, b: *q, z: height - section.1 / 2.0, section, primary });
            }
        }
    }
}

/// The timber fit-out of a built project; empty unless its structure is timber.
pub fn design(spec: &ProjectSpec, p: &GProject) -> GTimber {
    if !spec.design.structure.is_timber() {
        return GTimber::default();
    }
    rules::scoped(&spec.rules, || {
        let (prefix, material) = names();
        let mut t = GTimber { prefix, material, ..GTimber::default() };
        for (k, st) in p.storeys.iter().enumerate() {
            if spec.design.structure.is_frame() {
                let cols: Vec<(f64, f64)> = st.layout.columns.iter().map(|c| c.at).collect();
                let mut over = 0;
                beams(k, st.height, &cols, &mut t.beams, &mut over);
                t.too_long += over;
            }
            panels(k, &st.layout.slab, &st.layout.slab_holes, &mut t.panels);
        }
        t
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A hole across a joint is a notch of the outline in each panel, never a hole on its edge;
    /// the panels still add up to the slab minus the hole.
    #[test]
    fn a_hole_across_the_joints_notches_the_panels() {
        let slab = vec![(0.0, 0.0), (12.0, 0.0), (12.0, 6.0), (0.0, 6.0)];
        let hole = vec![(1.0, 2.0), (11.0, 2.0), (11.0, 3.0), (1.0, 3.0)];
        let mut out = vec![];
        panels(0, &slab, &[hole], &mut out);
        assert!(out.len() > 2);
        for p in &out {
            assert!(p.holes.iter().all(|h| h.iter().all(|q| !on_edge(p.rect, q))), "a hole touches the edge of {:?}", p.rect);
        }
        let total: f64 = out.iter().map(|p| area(&p.outer) - p.holes.iter().map(|h| area(h)).sum::<f64>()).sum();
        assert!((total - (72.0 - 10.0)).abs() < 1e-6, "{total}");
    }

    /// A hole inside one panel stays a hole.
    #[test]
    fn a_hole_inside_a_panel_stays_a_hole() {
        let slab = vec![(0.0, 0.0), (12.0, 0.0), (12.0, 6.0), (0.0, 6.0)];
        let hole = vec![(0.2, 2.0), (0.6, 2.0), (0.6, 3.0), (0.2, 3.0)];
        let mut out = vec![];
        panels(0, &slab, &[hole], &mut out);
        assert_eq!(out.iter().map(|p| p.holes.len()).sum::<usize>(), 1);
    }

    /// 100 columns on 40 storeys fit the TIMBER slot block with room to spare.
    #[test]
    fn a_40_storey_frame_of_100_columns_fits_the_slot_block() {
        let cols: Vec<(f64, f64)> = (0..10).flat_map(|i| (0..10).map(move |j| (f64::from(i) * 6.0, f64::from(j) * 6.0))).collect();
        let mut out = vec![];
        let mut over = 0;
        for k in 0..40 {
            beams(k, 3.5, &cols, &mut out, &mut over);
        }
        assert_eq!(over, 0);
        assert!(out.len() == 40 * 180 && out.len() < super::super::slots::TIMBER_MAX as usize, "{} beams", out.len());
    }

    /// The rectangle fallback drops the edge hole's box and keeps the area exact.
    #[test]
    fn the_rect_fallback_cuts_the_edge_hole_out() {
        let r = [0.0, 0.0, 4.0, 6.0];
        let outer = vec![(0.0, 0.0), (4.0, 0.0), (4.0, 6.0), (0.0, 6.0)];
        let edge = vec![(3.0, 2.0), (4.0, 2.0), (4.0, 3.0), (3.0, 3.0)];
        let inner = vec![(0.5, 4.0), (1.0, 4.0), (1.0, 5.0), (0.5, 5.0)];
        let out = notched_rects(&outer, vec![edge, inner], r);
        assert!(out.len() <= 4 && out.len() >= 3);
        let total: f64 = out.iter().map(|(o, hs)| area(o) - hs.iter().map(|h| area(h)).sum::<f64>()).sum();
        assert!((total - (24.0 - 1.0 - 0.5)).abs() < 1e-9, "{total}");
        assert_eq!(out.iter().map(|(_, hs)| hs.len()).sum::<usize>(), 1);
    }
}
