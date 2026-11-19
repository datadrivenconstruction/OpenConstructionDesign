//! Checks of a composed drawing set, for tests and for the reply to the person: every view and
//! schedule inside its sheet frame and clear of the title block and of each other, and no tag
//! text on top of another tag or any other text of a view (dimensions, axes, levels, titles).

use ddc_model::Model;

use super::dset::{mark_box, text_corners, Region};
use super::set::{compose_sheet, Ctx, PlacedKind};
use super::{Mark, SheetError, HATCH, LEVELS, TAGS};

#[derive(Clone, Debug, Default, PartialEq)]
pub struct DsetReport {
    pub sheets: usize,
    pub views: usize,
    pub schedules: usize,
    /// «sheet: what» for a view or schedule outside the frame or on the title block.
    pub outside: Vec<String>,
    /// «sheet: a / b» for two views or schedules on top of each other.
    pub overlaps: Vec<String>,
    /// Pairs of tag texts (or a tag and another text of a view) that overlap.
    pub tag_overlaps: usize,
    /// «sheet: a / b» for the first overlapping tag pairs, to find them.
    pub tag_pairs: Vec<String>,
    /// Text drawn on the sheets, for the page text check.
    pub texts: Vec<String>,
    /// Level mark texts (+3.300 and the level name) on sections and elevations.
    pub level_marks: usize,
    /// Hatch strokes and fills of cut materials.
    pub hatches: usize,
}

fn hit(a: [f64; 4], b: [f64; 4], shrink: f64) -> bool {
    a[0] + shrink < b[2] && b[0] + shrink < a[2] && a[1] + shrink < b[3] && b[1] + shrink < a[3]
}

/// Separating axes of two convex quads: true when they overlap by more than `eps`.
fn quads_hit(a: &[[f64; 2]; 4], b: &[[f64; 2]; 4], eps: f64) -> bool {
    for q in [a, b] {
        for i in 0..4 {
            let (p, r) = (q[i], q[(i + 1) % 4]);
            let n = [-(r[1] - p[1]), r[0] - p[0]];
            let l = n[0].hypot(n[1]).max(1e-12);
            let pr = |s: &[[f64; 2]; 4]| {
                s.iter()
                    .fold((f64::INFINITY, f64::NEG_INFINITY), |(lo, hi), v| {
                        let d = (v[0] * n[0] + v[1] * n[1]) / l;
                        (lo.min(d), hi.max(d))
                    })
            };
            let (a0, a1) = pr(a);
            let (b0, b1) = pr(b);
            if a1 <= b0 + eps || b1 <= a0 + eps {
                return false;
            }
        }
    }
    true
}

/// Composes each sheet of `ids` (numbered in that order) and checks it; see the module note.
pub fn check_set(m: &Model, ids: &[i64], ctx: &Ctx) -> Result<DsetReport, SheetError> {
    let mut rep = DsetReport {
        sheets: ids.len(),
        ..Default::default()
    };
    for (n, &id) in ids.iter().enumerate() {
        let s = m.sheet(id).map_err(|e| SheetError(e.to_string()))?;
        let c = Ctx {
            units: ctx.units,
            schedules: ctx.schedules,
            index: Some((n + 1, ids.len())),
            paper: ctx.paper,
            text: ctx.text,
            refs: ctx.refs,
        };
        let comp = compose_sheet(m, id, &c)?;
        let reg = Region::of(&s.standard, &s.paper)?;
        let mut boxes: Vec<(String, [f64; 4])> = vec![];
        let mut vp = 0;
        for p in &comp.placed {
            match p.kind {
                PlacedKind::Viewport(_) => {
                    let mut bb = [
                        f64::INFINITY,
                        f64::INFINITY,
                        f64::NEG_INFINITY,
                        f64::NEG_INFINITY,
                    ];
                    for (v, mk) in &comp.drawing.marks {
                        if *v == Some(vp) {
                            let b = mark_box(mk);
                            bb = [
                                bb[0].min(b[0]),
                                bb[1].min(b[1]),
                                bb[2].max(b[2]),
                                bb[3].max(b[3]),
                            ];
                        }
                    }
                    vp += 1;
                    rep.views += 1;
                    boxes.push((format!("view {vp}"), bb));
                }
                PlacedKind::Schedule(_) => {
                    rep.schedules += 1;
                    let r = p.rect;
                    boxes.push((
                        format!("schedule {}", rep.schedules),
                        [r[0], r[1], r[0] + r[2], r[1] + r[3]],
                    ));
                }
                _ => {}
            }
        }
        for (what, b) in &boxes {
            let f = reg.frame;
            let st = reg.stamp;
            let inside = b[0] >= f[0] - 0.01
                && b[2] <= f[2] + 0.01
                && b[1] >= f[1] - 0.01
                && b[3] <= f[3] + 0.01;
            let on_stamp = b[2] > st[0] + 0.01 && b[1] < st[1] - 0.01;
            if !inside || on_stamp {
                rep.outside.push(format!("{}: {what}", s.number));
            }
        }
        for i in 0..boxes.len() {
            for j in i + 1..boxes.len() {
                if hit(boxes[i].1, boxes[j].1, 0.01) {
                    rep.overlaps
                        .push(format!("{}: {} / {}", s.number, boxes[i].0, boxes[j].0));
                }
            }
        }
        let mut tags = vec![];
        let mut others = vec![];
        for (v, mk) in &comp.drawing.marks {
            match mk {
                Mark::Line { layer, .. } | Mark::Fill { layer, .. } if *layer == HATCH => {
                    rep.hatches += 1
                }
                Mark::Text { layer, .. } if *layer == LEVELS => rep.level_marks += 1,
                _ => {}
            }
            if let Mark::Text {
                pos,
                h,
                rot,
                s: t,
                layer,
            } = mk
            {
                rep.texts.push(t.clone());
                let q = text_corners(*pos, *h, *rot, t);
                if *layer == TAGS {
                    tags.push((q, t.clone()));
                } else if v.is_some() {
                    others.push((q, t.clone()));
                }
            }
        }
        let mut note = |rep: &mut DsetReport, a: &([[f64; 2]; 4], String), b: &String| {
            rep.tag_overlaps += 1;
            if rep.tag_pairs.len() < 12 {
                let at = a.0[0].map(|v| (v * 10.0).round() / 10.0);
                rep.tag_pairs
                    .push(format!("{}: {} / {b} at {at:?}", s.number, a.1));
            }
        };
        for i in 0..tags.len() {
            for j in i + 1..tags.len() {
                if quads_hit(&tags[i].0, &tags[j].0, 0.05) {
                    note(&mut rep, &tags[i], &tags[j].1);
                }
            }
            for d in &others {
                if quads_hit(&tags[i].0, &d.0, 0.05) {
                    note(&mut rep, &tags[i], &d.1);
                }
            }
        }
    }
    Ok(rep)
}
