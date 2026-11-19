// SPDX-License-Identifier: AGPL-3.0-only
// SPDX-FileCopyrightText: 2026 Artem Boiko / DataDrivenConstruction.io and OpenConstructionDesign Contributors
//! View graphics on a sheet: line and fill overrides, thin lines, detail level and material
//! hatching. The same `ViewPaint` result as on screen; paper does not use the material colour.

use std::collections::HashMap;
use std::sync::Arc;

use ddc_model::drawq::{halftone, Band, DrawLook, HALFTONE_K};
use ddc_model::{
    is_lib_hatch, material_default_hatch, prim_ref, DetailLevel, ElemPaint, Error, Model, ViewKey,
    ViewPaint, ViewPrim, ViewRef, LIB_HATCH_PREFIX,
};

use super::{dashed, hatch_rings, Mark, Pen, Weights, HATCH};

/// An override weight is given in screen px (96 dpi): that many mm on paper.
const PX_MM: f64 = 0.25;
/// Spacing and dash of the "dots" pattern on paper, mm.
const DOTS_STEP: f64 = 2.5;
const DOTS_DASH: [f64; 2] = [0.2, 2.3];

/// Pixels per mm for judging whether a line pattern is readable on paper.
const PATTERN_PX_PER_MM: f64 = 4.0;

/// Graphics of one view for a sheet.
#[derive(Clone, Debug)]
pub struct ViewStyle {
    paint: Arc<ViewPaint>,
    hatches: HashMap<String, String>,
    /// Pens, libraries and cut/projection styles of the view (drawing quality window).
    look: DrawLook,
    /// Scale denominator of the viewport: picks the pen column and sizes library hatches.
    denom: f64,
}

/// Category and band of a primitive for the cut and projection styles: cut rings and section cuts
/// are cut lines, everything else is seen. None for annotations, which no style touches.
fn prim_class(p: &ViewPrim) -> Option<(&'static str, Band)> {
    Some(match p {
        ViewPrim::Cut { .. } => ("wall", Band::Cut),
        ViewPrim::DoorLeaf { .. } | ViewPrim::DoorSwing { .. } => ("door", Band::Seen),
        ViewPrim::WindowLine { .. } => ("window", Band::Seen),
        ViewPrim::CurtainLine { .. } => ("curtain_wall", Band::Seen),
        ViewPrim::SectionCut { table, .. } => (category_of(table)?, Band::Cut),
        ViewPrim::Edge { table, .. } => (category_of(table)?, Band::Seen),
        _ => return None,
    })
}

fn category_of(table: &str) -> Option<&'static str> {
    ddc_model::GFX_CATEGORIES
        .iter()
        .map(|(k, _)| *k)
        .find(|k| *k == table)
}

fn rgb_of(c: u32) -> [u8; 3] {
    [(c >> 16) as u8, (c >> 8) as u8, c as u8]
}

/// Paper is white and has no transparency layers: transparency is a blend with white.
fn over_paper(c: u32, transparency: Option<u8>) -> [u8; 3] {
    let a = 1.0 - f64::from(transparency.unwrap_or(0).min(100)) / 100.0;
    rgb_of(c).map(|x| (f64::from(x) * a + 255.0 * (1.0 - a)).round() as u8)
}

impl ViewStyle {
    pub fn of(m: &Model, v: &ViewRef) -> Result<ViewStyle, Error> {
        let key = match *v {
            ViewRef::Plan { level, .. } => ViewKey::Plan(level),
            ViewRef::View(id) => ViewKey::Section(id),
        };
        let hatches = m
            .material_rows()?
            .into_iter()
            .map(|x| (x.spec.name, x.spec.hatch))
            .collect();
        Ok(ViewStyle {
            paint: m.view_paint(key)?,
            hatches,
            look: m.drawq_look(key)?,
            denom: 100.0,
        })
    }

    /// The viewport scale 1:`denom` the view is drawn at.
    pub fn with_scale(mut self, denom: f64) -> ViewStyle {
        if denom.is_finite() && denom > 0.0 {
            self.denom = denom;
        }
        self
    }

