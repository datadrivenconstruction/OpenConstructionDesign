// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View annotations on a sheet: the same strokes as on screen, in sheet mm (PDF and DXF come from the same marks).

use ddc_model::{Anno, AnnoCtx, AnnoStroke, AnnoView, Model, PlanView, ViewPrim};

use super::{dashed, text_width, Pen, DETAIL, NOTES};

/// Notation of axis numbers and letters in labels: from the sheet standard, one source for screen and sheet.
#[derive(Clone, Debug, PartialEq)]
pub struct AnnoFmt {
    pub sep: String,
    pub north: String,
    pub east: String,
}

/// `words` — the document language's catalog (None: the standard's own letters); the decimal
/// mark stays the standard's drafting convention.
pub fn anno_fmt(standard: &str, words: Option<&dyn Fn(&str) -> String>) -> AnnoFmt {
    let d = super::data();
    let st = &d["standards"][standard];
    let get = |k: &str, or: &str| st[k].as_str().unwrap_or(or).to_string();
    AnnoFmt {
        sep: get("decimal", "."),
        north: super::std_word(words, standard, "north", &get("spot_north", "N")),
        east: super::std_word(words, standard, "east", &get("spot_east", "E")),
    }
}

/// The view is not on a sheet yet — ISO, as for a new sheet.
pub fn view_anno_fmt(m: &Model, view: AnnoView, words: Option<&dyn Fn(&str) -> String>) -> AnnoFmt {
    let st = m.anno_view_standard(view).ok().flatten();
    anno_fmt(st.as_deref().unwrap_or("iso"), words)
}

/// `tf` — from view coordinates to sheet mm, `scale` — viewport scale denominator.
pub(super) fn draw_anno(
    pen: &mut Pen<'_>,
    tf: &dyn Fn((f64, f64)) -> [f64; 2],
    item: &Anno,
    scale: f64,
    fmt: &AnnoFmt,
) {
    let measure = |s: &str, h: f64| text_width(s, h);
    let ctx = AnnoCtx {
        scale,
        sep: &fmt.sep,
        north: &fmt.north,
        east: &fmt.east,
        measure: &measure,
    };
    for st in item.strokes(&ctx) {
        match st {
            AnnoStroke::Line {
                pts,
                closed,
                weight_mm,
                dash_mm,
            } => {
                let mut r: Vec<[f64; 2]> = pts.iter().map(|&p| tf(p)).collect();
                if dash_mm.is_empty() {
                    pen.line(r, closed, weight_mm, DETAIL);
                    continue;
                }
                if closed {
                    if let Some(&first) = r.first() {
                        r.push(first);
                    }
                }
                for w in r.windows(2) {
                    for [a, b] in dashed(w[0], w[1], &dash_mm) {
                        pen.line(vec![a, b], false, weight_mm, DETAIL);
                    }
                }
            }
            AnnoStroke::Fill { rings, gray, .. } => {
                let rings = rings
                    .iter()
                    .map(|r| r.iter().map(|&p| tf(p)).collect())
                    .collect();
                pen.fill(rings, gray, DETAIL);
            }
            AnnoStroke::Text {
                pos,
                h_mm,
                rot_deg,
                s,
            } => pen.text(tf(pos), h_mm, rot_deg, &s, NOTES),
        }
    }
}

/// Legend on a sheet: its coordinate origin is at `pos`, the view is placed without a viewport.
pub(super) fn draw_view(
    pen: &mut Pen<'_>,
    view: &PlanView,
    pos: (f64, f64),
    scale: f64,
    fmt: &AnnoFmt,
) {
    let tf = |p: (f64, f64)| [pos.0 + p.0 * 1000.0 / scale, pos.1 + p.1 * 1000.0 / scale];
    for p in &view.prims {
        if let ViewPrim::Anno { item } = p {
            draw_anno(pen, &tf, item, scale, fmt);
        }
    }
}
