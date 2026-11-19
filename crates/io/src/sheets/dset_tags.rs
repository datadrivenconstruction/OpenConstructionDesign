// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! Tags of a drawing set's plan clear of every other text and mark on the sheet: dimension
//! figures, room areas and numbers, axis bubbles, level marks, section heads and view titles,
//! each with its real paper frame. The model places tags at 1:100 among walls only; here the
//! plan is drawn at its sheet scale without tags, and a tag on something moves to the
//! nearest free place about its host, with a leader when its head leaves the host.

use ddc_model::{PlanView, SectionSpec, TagCat, TagSpec, ViewPrim};

use super::dset::mark_box;
use super::set::{area_unit, clip_seg, extent, Ctx, PlanMarks};
use super::{data, tag_half, tag_lines, text, weights, Mark, SheetError, CUT, DIMS, GRID, TAGS};

/// Gap a tag keeps from other texts and marks, mm.
const CLEAR: f64 = 0.4;
/// Rings of places tried about a host, and places on a ring.
const RINGS: usize = 10;
const PER_RING: usize = 24;
/// A closed line this small (mm) is a bubble, a head or an arrow, not drawing geometry.
const SMALL: f64 = 30.0;

type Box4 = [f64; 4];

fn hits(a: Box4, b: Box4) -> bool {
    a[0] < b[2] && b[0] < a[2] && a[1] < b[3] && b[1] < a[3]
}

fn grown(b: Box4, g: f64) -> Box4 {
    [b[0] - g, b[1] - g, b[2] + g, b[3] + g]
}

fn crosses(seg: &([f64; 2], [f64; 2]), b: Box4) -> bool {
    clip_seg(seg.0, seg.1, [b[0], b[1], b[2] - b[0], b[3] - b[1]]).is_some()
}

/// What a tag keeps clear of, in paper mm.
struct Sheet {
    /// Texts, bubbles, heads and arrows.
    boxes: Vec<Box4>,
    /// Dimension lines and the edges of cut walls.
    lines: Vec<([f64; 2], [f64; 2])>,
    /// Edges of cut walls only: a room tag never crosses into another room.
    walls: Vec<([f64; 2], [f64; 2])>,
    /// The drawing's extent: a tag stays inside it, so the plan keeps its footprint.
    bound: Box4,
}

impl Sheet {
    fn free(&self, b: Box4, placed: &[Box4]) -> bool {
        let o = self.bound;
        let inside = b[0] >= o[0] && b[1] >= o[1] && b[2] <= o[2] && b[3] <= o[3];
        let g = grown(b, CLEAR);
        inside
            && !self.boxes.iter().any(|o| hits(g, *o))
            && !placed.iter().any(|o| hits(g, *o))
            && !self.lines.iter().any(|s| crosses(s, b))
    }
}

fn sheet_of(marks: &[(Option<usize>, Mark)]) -> Sheet {
    let mut sh = Sheet {
        boxes: vec![],
        lines: vec![],
        walls: vec![],
        bound: [
            f64::INFINITY,
            f64::INFINITY,
            f64::NEG_INFINITY,
            f64::NEG_INFINITY,
        ],
    };
    for (vp, m) in marks {
        if *vp != Some(0) {
            continue;
        }
        let b = mark_box(m);
        sh.bound = [
            sh.bound[0].min(b[0]),
            sh.bound[1].min(b[1]),
            sh.bound[2].max(b[2]),
            sh.bound[3].max(b[3]),
        ];
        match m {
            Mark::Text { .. } => sh.boxes.push(b),
            Mark::Line {
                pts, closed, layer, ..
            } => {
                let small = b[2] - b[0] < SMALL && b[3] - b[1] < SMALL;
                if (*layer == GRID || *layer == TAGS) && *closed && small {
                    sh.boxes.push(b);
                }
                let n = pts.len();
                if (*layer == DIMS || *layer == CUT) && n >= 2 {
                    let segs = if *closed && n >= 3 { n } else { n - 1 };
                    for i in 0..segs {
                        let s = (pts[i], pts[(i + 1) % n]);
                        sh.lines.push(s);
                        if *layer == CUT {
                            sh.walls.push(s);
                        }
                    }
                }
            }
            Mark::Fill { .. } => {}
        }
    }
    sh
}