    fn denom_u32(&self) -> u32 {
        self.denom.round().clamp(1.0, f64::from(u32::MAX)) as u32
    }

    pub(super) fn paint_of(&self, p: &ViewPrim) -> Option<ElemPaint> {
        let (t, id) = prim_ref(p)?;
        self.paint.elem(t, id)
    }

    /// Section hatching with the material pattern; absent at coarse detail, as on screen.
    pub(super) fn hatch(
        &self,
        pen: &mut Pen<'_>,
        rs: &[Vec<[f64; 2]>],
        material: &str,
        weight: f64,
    ) {
        if self.paint.detail == DetailLevel::Coarse {
            return;
        }
        let key = self
            .hatches
            .get(material)
            .map_or_else(|| material_default_hatch(material), String::as_str);
        if is_lib_hatch(key) {
            self.lib_hatch(pen, rs, &key[LIB_HATCH_PREFIX.len()..], weight);
            return;
        }
        let lines: &[(f64, f64)] = match key {
            // sheet with y up: the same directions as on screen
            "diagonal" => &[(135.0, 1.5)],
            "back_diagonal" => &[(45.0, 2.1)],
            "cross" => &[(45.0, 2.5), (135.0, 2.5)],
            "dots" => {
                for s in hatch_rings(rs, 0.0, DOTS_STEP) {
                    for d in dashed(s[0], s[1], &DOTS_DASH) {
                        pen.line(d.to_vec(), false, weight, HATCH);
                    }
                }
                return;
            }
            _ => return,
        };
        for (deg, step) in lines {
            for s in hatch_rings(rs, *deg, *step) {
                pen.line(s.to_vec(), false, weight, HATCH);
            }
        }
    }

    /// Library hatch: the pattern works in model metres, the rings are paper mm.
    fn lib_hatch(&self, pen: &mut Pen<'_>, rs: &[Vec<[f64; 2]>], name: &str, weight: f64) {
        let Some(p) = self.look.hatches.get(name) else {
            return;
        };
        let k = self.denom / 1000.0;
        let rings: Vec<Vec<(f64, f64)>> = rs
            .iter()
            .map(|r| r.iter().map(|q| (q[0] * k, q[1] * k)).collect())
            .collect();
        // too dense or invalid: nothing, as on screen where the region stays plain
        let Ok(segs) = p.strokes(&rings, self.denom) else {
            return;
        };
        for (a, b) in segs {
            pen.line(vec![[a.0 / k, a.1 / k], [b.0 / k, b.1 / k]], false, weight, HATCH);
        }
    }

    /// Pen weight, pattern, colour and halftone of the view's cut and projection style for the
    /// marks of one primitive. Only categories with an override of their own are touched.
    fn apply_look(
        &self,
        marks: &mut Vec<(Option<usize>, Mark)>,
        first: usize,
        p: &ViewPrim,
        over_line: bool,
        thin: bool,
        wt: &Weights,
    ) {
        let Some((cat, band)) = prim_class(p) else {
            return;
        };
        if !self.look.styles.contains_key(cat) {
            return;
        }
        let rl = self.look.line(cat, band, self.denom_u32());
        let pat = self.look.patterns.get_or_solid(&rl.pattern);
        let own = rl.color.map(rgb_of);
        let ink = |rgb: Option<[u8; 3]>| {
            if !rl.halftone {
                return rgb;
            }
            let c = rgb.map_or(0, |c| {
                (u32::from(c[0]) << 16) | (u32::from(c[1]) << 8) | u32::from(c[2])
            });
            Some(rgb_of(halftone(c, 0xFFFFFF, HALFTONE_K)))
        };
        let tail: Vec<(Option<usize>, Mark)> = marks.drain(first..).collect();
        for (vp, m) in tail {
            match m {
                Mark::Line {
                    pts,
                    closed,
                    weight,
                    rgb,
                    layer,
                } => {
                    let hatch = layer == HATCH;
                    let rgb = ink(if over_line { rgb } else { own.or(rgb) });
                    let mut w = if hatch { weight } else { f64::from(rl.mm) };
                    if thin {
                        w = w.min(wt.thin);
                    }
                    if hatch || pat.solid() || !pat.readable(PATTERN_PX_PER_MM) {
                        marks.push((vp, Mark::Line { pts, closed, weight: w, rgb, layer }));
                        continue;
                    }
                    let mut path: Vec<(f64, f64)> = pts.iter().map(|q| (q[0], q[1])).collect();
                    if closed {
                        if let Some(&f) = path.first() {
                            path.push(f);
                        }
                    }
                    for piece in pat.stroke(&path, PATTERN_PX_PER_MM) {
                        let pts = piece.iter().map(|q| [q.0, q.1]).collect();
                        marks.push((
                            vp,
                            Mark::Line { pts, closed: false, weight: w, rgb, layer },
                        ));
                    }
                }
                Mark::Fill {
                    rings,
                    gray,
                    rgb,
                    layer,
                } if rl.halftone => {
                    let rgb = ink(rgb.or(Some([gray; 3])));
                    marks.push((vp, Mark::Fill { rings, gray, rgb, layer }));
                }
                other => marks.push((vp, other)),
            }
        }
    }

    /// Applies graphics to the marks of the just-drawn primitive, starting at `first`.
    pub(super) fn restyle(
        &self,
        marks: &mut Vec<(Option<usize>, Mark)>,
        first: usize,
        p: &ViewPrim,
        e: Option<ElemPaint>,
        wt: &Weights,
    ) {
        let over = e.map(|e| e.over).unwrap_or_default();
        let thin = self.paint.thin_lines;
        let styled = prim_class(p).is_some_and(|(c, _)| self.look.styles.contains_key(c));
        if over.is_empty() && !thin && !styled {
            return;
        }
        self.apply_look(marks, first, p, over.line.is_some(), thin, wt);
        let marks = &mut marks[first..];
        let line = over.line.map(rgb_of);
        let fill = over.fill.map(|c| over_paper(c, over.transparency));
        let width = over.line_weight.map(|w| f64::from(w) * PX_MM);
        for (_, m) in marks.iter_mut() {
            match m {
                Mark::Line {
                    weight, rgb, layer, ..
                } => {
                    if line.is_some() {
                        *rgb = line;
                    }
                    if let (Some(w), true) = (width, *layer != HATCH) {
                        *weight = w;
                    }
                    if thin {
                        *weight = weight.min(wt.thin);
                    }
                }
                Mark::Fill { rgb, .. } => {
                    if fill.is_some() {
                        *rgb = fill;
                    }
                }
                Mark::Text { .. } => {}
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ddc_model::drawq::{HatchFamily, HatchPattern, LineStyle, StyleOverride};

    #[test]
    fn prims_map_to_categories_and_bands() {
        let cut = ViewPrim::Cut {
            wall: 1,
            layer: 0,
            material: String::new(),
            ring: vec![],
        };
        assert_eq!(prim_class(&cut), Some(("wall", Band::Cut)));
        let leaf = ViewPrim::DoorLeaf {
            opening: 1,
            a: (0.0, 0.0),
            b: (1.0, 0.0),
        };
        assert_eq!(prim_class(&leaf), Some(("door", Band::Seen)));
        let win = ViewPrim::WindowLine {
            opening: 1,
            a: (0.0, 0.0),
            b: (1.0, 0.0),
        };
        assert_eq!(prim_class(&win), Some(("window", Band::Seen)));
        assert_eq!(category_of("slab"), Some("slab"));
        assert_eq!(category_of("nonsense"), None);
    }

    fn styled(ov: StyleOverride) -> ViewStyle {
        let mut look = DrawLook::default();
        look.styles.insert("wall".into(), ov);
        ViewStyle {
            paint: Arc::new(ViewPaint::default()),
            hatches: HashMap::new(),
            look,
            denom: 100.0,
        }
    }

    fn wall_cut() -> ViewPrim {
        ViewPrim::Cut {
            wall: 1,
            layer: 0,
            material: String::new(),
            ring: vec![],
        }
    }

    fn weights() -> Weights {
        crate::sheets::weights("iso", 100.0).unwrap()
    }

    fn line40() -> Vec<(Option<usize>, Mark)> {
        vec![(
            None,
            Mark::Line {
                pts: vec![[0.0, 0.0], [40.0, 0.0]],
                closed: false,
                weight: 0.1,
                rgb: None,
                layer: crate::sheets::CUT,
            },
        )]
    }

    #[test]
    fn cut_style_sets_weight_and_splits_a_dashed_line() {
        let st = styled(StyleOverride {
            cut: Some(LineStyle::new("bold", "dash")),
            ..Default::default()
        });
        let mut marks = line40();
        st.restyle(&mut marks, 0, &wall_cut(), None, &weights());
        assert!(marks.len() > 1, "dashes are separate pieces");
        let want = f64::from(st.look.line("wall", Band::Cut, 100).mm);
        for (_, m) in &marks {
            let Mark::Line { weight, .. } = m else {
                panic!("only lines")
            };
            assert!((weight - want).abs() < 1e-9);
        }
    }

    #[test]
    fn halftone_lightens_the_ink_and_other_categories_stay() {
        let st = styled(StyleOverride {
            halftone: true,
            ..Default::default()
        });
        let mut marks = line40();
        st.restyle(&mut marks, 0, &wall_cut(), None, &weights());
        let Mark::Line { rgb, .. } = &marks[0].1 else {
            panic!()
        };
        let g = rgb.expect("lightened")[0];
        assert!(g > 100 && g < 200, "{g}");
        // a slab has no override: its marks are untouched
        let slab = ViewPrim::Edge {
            table: "slab",
            element: 1,
            class: ddc_model::LineClass::Projection,
            a: (0.0, 0.0),
            b: (1.0, 0.0),
        };
        let mut marks = line40();
        st.restyle(&mut marks, 0, &slab, None, &weights());
        assert_eq!(marks.len(), 1);
        assert!(matches!(&marks[0].1, Mark::Line { rgb: None, weight, .. } if *weight == 0.1));
    }

    #[test]
    fn marks_before_the_primitive_are_left_alone() {
        let st = styled(StyleOverride {
            halftone: true,
            ..Default::default()
        });
        let mut marks = line40();
        marks.extend(line40());
        st.restyle(&mut marks, 1, &wall_cut(), None, &weights());
        assert!(matches!(&marks[0].1, Mark::Line { rgb: None, .. }));
        assert!(matches!(&marks[1].1, Mark::Line { rgb: Some(_), .. }));
    }

    #[test]
    fn library_hatch_fills_a_region_at_its_spacing() {
        let mut st = styled(StyleOverride::default());
        let mut p = HatchPattern {
            name: "wide".into(),
            families: vec![HatchFamily {
                angle: 0.0,
                origin: (0.0, 0.0),
                shift: 0.0,
                spacing: 5.0,
                dashes: vec![],
            }],
            model: false,
        };
        assert!(st.look.hatches.set(p.clone()));
        let ring = vec![vec![[0.0, 0.0], [50.0, 0.0], [50.0, 50.0], [0.0, 50.0]]];
        let mut out = vec![];
        st.lib_hatch(&mut Pen { out: &mut out, vp: None }, &ring, "wide", 0.1);
        let wide = out.len();
        assert!((8..=11).contains(&wide), "{wide}");
        p.families[0].spacing = 2.5;
        assert!(st.look.hatches.set(p));
        let mut out = vec![];
        st.lib_hatch(&mut Pen { out: &mut out, vp: None }, &ring, "wide", 0.1);
        assert!(out.len() > wide);
        let mut none = vec![];
        st.lib_hatch(&mut Pen { out: &mut none, vp: None }, &ring, "missing", 0.1);
        assert!(none.is_empty());
    }

    #[test]
    fn transparency_blends_with_the_paper() {
        assert_eq!(over_paper(0xFF0000, None), [255, 0, 0]);
        assert_eq!(over_paper(0x00FF00, Some(50)), [128, 255, 128]);
        assert_eq!(over_paper(0x123456, Some(100)), [255, 255, 255]);
    }
}