/// Tags of plan `d` (as drawn at 1:`scale` beside `views`' marks, its title and `marks`) that
/// sit on another text or mark, moved clear: their new head offsets and leaders, by host.
/// A tag with no free place about its host stays where it is.
#[allow(clippy::too_many_arguments)]
pub(super) fn settle_tags(
    d: &PlanView,
    scale: f64,
    standard: &str,
    ctx: &Ctx,
    title: &str,
    views: &[SectionSpec],
    marks: &PlanMarks,
) -> Result<Vec<TagSpec>, SheetError> {
    let mut bare = d.clone();
    bare.bbox = extent(d);
    let mut tags: Vec<&ddc_model::Tag> = d
        .prims
        .iter()
        .filter_map(|p| match p {
            ViewPrim::Tag { tag } => Some(tag),
            _ => None,
        })
        .collect();
    bare.prims.retain(|p| !matches!(p, ViewPrim::Tag { .. }));
    if tags.is_empty() {
        return Ok(vec![]);
    }
    // rooms first: their tags hold the middle of the room, doors and windows fit about them
    tags.sort_by_key(|t| (t.cat != TagCat::Room, t.id));
    let (dr, _) = super::dset::lone_view(&bare, scale, standard, ctx, title, views, marks)?;
    let sh = sheet_of(&dr.marks);
    let (sc, mc, s) = dr.transforms[0];
    let tf = |p: (f64, f64)| [sc[0] + (p.0 - mc[0]) / s, sc[1] + (p.1 - mc[1]) / s];
    let th = weights(standard, scale)?.tag;
    let st = &data()["standards"][standard];
    let (unit, sep) = (area_unit(ctx), text(st, "decimal"));
    let mut placed: Vec<Box4> = vec![];
    let mut out = vec![];
    for t in tags {
        let lines = tag_lines(t, &unit, &sep);
        let (hw, hh) = tag_half(&lines, th);
        let wing = if t.cat == TagCat::Window {
            hh * 0.6
        } else {
            0.0
        };
        let at_c = |c: [f64; 2]| [c[0] - hw - wing, c[1] - hh, c[0] + hw + wing, c[1] + hh];
        let (head, at) = (tf(t.head), tf(t.anchor));
        if sh.free(at_c(head), &placed) {
            placed.push(at_c(head));
            continue;
        }
        let step = th * 1.2;
        let mut best = None;
        'rings: for r in 1..=RINGS {
            for k in 0..PER_RING {
                let a = std::f64::consts::TAU * k as f64 / PER_RING as f64;
                let c = [
                    at[0] + r as f64 * step * a.cos(),
                    at[1] + r as f64 * step * a.sin(),
                ];
                let b = at_c(c);
                if !sh.free(b, &placed) {
                    continue;
                }
                // a room's tag stays in its room: no wall between the room point and the head
                if t.cat == TagCat::Room && sh.walls.iter().any(|w| seg_cross(*w, (at, c))) {
                    continue;
                }
                best = Some(c);
                break 'rings;
            }
        }
        let Some(c) = best else {
            placed.push(at_c(head));
            continue;
        };
        let b = at_c(c);
        placed.push(b);
        let off = [(c[0] - at[0]) * s, (c[1] - at[1]) * s];
        let leader = !(at[0] >= b[0] && at[0] <= b[2] && at[1] >= b[1] && at[1] <= b[3]);
        out.push(TagSpec {
            level: t.spec.level,
            host: t.spec.host,
            offset: (off[0], off[1]),
            leader,
        });
    }
    Ok(out)
}

/// Two segments cross (touching ends do not count).
fn seg_cross(p: ([f64; 2], [f64; 2]), q: ([f64; 2], [f64; 2])) -> bool {
    let o = |a: [f64; 2], b: [f64; 2], c: [f64; 2]| {
        (b[0] - a[0]) * (c[1] - a[1]) - (b[1] - a[1]) * (c[0] - a[0])
    };
    let (d1, d2) = (o(q.0, q.1, p.0), o(q.0, q.1, p.1));
    let (d3, d4) = (o(p.0, p.1, q.0), o(p.0, p.1, q.1));
    d1 * d2 < 0.0 && d3 * d4 < 0.0
}
